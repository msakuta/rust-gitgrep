use anyhow::{anyhow, Result};
use dunce::canonicalize;
use git2::{Repository, TreeWalkResult};
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
    #[structopt(help = "Branch name")]
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

struct FileEntry {
    name: PathBuf,
    lines: usize,
    size: u64,
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

fn process_files_git(_root: &Path, settings: &Settings) -> Result<Vec<FileEntry>> {
    let mut walked = 0;
    let repo = Repository::open(&settings.repo)?;
    let mut i = 0;
    let mut files = vec![];
    let reference = if let Some(ref branch) = settings.branch {
        repo.resolve_reference_from_short_name(&branch)?
    } else {
        repo.head()?
    };
    reference
        .peel_to_tree()?
        .walk(git2::TreeWalkMode::PostOrder, |_, entry| {
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
                walked += 1;
                if blob.is_binary() {
                    return None;
                }
                let path: PathBuf = entry.name()?.into();
                let ext = path.extension()?.to_owned();
                if !settings.extensions.contains(&ext.to_ascii_lowercase()) {
                    return None;
                }

                let filesize = blob.size() as u64;

                Some((
                    ext,
                    process_file(settings, blob.content(), path, i, filesize)?,
                ))
            })() {
                Some((ext, file_entry)) => {
                    files.push(file_entry);

                    i += 1;
                }
                _ => (),
            }
            TreeWalkResult::Ok
        })?;
    eprintln!("Listing {}/{} files...", files.len(), walked);
    Ok(files)
}

fn process_file(
    settings: &Settings,
    input: &[u8],
    filepath: PathBuf,
    i: usize,
    filesize: u64,
) -> Option<FileEntry> {
    let mut linecount = 0;
    let mut linepos = 0;
    let reader = BufReader::new(input).lines();
    for line in reader {
        let line_str = line.ok()?;
        linecount += 1;
        linepos += line_str.len();
        for found in settings
            .pattern
            .find_iter(&String::from_utf8_lossy(line_str.as_bytes()))
        {
            // if found.start() < linepos {
            println!(
                "{}({}): {}",
                filepath.to_string_lossy(),
                linecount,
                line_str
            );
            break;
            // }
        }
    }

    Some(FileEntry {
        name: filepath,
        lines: 0,
        size: filesize,
    })
}
