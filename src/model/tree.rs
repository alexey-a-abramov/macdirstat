use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::scan::getattrlistbulk::{self, DirEntry};

/// Index path from root to a node in the tree (e.g. [2, 0, 1] = root's 3rd child → 1st child → 2nd child).
pub type TreePath = Vec<usize>;

thread_local! {
    static LOCAL_EXT_MAP: RefCell<FxHashMap<Box<str>, (u64, u64)>> = RefCell::new(FxHashMap::default());
}

fn raw_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext,
        _ => "",
    }
}

/// Treemap rectangle stored on every node. Four `f32`s (16 bytes) instead of
/// `treemap::Rect`'s four `f64`s (32 bytes): pixel coordinates never need f64
/// precision, and egui itself works in f32. The cushion-shading math converts
/// to f64 only where it needs the extra range.
#[derive(Clone, Copy, Default)]
pub struct NodeRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A node in the file tree. Compact by design — 72 bytes/struct.
/// `Box<str>`/`Box<[T]>` (not String/Vec), an f32 `NodeRect`, and u32 counts
/// keep per-node memory low: a full-disk scan can hold several million of these.
#[derive(Serialize, Deserialize)]
pub struct FileNode {
    pub name: Box<str>,
    pub children: Box<[FileNode]>,
    pub size: u64,
    /// Treemap rectangle, set during layout. Transient — not cached.
    #[serde(skip)]
    pub rect: NodeRect,
    /// Cached file count (1 for files, sum of children for dirs).
    pub file_count: u32,
    /// Cached directory count (0 for files, 1 + sum of children for dirs).
    pub dir_count: u32,
    pub is_dir: bool,
}

// Compile-time guard against accidental growth of the per-node footprint.
const _: () = assert!(
    std::mem::size_of::<FileNode>() == 72,
    "FileNode size changed — update this assertion and the doc comment above",
);

impl FileNode {
    /// Get the file extension, or empty string for dirs/extensionless files.
    pub fn extension(&self) -> &str {
        if self.is_dir {
            ""
        } else {
            raw_extension(&self.name)
        }
    }

    /// Walk a path of child indices to reach a descendant node.
    pub fn resolve_path(&self, path: &[usize]) -> Option<&FileNode> {
        let mut node = self;
        for &idx in path {
            node = node.children.get(idx)?;
        }
        Some(node)
    }

    /// Remove the child at `index` from this node's children, updating size and counts.
    /// Returns the removed child.
    pub fn remove_child(&mut self, index: usize) -> FileNode {
        let mut children = std::mem::take(&mut self.children).into_vec();
        let removed = children.remove(index);
        self.size = self.size.saturating_sub(removed.size);
        self.file_count = self.file_count.saturating_sub(removed.file_count);
        self.dir_count = self.dir_count.saturating_sub(removed.dir_count);
        self.children = children.into();
        removed
    }
}

/// Aggregated statistics for one file extension across the whole scan.
#[derive(Clone, Serialize, Deserialize)]
pub struct ExtStat {
    pub ext: Box<str>,
    pub bytes: u64,
    pub count: u64,
}

/// A directory summary used by the "Largest folders" view.
#[derive(Clone)]
pub struct DirSummary {
    pub path: TreePath,
    pub name: Box<str>,
    pub size: u64,
    pub file_count: u32,
    pub dir_count: u32,
}

/// A directory's total for one file extension, used by the "filter by type" view.
#[derive(Clone)]
pub struct ExtDirSummary {
    pub path: TreePath,
    pub name: Box<str>,
    pub ext_bytes: u64,
    pub ext_count: u64,
}

/// The complete scanned file tree with precomputed extension statistics.
#[derive(Serialize, Deserialize)]
pub struct FileTree {
    pub root: FileNode,
    pub root_path: String,
    /// Per-extension totals (bytes + file count), sorted by bytes descending.
    pub extensions: Vec<ExtStat>,
    /// Directories that could not be opened during the scan (usually because the
    /// app lacks Full Disk Access). Their contents are missing from the totals.
    pub permission_denied: u64,
    /// True when directory totals are lower bounds rather than exact: the scan
    /// skipped small files (optimization mode), skipped excluded/cloud folders,
    /// or hit unreadable directories. The UI marks affected sizes with a "~".
    pub size_approximate: bool,
}

