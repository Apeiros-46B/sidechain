use std::{
    fs,
    path::{Path, PathBuf}, process::Command,
};

use anyhow::{Context, Result, ensure};

use crate::db::FileCache;

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub dst: PathBuf,
    pub hash: String,
    pub mtime: i64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessedFile {
    pub src: PathBuf,
    pub info: FileInfo,
    pub status: FileStatus,
}

#[derive(Debug, Clone)]
pub enum FileStatus {
    Transcoded,
    Hardlinked,
    Skipped,
    Ignored,
}

pub fn process_file(
    cache: &FileCache,
    src: &Path,
    src_root: &Path,
    dst_root: &Path,
    allowed_exts: &[String],
    ignored_exts: &[String],
    target_ext: &str,
    bitrate: u32,
) -> Result<ProcessedFile> {
    let meta = fs::metadata(src).context("failed to stat file")?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let size = meta.len();

    if has_extension(src, ignored_exts) {
        return Ok({
            let src = src.to_path_buf();
            ProcessedFile {
                src,
                info: FileInfo::default(),
                status: FileStatus::Ignored,
            }
        });
    }

    let rel_path = src.strip_prefix(src_root).context("src not inside root")?;
    let mut dst = dst_root.join(rel_path);

    let do_transcode = has_extension(src, allowed_exts);
    if do_transcode {
        dst.set_extension(target_ext);
    }

    // fast path
    if let Some(cached) = cache.get(src) {
        if cached.mtime == mtime && cached.size == size && cached.dst.exists() {
            return Ok({
                let src = src.to_path_buf();
                ProcessedFile {
                    src,
                    info: FileInfo {
                        dst: dst,
                        hash: cached.hash.clone(),
                        mtime: cached.mtime,
                        size: cached.size,
                    },
                    status: FileStatus::Skipped,
                }
            });
        }
    }

    // slow path
    // TODO: actually do something with this hash. we need to see when
    // files in the source directory are renamed, and accordingly directly
    // rename the files in the destination directory, instead of transcoding
    // them again. this might require a large refactor of the cache system
    let hash = compute_hash(src)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).context("failed to create dst dir")?;
    }

    let status = if do_transcode {
        spawn_ffmpeg(src, &dst, bitrate)?;
        FileStatus::Transcoded
    } else {
        if dst.exists() {
            fs::remove_file(&dst)?;
        }
        fs::hard_link(src, &dst).context("failed to hardlink")?;
        FileStatus::Hardlinked
    };

    Ok(ProcessedFile {
        src: src.to_path_buf(),
        info: FileInfo { dst, hash, mtime, size },
        status,
    })
}

fn has_extension(path: &Path, ext_list: &[String]) -> bool {
    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        ext_list.iter().any(|e| e.to_lowercase() == ext_lower)
    } else {
        false
    }
}

fn compute_hash(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let hash = blake3::hash(&bytes);
    Ok(hash.to_hex().to_string())
}

fn spawn_ffmpeg(src: &Path, dst: &Path, bitrate: u32) -> Result<()> {
    if dst.exists() {
        fs::remove_file(dst)?;
    }
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
