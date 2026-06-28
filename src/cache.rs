//! Persistent on-disk cache of scan results.
//!
//! A full home scan is ~1.6M nodes and takes ~70 s (it's syscall-bound). The
//! resulting `FileTree` is serialized to a compact binary file (postcard,
//! varint-encoded) so the next launch can render the previous scan in well
//! under a second instead of re-walking the disk. The cache is keyed by the
//! scan root, versioned, and root-path-checked, so format changes and hash
//! collisions are ignored safely rather than mis-rendered.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::model::tree::FileTree;

/// Bump when the serialized shape of `FileTree`/`FileNode` changes — old caches
/// then fail the version check and are ignored (a fresh scan rewrites them).
const CACHE_VERSION: u32 = 2;

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").join("macdirstat"))
}

/// Stable cache file name for a scan root (hashed so any path maps to one file).
fn cache_file(root_path: &str) -> Option<PathBuf> {
    let mut h = rustc_hash::FxHasher::default();
    root_path.hash(&mut h);
    cache_dir().map(|d| d.join(format!("{:016x}.bin", h.finish())))
}

/// Serialize `tree` (and how long its scan took) to disk. Best-effort.
pub fn save(tree: &FileTree, scan_time_ms: f64) {
    let Some(path) = cache_file(&tree.root_path) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match postcard::to_allocvec(&(CACHE_VERSION, scan_time_ms, tree)) {
        Ok(bytes) => {
            let n = bytes.len();
            match std::fs::write(&path, bytes) {
                Ok(()) => log::info!("Cached {} ({:.1} MB)", tree.root_path, n as f64 / 1.0e6),
                Err(e) => log::warn!("Failed to write cache {:?}: {}", path, e),
            }
        }
        Err(e) => log::warn!("Failed to serialize cache: {}", e),
    }
}

/// Load a cached `(tree, scan_time_ms)` for `root_path`, if current.
pub fn load(root_path: &str) -> Option<(FileTree, f64)> {
    let path = cache_file(root_path)?;
    let bytes = std::fs::read(&path).ok()?;
    let (version, scan_time_ms, tree): (u32, f64, FileTree) = postcard::from_bytes(&bytes).ok()?;
    if version != CACHE_VERSION || tree.root_path != root_path {
        return None;
    }
    log::info!("Loaded cached scan of {} from {:?}", root_path, path);
    Some((tree, scan_time_ms))
}