/// Scan-wide options and mutable state shared across threads.
struct ScanCtx {
    skip_duplicates: bool,
    /// Files strictly smaller than this byte count are ignored (0 = disabled).
    min_file_size_bytes: u64,
    /// Set to true to abort the scan early; the partial tree built so far is kept.
    cancel: Arc<AtomicBool>,
    /// (devid, ino) of every directory we have opened so far.
    seen_dirs: Mutex<FxHashSet<(u32, u64)>>,
    /// (devid, fileid) of every regular file we have counted so far.
    seen_files: Mutex<FxHashSet<(u32, u64)>>,
    /// Count of directories that could not be opened (permission denied, etc.).
    denied: AtomicU64,
    /// Set once any directory was skipped because it matched the exclusion list.
    excluded_hit: AtomicBool,
}

impl ScanCtx {
    fn new(skip_duplicates: bool, min_file_size_bytes: u64, cancel: Arc<AtomicBool>) -> Self {
        Self {
            skip_duplicates,
            min_file_size_bytes,
            cancel,
            seen_dirs: Mutex::new(FxHashSet::default()),
            seen_files: Mutex::new(FxHashSet::default()),
            denied: AtomicU64::new(0),
            excluded_hit: AtomicBool::new(false),
        }
    }

    /// True once the user has requested the scan be stopped.
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Register a directory fd. Returns `false` if this (devid, ino) was already seen.
    fn try_register_dir(&self, fd: libc::c_int) -> bool {
        if !self.skip_duplicates || fd < 0 {
            return true;
        }
        let mut s: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut s) } != 0 {
            return true; // fstat failed — allow traversal
        }
        let key = (s.st_dev as u32, s.st_ino as u64);
        self.seen_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key)
    }

    /// Register a file inode. Returns `false` if this (devid, fileid) was already seen.
    fn try_register_file(&self, devid: u32, fileid: u64) -> bool {
        if !self.skip_duplicates || fileid == 0 {
            return true;
        }
        self.seen_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((devid, fileid))
    }
}

impl FileTree {
    /// Build a file tree by scanning the given path using getattrlistbulk.
    /// Directories whose absolute path exactly matches an entry in `excluded` are skipped.
    /// `progress` is incremented once per file discovered (for live UI updates).
    /// Setting `cancel` to true aborts traversal early; the partial tree built up
    /// to that point is still returned.
    pub fn scan(
        root: &Path,
        excluded: &[PathBuf],
        progress: &Arc<AtomicU64>,
        skip_duplicates: bool,
        min_file_size_bytes: u64,
        cancel: &Arc<AtomicBool>,
        scan_threads: usize,
    ) -> Self {
        let ext_map = Mutex::new(FxHashMap::<Box<str>, (u64, u64)>::default());
        let ctx = Arc::new(ScanCtx::new(
            skip_duplicates,
            min_file_size_bytes,
            Arc::clone(cancel),
        ));

        // Build the tree, then drain every thread's local extension map into the
        // shared `ext_map`. The drain must run on the SAME pool the scan used:
        // when a custom thread count is requested we install a dedicated pool and
        // broadcast on *it*, because a global `rayon::broadcast` would miss the
        // custom pool's worker-thread-local maps and silently lose the extension
        // statistics.
        let custom_pool = if scan_threads > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(scan_threads)
                .build()
                .ok()
        } else {
            None
        };

        let root_node = match &custom_pool {
            Some(pool) => pool.install(|| {
                let node = build_root_node(root, excluded, progress, &ctx);
                drain_local_ext(&ext_map); // this (calling) thread
                pool.broadcast(|_| drain_local_ext(&ext_map)); // custom pool workers
                node
            }),
            None => {
                let node = build_root_node(root, excluded, progress, &ctx);
                drain_local_ext(&ext_map); // this (calling) thread
                rayon::broadcast(|_| drain_local_ext(&ext_map)); // global pool workers
                node
            }
        };

