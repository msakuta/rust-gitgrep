use anyhow::{anyhow, Result};
use dunce::canonicalize;
use git2::{Commit, Oid, Repository, TreeWalkResult};
use regex::Regex;
use std::{
    collections::HashSet,
    convert::{TryFrom, TryInto},
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(help = "The pattern to search for")]
    pattern: String,
    #[structopt(help = "Root repo to grep")]
    repo: Option<PathBuf>,
    #[structopt(short, long, help = "Branch name")]
    branch: Option<String>,
    #[structopt(
        short = "o",
        long,
        help = "Turn off showing matches to a file only once; the default behavior is that if the same file with the same name has different versions that matches, they will not be printed."
    )]
    no_once_file: bool,
    #[structopt(short, long, help = "Verbose flag")]
    verbose: bool,
    #[structopt(short, long, help = "Add an entry to list of extensions to search")]
    extensions: Vec<String>,
    #[structopt(
        short,
        long,
        help = "Add an entry to list of directory names to ignore"
    )]
    ignore_dirs: Vec<String>,
}

fn main() -> Result<()> {
    let settings: Settings = Opt::from_args().try_into()?;

    eprintln!(
        "Searching path: {:?} extensions: {:?} ignore_dirs: {:?}",
        settings.repo, settings.extensions, settings.ignore_dirs
    );

    let _file_list = process_files_git(&settings.repo, &settings)?;

    Ok(())
}

#[allow(dead_code)]
struct MatchEntry {
    commit: Oid,
    path: PathBuf,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct Settings {
    pattern: Regex,
    repo: PathBuf,
    branch: Option<String>,
    once_file: bool,
    verbose: bool,
    extensions: HashSet<OsString>,
    ignore_dirs: HashSet<OsString>,
}

// It's a bit awkward to convert from Opt to Settings, but some settings are hard to write
// conversion code inside structopt annotations.
impl TryFrom<Opt> for Settings {
    type Error = anyhow::Error;

