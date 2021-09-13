use anyhow::{anyhow, Result};
use dunce::canonicalize;
use git2::{Commit, Oid, Repository, TreeWalkResult};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    env,
    ffi::OsString,
    fs::File,
    io::{BufRead, BufReader},
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

    let file_list = process_files_git(&settings.repo, &settings)?;

    Ok(())
}

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
            pattern: Regex::new(&src.pattern).map_err(|e| anyhow!("Error in regex compilation"))?,
            repo: canonicalize(
                src.repo.unwrap_or_else(|| {
                    PathBuf::from(env::current_dir().unwrap().to_str().unwrap())
                }),
            )
            .expect("Canonicalized path"),
            branch: src.branch,
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
    let mut i = 0;
    let mut all_matches = vec![];
    let reference = if let Some(ref branch) = settings.branch {
        repo.resolve_reference_from_short_name(&branch)?
    } else {
        repo.head()?
    };

    let mut checked_commits = HashSet::new();

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

                        let ret = process_file(settings, commit, blob.content(), path);
                        Some(ret)
                    })() {
                        Some(matches) => {
                            all_matches.extend(matches);

                            i += 1;
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
        eprintln!(
            "{} Matches in {} files... Next round has {} refs...",
            all_matches.len(),
            walked,
            next_refs.len()
        );
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
    let mut linecount = 0;
    let mut linepos = 0;
    let mut ret = vec![];
    let reader = BufReader::new(input).lines();
    // for line in reader {
    //     let line_str = if let Ok(line) = line {
    //         line
    //     } else {
    //         continue;
    //     };
    //     linecount += 1;
    //     linepos += line_str.len();
    for found in settings
        .pattern
        // .find_iter(&String::from_utf8_lossy(line_str.as_bytes()))
        .find_iter(&String::from_utf8_lossy(input))
    {
        ret.push(MatchEntry {
            commit: commit.id(),
            path: filepath.clone(),
            start: found.start(),
            end: found.end(),
        });
        println!(
            "{} {}({}): {}",
            commit.id(),
            filepath.to_string_lossy(),
            linecount,
            // line_str
            String::from_utf8_lossy(&input[found.range()])
        );
        // break;
        // }
    }
    // }

    ret
}