        let mut extensions: Vec<ExtStat> = ext_map
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into_iter()
            .map(|(ext, (bytes, count))| ExtStat { ext, bytes, count })
            .collect();
        extensions.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));

        let permission_denied = ctx.denied.load(Ordering::Relaxed);
        // Sizes are lower bounds whenever the scan deliberately left content out:
        // a small-file floor, an excluded/cloud subtree, or an unreadable folder.
        let size_approximate = min_file_size_bytes > 0
            || ctx.excluded_hit.load(Ordering::Relaxed)
            || permission_denied > 0;

        FileTree {
            root: root_node,
            root_path: root.display().to_string(),
            extensions,
            permission_denied,
            size_approximate,
        }
    }

    /// Build the full filesystem path for a node identified by index path.
    pub fn build_fs_path(&self, path: &[usize]) -> Option<std::path::PathBuf> {
        let mut fs_path = std::path::PathBuf::from(&self.root_path);
        let mut node = &self.root;
        for &idx in path {
            let child = node.children.get(idx)?;
            fs_path.push(&*child.name);
            node = child;
        }
        Some(fs_path)
    }

    /// Remove the node at the given index path from the tree, updating all ancestor sizes/counts.
    /// Returns the removed node, or None if the path is invalid.
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<FileNode> {
        let (&child_idx, parent_path) = path.split_last()?;

        // Navigate to the parent
        let mut node = &mut self.root;
        for &idx in parent_path {
            node = node.children.get_mut(idx)?;
        }

        if child_idx >= node.children.len() {
            return None;
        }

        Some(node.remove_child(child_idx))
    }

    /// Propagate a size/count reduction up the ancestor chain (excluding the node itself).
    pub fn subtract_from_ancestors(
        &mut self,
        path: &[usize],
        size: u64,
        file_count: u32,
        dir_count: u32,
    ) {
        // The direct parent is already updated by remove_child; update grandparents and above.
        let mut node = &mut self.root;
        // Update root
        node.size = node.size.saturating_sub(size);
        node.file_count = node.file_count.saturating_sub(file_count);
        node.dir_count = node.dir_count.saturating_sub(dir_count);
        // Update intermediate ancestors (not the direct parent, which remove_child handles)
        if path.len() >= 2 {
            for &idx in &path[..path.len() - 2] {
                if let Some(child) = node.children.get_mut(idx) {
                    child.size = child.size.saturating_sub(size);
                    child.file_count = child.file_count.saturating_sub(file_count);
                    child.dir_count = child.dir_count.saturating_sub(dir_count);
                    node = child;
                } else {
                    break;
                }
            }
        }
    }

    /// Rebuild extension statistics from the current tree.
    pub fn rebuild_extensions(&mut self) {
        let mut ext_map: FxHashMap<Box<str>, (u64, u64)> = FxHashMap::default();
        collect_extensions(&self.root, &mut ext_map);
        let mut extensions: Vec<ExtStat> = ext_map
            .into_iter()
            .map(|(ext, (bytes, count))| ExtStat { ext, bytes, count })
            .collect();
        extensions.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
        self.extensions = extensions;
    }

    /// Flatten every directory below the root and return the `limit` largest by
    /// size (descending). Powers the "Largest folders" view.
    pub fn largest_directories(&self, limit: usize) -> Vec<DirSummary> {
        let mut dirs: Vec<DirSummary> = Vec::new();
        let mut path: Vec<usize> = Vec::new();
        collect_dirs(&self.root, &mut path, &mut dirs);
        dirs.sort_unstable_by_key(|d| std::cmp::Reverse(d.size));
        dirs.truncate(limit);
        dirs
    }

    /// Folders that contain files of `ext`, ranked by how many bytes of that
    /// extension live under each (descending). Pass the `(no ext)` sentinel to
    /// match extensionless files. Powers the "filter by file type" view.
    pub fn folders_by_extension(&self, ext: &str, limit: usize) -> Vec<ExtDirSummary> {
        let mut out: Vec<ExtDirSummary> = Vec::new();
        let mut path: Vec<usize> = Vec::new();
        collect_ext_dirs(&self.root, ext, &mut path, &mut out);
        out.sort_unstable_by_key(|d| std::cmp::Reverse(d.ext_bytes));
        out.truncate(limit);
        out
    }

    /// Incremental refresh: walk the existing tree, drop nodes that no longer
    /// exist on disk, update file sizes for those that do, and re-sort every
    /// directory. Much faster than a full rescan when most content is unchanged
    /// because it only does one `getattrlistbulk` call per directory rather than
    /// rebuilding the whole tree from scratch.
    ///
    /// New files or directories that appeared since the last scan are NOT added;
    /// use `FileTree::scan` for full discovery.
    pub fn refresh_exists(
        mut self,
        excluded: &[PathBuf],
        progress: &Arc<AtomicU64>,
        cancel: &Arc<AtomicBool>,
    ) -> Self {
        let root_path = std::path::Path::new(&self.root_path);
        let fd = getattrlistbulk::open_dir(root_path);
        if fd >= 0 {
            refresh_node_fd(&mut self.root, fd, root_path, excluded, progress, cancel);
            getattrlistbulk::close_dir(fd);
        }
        self.rebuild_extensions();
        self
    }
}

