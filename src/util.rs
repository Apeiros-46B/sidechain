use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn has_extension(path: &Path, ext_list: &[String]) -> bool {
    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        ext_list.iter().any(|e| e.to_lowercase() == ext_lower)
    } else {
        false
    }
}

pub fn map_src_to_dst(
    src: &Path,
    src_root: &Path,
    dst_root: &Path,
    target_ext: &str,
    set_ext: bool,
) -> Result<PathBuf> {
    let rel_path = src.strip_prefix(src_root).context("src outside root")?;
    let mut dst = dst_root.join(rel_path);

    if set_ext {
        dst.set_extension(target_ext);
    }

    Ok(dst)
}
