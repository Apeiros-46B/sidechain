mod db;
mod util;
mod worker;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};

use anyhow::{ensure, Context, Result};
use argh::FromArgs;
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::{
    util::{has_extension, map_src_to_dst},
    worker::{FileCache, FileStatus, OrphanCache, WorkerSettings},
};

/**
Creates a lossy mirror of your lossless music collection.
- To force a full rebuild, delete the destination directory and database file.
- Symlinks in the input directory will be ignored.
- All files that are not transcoded or ignored will be passed through (hardlinked or copied, depending on the --copy flag)
- Non-UTF8 file names or paths are not supported.
- Unexpected behaviour will occur on certain filesystems if your source folder contains name collisions in different cases (e.g. Song.flac vs song.flac). This scenario is NOT SUPPORTED.
 */
#[derive(FromArgs, Debug, Clone)]
struct Args {
    /// the source directory to sync from
    #[argh(option, short = 'i')]
    source: PathBuf,

    /// the destination directory to sync to
    #[argh(option, short = 'o')]
    destination: PathBuf,

    /// path to SQLite database (created if missing)
    #[argh(option, short = 'd')]
    db_path: PathBuf,

    /// file extensions to transcode (can provide multiple)
    #[argh(option, short = 'a', long = "allowed")]
    allowed_exts: Vec<String>,

    /// file extensions to ignore (can provide multiple)
    #[argh(option, short = 'x', long = "ignored")]
    ignored_exts: Vec<String>,

    /// transcoded output format (file extension for ffmpeg)
    #[argh(option, short = 'f')]
    format: String,

    /// bitrate of transcoded output files (in kbps)
    #[argh(option, short = 'b')]
    bitrate: u32,

    /// maximum number of threads to use (default=max(CORES - 1, 1))
    #[argh(option, short = 't')]
    max_threads: Option<usize>,

    /// copy passed-through files instead of hardlinking. turn this on
    /// if the filesystem your destination directory is on doesn't support
    /// hardlinks (e.g. FAT32), or if your source and destination folders
    /// are on different filesystems
    #[argh(switch, short = 'c')]
    copy: bool,
}

fn main() -> Result<()> {
    env_logger::init();

    let args: Args = argh::from_env();
    ensure!(
        args.source.is_dir(),
        "--source argument must be a directory",
    );
    ensure!(
        args.destination.is_dir(),
        "--destination argument must be a directory",
    );
    ensure!(
        args.source != args.destination,
        "--source and --destination must be different directories",
    );
    ensure!(
        !args.allowed_exts.is_empty(),
        "at least one allowed extension must be provided (e.g. -a flac)",
    );
    ensure!(
        args.format.chars().all(char::is_alphanumeric),
        "invalid format '{}', must be alphanumeric",
        args.format,
    );

    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .context("ffmpeg not executable")?;

    let time = Instant::now();

    init_thread_pool(args.max_threads)?;

    let (mut conn, cache) = init_db(&args.db_path)?;

    let dest_canon = fs::canonicalize(&args.destination)
        .context("failed to canonicalize destination path")?;
    let db_path_canon = fs::canonicalize(&args.db_path)
        .context("failed to canonicalize database path")?;
    ensure!(
        !db_path_canon.starts_with(&dest_canon),
        "database file cannot be located inside the destination directory",
    );

    let files = find_src_files(&args, &db_path_canon)?;
    let (orphans, to_prune) = find_orphans(&cache, &files);

    let orphans = Arc::new(orphans);
    let dst_root = args.destination.clone(); // clone for later use cus we move args
    let stats = spawn_workers(&mut conn, files, orphans.clone(), cache, args)?;

    // cleanup
    for candidates in orphans.values() {
        for info in candidates {
            // if it still exists, no worker claimed it; it is safe to delete
            if info.dst.exists() {
                log::info!("removing orphan {}", info.dst.display());
                _ = std::fs::remove_file(&info.dst);
            }
        }
    }
    db::prune(&mut conn, to_prune.iter())?;
    remove_empty_dirs(&dst_root)?;

    let duration = Instant::now() - time;

    log::info!("operation took {:.2} seconds", duration.as_secs_f32());
    log::info!(
        "processed {}/{} files successfully ({} cached)",
        stats.successes,
        stats.successes + stats.fails,
        stats.skips,
    );

    Ok(())
}

fn init_thread_pool(threads: Option<usize>) -> Result<()> {
    let threads = threads
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .saturating_sub(1)
        })
        .max(1);

    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .context("failed to build thread pool")?;

    log::info!("using {threads} worker threads");

    Ok(())
}

fn init_db(db_path: &Path) -> Result<(Connection, FileCache)> {
    let conn = db::connect(db_path)?;
    db::init(&conn)?;

    let cache = db::load_cache(&conn)?;

    log::info!("connected to database");

    Ok((conn, cache))
}