/// Drain this thread's local extension map into the shared global map.
fn drain_local_ext(ext_map: &Mutex<FxHashMap<Box<str>, (u64, u64)>>) {
    LOCAL_EXT_MAP.with(|m| {
        let local = m.replace(FxHashMap::default());
        if !local.is_empty() {
            let mut global = ext_map.lock().unwrap_or_else(|e| e.into_inner());
            for (k, (bytes, count)) in local {
                let e = global.entry(k).or_insert((0, 0));
                e.0 += bytes;
                e.1 += count;
            }
        }
    });
}

fn collect_extensions(node: &FileNode, map: &mut FxHashMap<Box<str>, (u64, u64)>) {
    if !node.is_dir {
        let key: Box<str> = {
            let ext = node.extension();
            if ext.is_empty() {
                "(no ext)".into()
            } else {
                ext.into()
            }
        };
        let e = map.entry(key).or_insert((0, 0));
        e.0 += node.size;
        e.1 += 1;
    }
    for child in node.children.iter() {
        collect_extensions(child, map);
    }
}

/// Recursively collect every directory (excluding the root) into `out`.
fn collect_dirs(node: &FileNode, path: &mut Vec<usize>, out: &mut Vec<DirSummary>) {
    for (i, child) in node.children.iter().enumerate() {
        if child.is_dir {
            path.push(i);
            out.push(DirSummary {
                path: path.clone(),
                name: child.name.clone(),
                size: child.size,
                file_count: child.file_count,
                dir_count: child.dir_count,
            });
            collect_dirs(child, path, out);
            path.pop();
        }
    }
}

/// True if `node` is a file whose extension is `ext` (`(no ext)` matches
/// extensionless files).
fn file_matches_ext(node: &FileNode, ext: &str) -> bool {
    if node.is_dir {
        return false;
    }
    let e = node.extension();
    if ext == "(no ext)" {
        e.is_empty()
    } else {
        e == ext
    }
}

/// Accumulate per-directory totals for `ext`. Returns this subtree's (bytes,
/// count) of matching files and records every directory that contains any.
fn collect_ext_dirs(
    node: &FileNode,
    ext: &str,
    path: &mut Vec<usize>,
    out: &mut Vec<ExtDirSummary>,
) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut count = 0u64;
    for (i, child) in node.children.iter().enumerate() {
        if child.is_dir {
            path.push(i);
            let (b, c) = collect_ext_dirs(child, ext, path, out);
            if b > 0 {
                out.push(ExtDirSummary {
                    path: path.clone(),
                    name: child.name.clone(),
                    ext_bytes: b,
                    ext_count: c,
                });
            }
            path.pop();
            bytes += b;
            count += c;
        } else if file_matches_ext(child, ext) {
            bytes += child.size;
            count += 1;
        }
    }
    (bytes, count)
}