    fn try_from(src: Opt) -> std::result::Result<Self, Self::Error> {
        let default_exts = [
            ".sh", ".js", ".tcl", ".pl", ".py", ".rb", ".c", ".cpp", ".h", ".rc", ".rci", ".dlg",
            ".pas", ".dpr", ".cs", ".rs",
        ];
        let default_ignore_dirs = [".hg", ".svn", ".git", ".bzr", "node_modules", "target"]; // Probably we could ignore all directories beginning with a dot.

        Ok(Self {
            pattern: Regex::new(&src.pattern)
                .map_err(|e| anyhow!("Error in regex compilation: {:?}", e))?,
            repo: canonicalize(
                src.repo.unwrap_or_else(|| {
                    PathBuf::from(env::current_dir().unwrap().to_str().unwrap())
                }),
            )
            .expect("Canonicalized path"),
            branch: src.branch,
            once_file: !src.no_once_file,
            verbose: src.verbose,
            extensions: if src.extensions.is_empty() {
                default_exts.iter().map(|ext| ext[1..].into()).collect()
            } else {
                default_exts
                    .iter()
                    .map(|ext| ext[1..].into())
                    .chain(src.extensions.iter().map(|ext| ext[1..].into()))
                    .collect()
            },
            ignore_dirs: if src.ignore_dirs.is_empty() {
                default_ignore_dirs.iter().map(|ext| ext.into()).collect()
            } else {
                default_ignore_dirs
                    .iter()
                    .map(|ext| ext.into())
                    .chain(src.ignore_dirs.iter().map(|ext| ext.into()))
                    .collect()
            },
        })
    }
}

fn process_files_git(_root: &Path, settings: &Settings) -> Result<Vec<MatchEntry>> {
    let mut walked = 0;
    let repo = Repository::open(&settings.repo)?;
    let mut skipped_blobs = 0;
    let mut all_matches = vec![];
    let reference = if let Some(ref branch) = settings.branch {
        repo.resolve_reference_from_short_name(&branch)?
    } else {
        repo.head()?
    };

    let mut checked_paths = HashSet::new();
    let mut checked_blobs = HashSet::new();
    let mut checked_commits = HashSet::new();
    let mut iter = 0;

    let mut next_refs = vec![reference.peel_to_commit()?];
    loop {
        for commit in &next_refs {
            let tree = if let Ok(tree) = commit.tree() {
                tree
            } else {
                continue;
            };
            if !checked_commits.contains(&commit.id()) {
                checked_commits.insert(commit.id());

                tree.walk(git2::TreeWalkMode::PostOrder, |_, entry| {
                    walked += 1;

                    match (|| {
                        let name = entry.name()?;
                        if entry.kind() != Some(git2::ObjectType::Blob)
                            || settings.ignore_dirs.contains(&OsString::from(name))
                        {
                            return None;
                        }

                        // We want to match with absolute path from root, but it seems impossible with `tree.walk`.
                        if settings.once_file && checked_paths.contains(name) {
                            return None;
                        }
                        checked_paths.insert(name.to_owned());

                        let obj = match entry.to_object(&repo) {
                            Ok(obj) => obj,
                            Err(e) => {
                                eprintln!("couldn't get_object: {:?}", e);
                                return None;
                            }
                        };
                        let blob = obj.peel_to_blob().ok()?;
                        if blob.is_binary() {
                            return None;
                        }
                        let path: PathBuf = entry.name()?.into();
                        let ext = path.extension()?.to_owned();
                        if !settings.extensions.contains(&ext.to_ascii_lowercase()) {
                            return None;
                        }

                        if checked_blobs.contains(&blob.id()) {
                            skipped_blobs += 1;
                            return None;
                        }

                        checked_blobs.insert(blob.id());

                        let ret = process_file(settings, commit, blob.content(), path);
                        Some(ret)
                    })() {
                        Some(matches) => {
                            all_matches.extend(matches);
                        }
                        _ => (),
                    }
                    TreeWalkResult::Ok
                })?;
            }
        }
        next_refs = next_refs
            .iter()
            .map(|reference| reference.parent_ids())
            .flatten()
            .filter(|reference| !checked_commits.contains(reference))
            .map(|id| repo.find_commit(id))
            .collect::<std::result::Result<Vec<_>, git2::Error>>()?;

        if settings.verbose {
            eprintln!(
                "[{}] {} Matches in {} files {} skipped blobs... Next round has {} refs...",
                iter,
                all_matches.len(),
                walked,
                skipped_blobs,
                next_refs.len()
            );
        }
        iter += 1;
        if next_refs.is_empty() {
            break;
        }
    }
    Ok(all_matches)
}

fn process_file(
    settings: &Settings,
    commit: &Commit,
    input: &[u8],
    filepath: PathBuf,
) -> Vec<MatchEntry> {
    let mut ret = vec![];

    // Non-utf8 files are not supported.
    let input_str = if let Ok(utf8) = std::str::from_utf8(&input) {
        utf8
    } else {
        return vec![];
    };

    for found in settings.pattern.find_iter(&input_str) {
        ret.push(MatchEntry {
            commit: commit.id(),
            path: filepath.clone(),
            start: found.start(),
            end: found.end(),
        });

        // Very naive way to count line numbers. Assumes newlines would not be part of multibyte
        // character, which is true for utf8 that is the only supported encoding in Rust anyway.
        let mut line_number = 1;
        let mut line_start = 0;
        let mut line_end = 0;
        for (i, c) in input.iter().enumerate() {
            if *c == b'\n' {
                line_number += 1;
                if i < found.start() {
                    line_start = (i + 1).min(input.len());
                }
                if found.end() <= i {
                    line_end = ((i as isize - 1) as usize).max(line_start);
                    break;
                }
            }
        }

        println!(
            "{} {}({}): {}",
            commit.id(),
            filepath.to_string_lossy(),
            line_number,
            &input_str[line_start..line_end]
        );
    }

    ret
}