// db_path_canon should be canonicalized
fn find_src_files(args: &Args, db_path_canon: &Path) -> Result<Vec<PathBuf>> {
    log::info!("scanning source directory {}", args.source.display());

    // path and size, for sorting
    let mut files = Vec::<(PathBuf, u64)>::new();

    // track allocated destinations to detect collisions (dst -> src)
    let mut dst_map = HashMap::<PathBuf, PathBuf>::new();

    for entry in WalkDir::new(&args.source) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            log::trace!(
                "skipping {}; not a normal file",
                entry.path().to_string_lossy()
            );
            continue;
        }

        let path = entry.path();

        if path.file_name() == db_path_canon.file_name() {
            // only canonicalize if names match (reduce number of syscalls)
            // don't include db in indexed files if it is in the same dir
            if let Ok(entry_canon) = fs::canonicalize(entry.path()) {
                if entry_canon == db_path_canon {
                    continue;
                }
            }
        }

        // don't push ignored files to the list, we don't need them later
        // ignored files don't produce output, no collision is possible
        if has_extension(path, &args.ignored_exts) {
            continue;
        }

        // collision detection
        let dst = map_src_to_dst(
            path,
            &args.source,
            &args.destination,
            &args.format,
            has_extension(path, &args.allowed_exts),
        )?;
        if let Some(existing_src) = dst_map.get(&dst) {
            log::warn!(
                "collision detected: '{}' and '{}' both map to '{}', skipping '{}'",
                existing_src.display(),
                path.display(),
                dst.display(),
                path.display(),
            );
            continue;
        }

        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        dst_map.insert(dst, path.to_path_buf());
        files.push((entry.into_path(), size));
    }

    // sort by descending size so later we process larger files first for efficiency
    files.sort_by_cached_key(|(_, size)| *size);
    files.reverse();

    log::info!("found {} files", files.len());

    Ok(files.into_iter().map(|(path, _)| path).collect())
}

// second return is a list of orphans for db pruning
fn find_orphans(cache: &FileCache, files: &[PathBuf]) -> (OrphanCache, Vec<PathBuf>) {
    let active_set: HashSet<&PathBuf> = files.iter().collect();
    let mut map: OrphanCache = HashMap::new();
    let mut to_prune = Vec::new();

    for (src, info) in cache {
        if !active_set.contains(src) {
            // missing from src
            map.entry(info.hash.clone()).or_default().push(info.clone());
            to_prune.push(src.clone());
        }
    }

    (map, to_prune)
}

#[derive(Default)]
struct WorkStats {
    successes: usize,
    skips: usize,
    fails: usize,
}

// returns number of succeeded and failed files
fn spawn_workers(
    conn: &mut Connection,
    files: Vec<PathBuf>,
    orphans: Arc<OrphanCache>,
    cache: FileCache,
    args: Args,
) -> Result<WorkStats> {
    let (tx, rx) = std::sync::mpsc::channel();

    rayon::spawn(move || {
        use rayon::prelude::*;

        files.into_par_iter().for_each_with(tx, |tx, src| {
            let settings = WorkerSettings {
                src_root: &args.source,
                dst_root: &args.destination,
                allowed_exts: &args.allowed_exts,
                target_ext: &args.format,
                bitrate: args.bitrate,
                should_copy: args.copy,
                orphans: &orphans,
                cache: &cache,
            };
            let raw_res = worker::process_file(&src, settings);
            _ = tx.send(raw_res.map_err(|e| (src, e)));
        });
    });

    let mut stats = WorkStats::default();

    let stream = rx.into_iter().inspect(|res| match &res {
        Ok(file) => match file.status {
            FileStatus::PassedThrough => {
                log::info!("passed through {}", file.src.display());
                stats.successes += 1;
            }
            FileStatus::Transcoded => {
                log::info!("transcoded {}", file.src.display());
                stats.successes += 1;
            }
            FileStatus::Reclaimed => {
                log::info!("reclaimed {}", file.src.display());
                stats.successes += 1;
            }
            FileStatus::Skipped => {
                log::trace!("skipped {}", file.src.display());
                stats.skips += 1;
            }
        },
        Err((src, e)) => {
            log::error!("failed to process {}: {e}", src.display());
            stats.fails += 1;
        }
    });
    db::ingest_results(conn, stream.flatten())?;

    Ok(stats)
}

fn remove_empty_dirs(root: &Path) -> Result<()> {
    // traverse leaf to root to delete nested empty dirs
    for entry in WalkDir::new(root).contents_first(true) {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        // protect the root dir
        if entry.path() == root {
            continue;
        }
        // attempt to remove; if it fails because the empty is not empty, just ignore
        let Err(e) = fs::remove_dir(entry.path()) else {
            continue;
        };
        if e.kind() != std::io::ErrorKind::DirectoryNotEmpty {
            log::warn!("failed to remove dir {}: {}", entry.path().display(), e);
        }
    }
    Ok(())
}