/// Recursively refresh a directory node using an already-open fd.
///
/// - Entries present in the current tree but absent from the filesystem are dropped.
/// - File sizes are updated to the current on-disk value.
/// - Directory subtrees are refreshed recursively (each opening its child dirs with openat).
/// - Children are re-sorted by size descending after the update.
/// - When cancelled mid-traversal, unvisited children are kept unchanged so the
///   tree never loses data it hasn't had a chance to verify.
fn refresh_node_fd(
    node: &mut FileNode,
    node_fd: libc::c_int,
    node_path: &std::path::Path,
    excluded: &[PathBuf],
    progress: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) {
    if cancel.load(Ordering::Relaxed) || node.children.is_empty() {
        return;
    }

    // Read the directory's current contents via the already-open fd.
    let current_entries = getattrlistbulk::scan_dir_entries_fd(node_fd);

    // Build a fast name → entry lookup.
    let mut entry_map: FxHashMap<&str, &DirEntry> =
        FxHashMap::with_capacity_and_hasher(current_entries.len(), Default::default());
    for entry in &current_entries {
        entry_map.insert(&entry.name, entry);
    }

    let old_children = std::mem::take(&mut node.children).into_vec();
    let mut new_children: Vec<FileNode> = Vec::with_capacity(old_children.len());
    let mut cancelled = false;

    for mut child in old_children {
        if cancelled {
            // Scan was stopped — carry remaining children forward unchanged.
            new_children.push(child);
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            new_children.push(child);
            continue;
        }

        // Drop children whose name no longer appears or whose type changed.
        let Some(entry) = entry_map.get(&*child.name) else {
            continue;
        };
        if entry.is_dir != child.is_dir {
            continue; // file replaced by dir or vice-versa — treat as deleted
        }

        if child.is_dir {
            let child_path = node_path.join(&*child.name);
            if excluded.iter().any(|e| child_path == *e) {
                continue; // now excluded — drop it
            }
            let child_fd = getattrlistbulk::openat_dir(node_fd, &child.name);
            if child_fd >= 0 {
                refresh_node_fd(
                    &mut child,
                    child_fd,
                    &child_path,
                    excluded,
                    progress,
                    cancel,
                );
                getattrlistbulk::close_dir(child_fd);
            }
            // If we can't open the dir (permission denied) keep the child with
            // its last-known sizes rather than removing it.
        } else {
            child.size = entry.file_size;
            progress.fetch_add(1, Ordering::Relaxed);
        }

        new_children.push(child);
    }

    // Re-sort by size descending to maintain the tree invariant.
    new_children.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    // Recompute this node's aggregate stats from the (possibly pruned) children.
    let mut total_size = 0u64;
    let mut total_file_count = 0u32;
    let mut total_dir_count = 0u32;
    for child in &new_children {
        total_size += child.size;
        total_file_count = total_file_count.saturating_add(child.file_count);
        total_dir_count = total_dir_count.saturating_add(child.dir_count);
    }
    node.children = new_children.into();
    node.size = total_size;
    node.file_count = total_file_count;
    node.dir_count = total_dir_count.saturating_add(1);
}

fn build_root_node(
    path: &Path,
    excluded: &[PathBuf],
    progress: &Arc<AtomicU64>,
    ctx: &Arc<ScanCtx>,
) -> FileNode {
    let fd = getattrlistbulk::open_dir(path);
    if fd < 0 {
        ctx.denied.fetch_add(1, Ordering::Relaxed);
        log::warn!(
            "Could not open directory {:?} (permission denied or not found)",
            path
        );
    }
    ctx.try_register_dir(fd); // register root so firmlink twins are skipped
    let name: Box<str> = path.display().to_string().into();
    let abs_path = path.to_owned();
    let node = build_node_fd(fd, name, Some(abs_path), excluded, progress, ctx);
    getattrlistbulk::close_dir(fd);
    node
}

