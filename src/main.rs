mod db;
mod worker;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use argh::FromArgs;
use log::{log, Level};
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::{db::FileCache, worker::FileStatus};

/// Create a lossy mirror of your lossless music collection.
/// To force a full rebuild, delete the destination folder and database file.
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
    #[argh(option, short = 'a')]
    allowed_exts: Vec<String>,

    /// file extensions to ignore (can provide multiple)
    #[argh(option, short = 'x')]
    ignored_exts: Vec<String>,

    /// output format (file extension for ffmpeg)
    #[argh(option, short = 'f')]
    format: String,

    /// bitrate of output (in kbps)
    #[argh(option, short = 'b')]
    bitrate: u32,

    /// maximum number of threads to use (default NUM_CORES)
    #[argh(option, short = 't')]
    max_threads: Option<usize>,
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
        !args.allowed_exts.is_empty(),
        "at least one allowed extension must be provided (e.g. -a flac)",
    );
    ensure!(
        args.format.chars().all(char::is_alphanumeric),
        "invalid format '{}', must be alphanumeric", args.format,
    );

    init_thread_pool(args.max_threads)?;
    let (mut conn, cache) = init_db(&args.db_path)?;
    let files = find_src_files(&args.source)?;
    prune_orphans(&mut conn, &cache, &files)?;
    spawn_workers(
        &mut conn,
        files,
        cache,
        args,
    )?;

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

    log!(Level::Info, "using {threads} worker threads");

    Ok(())
}

fn init_db(db_path: &Path) -> Result<(Connection, FileCache)> {
    let conn = db::connect(db_path)?;
    db::init(&conn)?;

    let cache = db::load_cache(&conn)?;

    log!(Level::Info, "connected to database");

    Ok((conn, cache))
}

fn find_src_files(src_root: &Path) -> Result<Vec<PathBuf>> {
    log!(
        Level::Info,
        "scanning source directory {}",
        src_root.display()
    );

    let mut files = Vec::new();

    for entry in WalkDir::new(src_root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }

    // sort by descending size so later we process larger files first for efficiency
    files.sort_by_cached_key(|path| {
        std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
    });
    files.reverse();

    log!(Level::Info, "found {} files", files.len());

    Ok(files)
}

fn prune_orphans(
    conn: &mut Connection,
    cache: &FileCache,
    files: &Vec<PathBuf>,
) -> Result<()> {
    let active_set: HashSet<&PathBuf> = files.iter().collect();

    let orphans = cache.iter().filter_map(|(src, info)| {
        // side effect: delete files
        if !active_set.contains(src) {
            if info.dst.exists() {
                log!(Level::Info, "removing orphan {}", info.dst.display());
                if let Err(e) = std::fs::remove_file(&info.dst) {
                    log!(
                        Level::Error,
                        "failed to remove orphan {}: {e}",
                        info.dst.display(),
                    );
                }
            }
            Some(src)
        } else {
            None
        }
    });
    db::prune(conn, orphans)?;

    Ok(())
}

fn spawn_workers(
    conn: &mut Connection,
    files: Vec<PathBuf>,
    cache: FileCache,
    args: Args,
) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();

    rayon::spawn(move || {
        use rayon::prelude::*;

        files.into_par_iter().for_each_with(tx, |tx, src| {
            let raw_res = worker::process_file(
                &cache,
                &src,
                &args.source,
                &args.destination,
                &args.allowed_exts,
                &args.ignored_exts,
                &args.format,
                args.bitrate,
            );

            let msg = raw_res.map_err(|e| (src, e));
            if let Err(e) = tx.send(msg) {
                log!(Level::Error, "receiver hung up: {e}");
            }
        });
    });

    let stream = rx.into_iter().inspect(|res| match &res {
        Ok(file) => match file.status {
            FileStatus::Transcoded => {
                log!(Level::Info, "transcoded {}", file.src.display());
            }
            FileStatus::Hardlinked => {
                log!(Level::Info, "hardlinked {}", file.src.display());
            }
            FileStatus::Skipped => {
                log!(Level::Debug, "skipped {}", file.src.display());
            }
            FileStatus::Ignored => {
                log!(Level::Trace, "ignored {}", file.src.display());
            }
        },
        Err((src, e)) => {
            log!(Level::Error, "failed to process {}: {e}", src.display());
        }
    });
    db::ingest_results(conn, stream.flatten())?;

    Ok(())
}
