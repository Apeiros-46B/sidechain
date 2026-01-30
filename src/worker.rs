use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{ensure, Context, Result};

use crate::util::{has_extension, map_src_to_dst};

pub type FileCache = HashMap<PathBuf, FileInfo>;
pub type OrphanCache = HashMap<String, Vec<FileInfo>>;

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub dst: PathBuf,
    pub hash: String,
    pub mtime: i64,
    pub size: u64,
    pub config: String,
}

#[derive(Debug, Clone)]
pub struct ProcessedFile {
    pub src: PathBuf,
    pub info: FileInfo,
    pub status: FileStatus,
}

#[derive(Debug, Clone)]
pub enum FileStatus {
    PassedThrough,
    Transcoded,
    Reclaimed,
    Skipped,
}

pub struct WorkerSettings<'a> {
    pub src_root: &'a Path,
    pub dst_root: &'a Path,
    pub allowed_exts: &'a [String],
    pub target_ext: &'a str,
    pub bitrate: u32,
    pub should_copy: bool,
    pub orphans: &'a OrphanCache,
    pub cache: &'a FileCache,
}

pub fn process_file(src: &Path, args: WorkerSettings) -> Result<ProcessedFile> {
    let do_transcode = has_extension(src, args.allowed_exts);

    // for change detection, when the user changes bitrate or format we should re-enc
    // we should also track passed-through files, so we never mix the two types
    let config = if do_transcode {
        format!("{}:{}", args.target_ext, args.bitrate)
    } else {
        "passthrough".to_string()
    };

    let meta = fs::metadata(src).context("failed to stat file")?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let size = meta.len();
    let dst = map_src_to_dst(
        src,
        args.src_root,
        args.dst_root,
        args.target_ext,
        do_transcode,
    )?;

    if let Some(hit) = args.cache.get(src) {
        if hit.config != config {
            // user changed bitrate or format, reprocess even if it's in the cache
            log::debug!(
                "config for file {} changed, reprocessing",
                hit.dst.display(),
            );
        } else if hit.dst != dst {
            // the source file was renamed, reprocess
            log::debug!(
                "file {} renamed to {}, reprocessing",
                hit.dst.display(),
                dst.display(),
            );
        } else if hit.mtime == mtime && hit.size == size && hit.dst.exists() {
            // cache hit, the config and file are unchanged
            // we only skip if EVERYTHING matches, including the dest path
            return Ok(ProcessedFile {
                src: src.to_path_buf(),
                info: FileInfo {
                    dst: dst,
                    hash: hit.hash.clone(),
                    mtime: hit.mtime,
                    size: hit.size,
                    config,
                },
                status: FileStatus::Skipped,
            });
        }

        if let Err(e) = fs::remove_file(&hit.dst) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "failed to remove stale file {}: {}",
                    hit.dst.display(),
                    e,
                );
            }
        }
    }

    let hash = compute_hash(src)?;
    if let Some(parent) = dst.parent() {
        // multiple workers may try to create the same directory
        // don't handle this error, let later file operations fail if needed
        // TODO: this might be bad for perf
        _ = fs::create_dir_all(parent);
    }

    // optimistic rename detection
    if let Some(candidates) = args.orphans.get(&hash) {
        for info in candidates {
            if !info.dst.exists() {
                continue;
            }

            // only reclaim if the config matches
            if info.config != config {
                continue;
            }

            if info.size != size {
                log::warn!(
                    "file {} and orphan {} have same hash but differing sizes",
                    src.display(),
                    info.dst.display(),
                );
                continue;
            }

            // remove target if it exists
            if dst.exists() {
                // don't handle this error, let the rename operation fail if needed
                _ = fs::remove_file(&dst);
            }

            // rely on the OS to serialize renames. failure implies the file was
            // already claimed by another worker or is invalid, in which case we
            // just fall back to a safe option (re-transcode or passthrough)
            if fs::rename(&info.dst, &dst).is_ok() {
                // no other worker got it, we successfully renamed the file
                return Ok(ProcessedFile {
                    src: src.to_path_buf(),
                    info: FileInfo {
                        dst,
                        hash,
                        mtime,
                        size,
                        config,
                    },
                    status: FileStatus::Reclaimed,
                });
            }
        }
    }

    // fallback to transcode or passthrough
    let status = if do_transcode {
        spawn_ffmpeg(src, &dst, args.bitrate)?;
        FileStatus::Transcoded
    } else {
        if dst.exists() {
            fs::remove_file(&dst)?;
        }
        if args.should_copy {
            fs::copy(src, &dst).context("failed to copy")?;
        } else {
            fs::hard_link(src, &dst).with_context(|| {
                format!(
                    "failed to hardlink {} -> {}. if source and destination are on different filesystems, or if your fs doesn't support hardlinks, use the --copy flag",
                    src.display(),
                    dst.display(),
                )
            })?;
        }
        FileStatus::PassedThrough
    };

    Ok(ProcessedFile {
        src: src.to_path_buf(),
        info: FileInfo {
            dst,
            hash,
            mtime,
            size,
            config,
        },
        status,
    })
}

fn compute_hash(path: &Path) -> Result<String> {
    // streaming hash so we don't use a ton of memory on large input files
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn spawn_ffmpeg(src: &Path, dst: &Path, bitrate: u32) -> Result<()> {
    if dst.exists() {
        fs::remove_file(dst)?;
    }
    #[rustfmt::skip]
    let status = Command::new("ffmpeg")
        // we are already running worker threads in parallel, each worker
        // thread shouldn't spawn even more threads
        .arg("-threads").arg("1")
        .arg("-v").arg("error")
        .arg("-i").arg(src)
        .arg("-b:a").arg(format!("{bitrate}k"))
        .arg("-vn")
        .arg(dst)
        .status()
        .context("ffmpeg invocation failed")?;
    ensure!(status.success(), "ffmpeg failed with status: {}", status);
    Ok(())
}