/// Build a FileNode from an already-opened directory fd.
/// `node_name` is the display name for this node.
/// `current_abs_path` is the absolute path of the directory, used to skip excluded children.
fn build_node_fd(
    parent_fd: libc::c_int,
    node_name: Box<str>,
    current_abs_path: Option<PathBuf>,
    excluded: &[PathBuf],
    progress: &Arc<AtomicU64>,
    ctx: &Arc<ScanCtx>,
) -> FileNode {
    use rayon::prelude::*;

    let entries = getattrlistbulk::scan_dir_entries_fd(parent_fd);

    // Separate files and directories. `entries` is consumed by value so each
    // entry's name (a heap `Box<str>`) is MOVED into its node rather than cloned.
    let mut file_nodes: Vec<FileNode> = Vec::new();
    let mut child_dirs: Vec<DirEntry> = Vec::new();
    let mut total_size: u64 = 0;
    let mut total_file_count: u32 = 0;

    for entry in entries {
        if entry.is_dir {
            // Skip directories whose absolute path is in the exclusion list
            if let Some(ref parent) = current_abs_path {
                let child_path = parent.join(&*entry.name);
                if excluded.iter().any(|excl| child_path == *excl) {
                    ctx.excluded_hit.store(true, Ordering::Relaxed);
                    continue;
                }
            }
            child_dirs.push(entry);
        } else {
            // Optimization mode: skip files below the configured threshold
            if ctx.min_file_size_bytes > 0 && entry.file_size < ctx.min_file_size_bytes {
                continue;
            }
            // Skip duplicate file inodes (e.g. hardlinks counted twice)
            if !ctx.try_register_file(entry.devid, entry.fileid) {
                continue;
            }
            total_size += entry.file_size;
            total_file_count = total_file_count.saturating_add(1);
            progress.fetch_add(1, Ordering::Relaxed);
            LOCAL_EXT_MAP.with(|m| {
                let mut map = m.borrow_mut();
                let ext = raw_extension(&entry.name);
                let key: Box<str> = if ext.is_empty() {
                    "(no ext)".into()
                } else {
                    ext.into()
                };
                let e = map.entry(key).or_insert((0, 0));
                e.0 += entry.file_size;
                e.1 += 1;
            });
            file_nodes.push(FileNode {
                name: entry.name, // moved, not cloned
                children: Box::new([]),
                size: entry.file_size,
                rect: NodeRect::default(),
                file_count: 1,
                dir_count: 0,
                is_dir: false,
            });
        }
    }

    // Recurse into subdirectories — use openat() relative to parent fd, consuming
    // each DirEntry so its name moves into the child node. Returns None for
    // directories we've already visited (firmlinks, bind-mounts) or once the scan
    // is cancelled, so the duplicate/remainder is dropped rather than shown.
    let build_child = |entry: DirEntry| -> Option<FileNode> {
        if ctx.is_cancelled() {
            return None;
        }
        let child_fd = getattrlistbulk::openat_dir(parent_fd, &entry.name);
        if child_fd < 0 {
            ctx.denied.fetch_add(1, Ordering::Relaxed);
        }
        if !ctx.try_register_dir(child_fd) {
            getattrlistbulk::close_dir(child_fd);
            return None;
        }
        let child_abs = current_abs_path.as_ref().map(|p| p.join(&*entry.name));
        let node = build_node_fd(child_fd, entry.name, child_abs, excluded, progress, ctx);
        getattrlistbulk::close_dir(child_fd);
        Some(node)
    };

    let dir_nodes: Vec<FileNode> = if child_dirs.len() >= 2 {
        child_dirs.into_par_iter().filter_map(build_child).collect()
    } else {
        child_dirs.into_iter().filter_map(build_child).collect()
    };

    let mut total_dir_count: u32 = 0;
    for child in &dir_nodes {
        total_size += child.size;
        total_file_count = total_file_count.saturating_add(child.file_count);
        total_dir_count = total_dir_count.saturating_add(child.dir_count);
    }

    let mut children: Vec<FileNode> = Vec::with_capacity(file_nodes.len() + dir_nodes.len());
    children.extend(file_nodes);
    children.extend(dir_nodes);

    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    FileNode {
        name: node_name,
        children: children.into(),
        size: total_size,
        rect: NodeRect::default(),
        file_count: total_file_count,
        dir_count: total_dir_count.saturating_add(1),
        is_dir: true,
    }
}
