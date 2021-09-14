use anyhow::{anyhow, Result};
use colored::*;
use dunce::canonicalize;
use git2::{Commit, ObjectType, Oid, Repository, Tree};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
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
    #[structopt(
        short = "c",
        long,
        help = "Disable color coding for the output, default is to use colors in terminal"
    )]
    no_color_code: bool,
    #[structopt(
        short = "g",
        long,
        help = "Disable output grouping. Better for machine inputs"
    )]
    no_output_grouping: bool,
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
    color_code: bool,
    output_grouping: bool,
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
            color_code: !src.no_color_code,
            output_grouping: !src.no_output_grouping,
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

struct ProcessTree<'a> {
    settings: &'a Settings,
    repo: &'a Repository,
    checked_paths: HashSet<PathBuf>,
    checked_blobs: HashSet<Oid>,
    checked_trees: HashSet<Oid>,
    walked: usize,
    skipped_blobs: usize,
    all_matches: Vec<MatchEntry>,
}

impl<'a> ProcessTree<'a> {
    fn process(&mut self, tree: &Tree, commit: &Commit, path: &Path, visited: &mut bool) {
        if self.checked_trees.contains(&tree.id()) {
            return;
        }
        self.checked_trees.insert(tree.id());
        self.walked += 1;

        for entry in tree {
            match (|| {
                let name = entry.name()?;
                let entry_path = path.join(name);

                // We want to match with absolute path from root, but it seems impossible with `tree.walk`.
                if self.settings.once_file && self.checked_paths.contains(&entry_path) {
                    return None;
                }
                self.checked_paths.insert(entry_path.clone());

                let obj = match entry.to_object(&self.repo) {
                    Ok(obj) => obj,
                    Err(e) => {
                        eprintln!("couldn't get_object: {:?}", e);
                        return None;
                    }
                };
                if obj.kind() == Some(ObjectType::Tree) {
                    self.process(obj.as_tree()?, commit, &entry_path, visited);
                    return None;
                }
                if entry.kind() != Some(ObjectType::Blob)
                    || self.settings.ignore_dirs.contains(&OsString::from(name))
                {
                    return None;
                }

                let blob = obj.peel_to_blob().ok()?;
                if blob.is_binary() {
                    return None;
                }
                let ext = PathBuf::from(name).extension()?.to_owned();
                if !self.settings.extensions.contains(&ext.to_ascii_lowercase()) {
                    return None;
                }

                if self.checked_blobs.contains(&blob.id()) {
                    self.skipped_blobs += 1;
                    return None;
                }

                self.checked_blobs.insert(blob.id());
                let ret = process_file(self.settings, commit, blob.content(), &entry_path, visited);
                Some(ret)
            })() {
                Some(matches) => {
                    self.all_matches.extend(matches);
                }
                _ => (),
            }
        }
    }
}

fn process_files_git(_root: &Path, settings: &Settings) -> Result<Vec<MatchEntry>> {
    let repo = Repository::open(&settings.repo)?;
    let reference = if let Some(ref branch) = settings.branch {
        repo.resolve_reference_from_short_name(&branch)?
    } else {
        repo.head()?
    };

    let mut process_tree = ProcessTree {
        settings,
        repo: &repo,
        checked_paths: HashSet::new(),
        checked_blobs: HashSet::new(),
        checked_trees: HashSet::new(),
        walked: 0,
        skipped_blobs: 0,
        all_matches: vec![],
    };
    let mut checked_commits = HashMap::new();
    let mut iter = 0;

    let mut next_refs = vec![reference.peel_to_commit()?];
    loop {
        for commit in &next_refs {
            if checked_commits.contains_key(&commit.id()) {
                continue;
            }
            let entry = checked_commits.entry(commit.id()).or_insert(false);

            let tree = if let Ok(tree) = commit.tree() {
                tree
            } else {
                continue;
            };

            process_tree.process(&tree, commit, &PathBuf::from(""), entry);
        }
        next_refs = next_refs
            .iter()
            .map(|reference| reference.parent_ids())
            .flatten()
            .filter(|reference| !checked_commits.contains_key(reference))
            .map(|id| repo.find_commit(id))
            .collect::<std::result::Result<Vec<_>, git2::Error>>()?;

        if settings.verbose {
            eprintln!(
                "[{}] {} Matches in {} files {} skipped blobs... Next round has {} refs...",
                iter,
                process_tree.all_matches.len(),
                process_tree.walked,
                process_tree.skipped_blobs,
                next_refs.len()
            );
        }
        iter += 1;
        if next_refs.is_empty() {
            break;
        }
    }
    Ok(process_tree.all_matches)
}

fn process_file(
    settings: &Settings,
    commit: &Commit,
    input: &[u8],
    filepath: &Path,
    visited: &mut bool,
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
            path: filepath.to_path_buf(),
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

        if settings.color_code {
            if settings.output_grouping && !*visited {
                println!("\ncommit {}:", commit.id().to_string().bright_blue());
                *visited = true;
            }
            let line = format!(
                "{} {} {}",
                filepath.to_string_lossy().green(),
                &format!("({}):", line_number).bright_yellow(),
                &input_str[line_start..line_end]
            );
            if !settings.output_grouping {
                println!("{} {}", commit.id().to_string().bright_blue(), line);
            } else {
                println!("  {}", line);
            }
        } else {
            if settings.output_grouping && !*visited {
                println!("\ncommit {}:", commit.id());
                *visited = true;
            }
            let line = format!(
                "{}({}): {}",
                filepath.to_string_lossy(),
                line_number,
                &input_str[line_start..line_end]
            );
            if !settings.output_grouping {
                println!("{} {}", commit.id(), line);
            } else {
                println!("  {}", line);
            }
        }
    }

    ret
}
