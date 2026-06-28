//! End-to-end tests for the native scanner: they build a real temporary
//! directory tree on disk, scan it with `FileTree::scan`, and assert on the
//! resulting tree. These lock in the behavior that the performance work must
//! preserve (counts, sizes, inode dedup, min-size filter, exclusion,
//! cancellation, path resolution).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use macdirstat::model::tree::{FileNode, FileTree};

/// Create a unique, empty temp directory for one test.
fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mds_test_{}_{}_{}", tag, std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, len: usize) {
    fs::write(path, vec![b'x'; len]).unwrap();
}

/// Convenience: scan with dedup on/off, no exclusions, no size floor.
fn scan(root: &Path, skip_duplicates: bool, min_file_size_bytes: u64) -> FileTree {
    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    FileTree::scan(
        root,
        &[],
        &progress,
        skip_duplicates,
        min_file_size_bytes,
        &cancel,
        0,
    )
}

#[test]
fn counts_sizes_and_extensions_are_correct() {
    let root = temp_dir("counts");
    // root/a.txt (10), root/b.txt (20), root/sub/c.log (30), root/sub/deep/d.txt (40)
    write_file(&root.join("a.txt"), 10);
    write_file(&root.join("b.txt"), 20);
    fs::create_dir_all(root.join("sub/deep")).unwrap();
    write_file(&root.join("sub/c.log"), 30);
    write_file(&root.join("sub/deep/d.txt"), 40);

    let tree = scan(&root, true, 0);

    assert_eq!(tree.root.file_count, 4, "4 files total");
    // dirs: root + sub + deep = 3
    assert_eq!(tree.root.dir_count, 3, "root + sub + deep");
    assert!(tree.root.is_dir);
    // TOTALSIZE is the logical size; our files sum to 100 bytes.
    assert_eq!(tree.root.size, 100, "10+20+30+40");

    // Extension histogram: txt = 3 files / 10+20+40 = 70, log = 1 file / 30.
    let txt = tree.extensions.iter().find(|s| &*s.ext == "txt");
    let log = tree.extensions.iter().find(|s| &*s.ext == "log");
    assert_eq!(txt.map(|s| (s.bytes, s.count)), Some((70, 3)));
    assert_eq!(log.map(|s| (s.bytes, s.count)), Some((30, 1)));

    // extensions are sorted by size descending.
    let sizes: Vec<u64> = tree.extensions.iter().map(|s| s.bytes).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted, "extensions sorted by size desc");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn children_sorted_by_size_descending() {
    let root = temp_dir("sort");
    write_file(&root.join("small.bin"), 5);
    write_file(&root.join("big.bin"), 5000);
    write_file(&root.join("medium.bin"), 500);

    let tree = scan(&root, true, 0);
    let sizes: Vec<u64> = tree.root.children.iter().map(|c| c.size).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted, "children sorted by size descending");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn min_file_size_filter_excludes_small_files() {
    let root = temp_dir("minsize");
    write_file(&root.join("tiny.dat"), 100);
    write_file(&root.join("big.dat"), 10_000);

    let full = scan(&root, true, 0);
    assert_eq!(full.root.file_count, 2);
    assert_eq!(full.root.size, 10_100);

    // Floor at 1000 bytes: only big.dat survives.
    let filtered = scan(&root, true, 1000);
    assert_eq!(filtered.root.file_count, 1, "tiny file filtered out");
    assert_eq!(filtered.root.size, 10_000);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn hardlink_deduplicated_only_when_enabled() {
    let root = temp_dir("hardlink");
    let original = root.join("original.dat");
    write_file(&original, 1234);
    // Two extra hard links to the same inode in the same dir.
    fs::hard_link(&original, root.join("link1.dat")).unwrap();
    fs::hard_link(&original, root.join("link2.dat")).unwrap();

    // Dedup OFF: all three directory entries counted.
    let no_dedup = scan(&root, false, 0);
    assert_eq!(no_dedup.root.file_count, 3, "3 entries without dedup");
    assert_eq!(no_dedup.root.size, 1234 * 3);

    // Dedup ON: same inode counted once.
    let dedup = scan(&root, true, 0);
    assert_eq!(
        dedup.root.file_count, 1,
        "hardlinks counted once with dedup"
    );
    assert_eq!(dedup.root.size, 1234);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn excluded_paths_are_skipped() {
    let root = temp_dir("excl");
    write_file(&root.join("keep.dat"), 100);
    fs::create_dir_all(root.join("skipme")).unwrap();
    write_file(&root.join("skipme/inside.dat"), 9999);

    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let excluded = vec![root.join("skipme")];
    let tree = FileTree::scan(&root, &excluded, &progress, true, 0, &cancel, 0);

    assert_eq!(tree.root.file_count, 1, "only keep.dat counted");
    assert_eq!(tree.root.size, 100);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn precancelled_scan_returns_promptly_without_counting() {
    let root = temp_dir("cancel");
    // A reasonably wide/deep tree so a non-cancelled scan would do real work.
    for d in 0..20 {
        let sub = root.join(format!("d{d}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..200 {
            write_file(&sub.join(format!("f{f}.dat")), 64);
        }
    }

    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(true)); // cancelled before we start
    let start = std::time::Instant::now();
    let tree = FileTree::scan(&root, &[], &progress, true, 0, &cancel, 0);
    let elapsed = start.elapsed();

    // The root's own direct files (none here) plus whatever was gathered before
    // the cancel check unwinds. With cancel pre-set, no subdirectories are
    // descended, so the tree is effectively just the root.
    assert!(
        tree.root.file_count < 4000,
        "pre-cancelled scan must not count the whole tree (got {})",
        tree.root.file_count
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "pre-cancelled scan must return promptly (took {elapsed:?})"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn custom_thread_count_matches_auto() {
    // Guards the custom-pool extension-drain: a custom rayon pool must drain its
    // OWN workers' thread-local extension maps (pool.broadcast), not the global
    // pool's. If that were wrong the extension table would silently collapse.
    let root = temp_dir("threads");
    for d in 0..8 {
        let sub = root.join(format!("d{d}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..50 {
            let ext = if f % 2 == 0 { "txt" } else { "bin" };
            write_file(&sub.join(format!("f{f}.{ext}")), 100 + f);
        }
    }

    let progress = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let auto = FileTree::scan(&root, &[], &progress, true, 0, &cancel, 0);

    let progress2 = Arc::new(AtomicU64::new(0));
    let cancel2 = Arc::new(AtomicBool::new(false));
    let pooled = FileTree::scan(&root, &[], &progress2, true, 0, &cancel2, 2);

    assert_eq!(auto.root.file_count, pooled.root.file_count);
    assert_eq!(auto.root.dir_count, pooled.root.dir_count);
    assert_eq!(auto.root.size, pooled.root.size);

    // Extension tables must be identical (this is what the pool.broadcast fix protects).
    let to_tuples = |t: &FileTree| -> Vec<(String, u64, u64)> {
        let mut v: Vec<(String, u64, u64)> = t
            .extensions
            .iter()
            .map(|s| (s.ext.to_string(), s.bytes, s.count))
            .collect();
        v.sort();
        v
    };
    let a = to_tuples(&auto);
    let b = to_tuples(&pooled);
    assert_eq!(
        a, b,
        "custom-pool scan must produce the same extension stats"
    );
    assert!(
        a.iter().any(|(e, ..)| e == "txt") && a.iter().any(|(e, ..)| e == "bin"),
        "extension stats must be non-empty under a custom thread pool"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn largest_directories_ranks_by_size() {
    let root = temp_dir("largest");
    // a/ holds 10000 + a/sub/ 5000 = 15000; b/ holds 100.
    fs::create_dir_all(root.join("a/sub")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    write_file(&root.join("a/big.dat"), 10_000);
    write_file(&root.join("a/sub/mid.dat"), 5_000);
    write_file(&root.join("b/small.dat"), 100);

    let tree = scan(&root, true, 0);
    let dirs = tree.largest_directories(10);

    assert!(dirs.len() >= 3, "a, a/sub, b");
    assert_eq!(&*dirs[0].name, "a");
    assert_eq!(dirs[0].size, 15_000);

    // Ranked strictly descending by size.
    let sizes: Vec<u64> = dirs.iter().map(|d| d.size).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable_by(|x, y| y.cmp(x));
    assert_eq!(sizes, sorted);

    // The recorded index path resolves back to the right node.
    let node = tree
        .root
        .resolve_path(&dirs[0].path)
        .expect("path resolves");
    assert_eq!(&*node.name, "a");
    assert!(node.is_dir);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn folders_by_extension_ranks_and_matches() {
    let root = temp_dir("extfilter");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    write_file(&root.join("a/big.raw"), 10_000);
    write_file(&root.join("a/small.txt"), 10);
    write_file(&root.join("b/mid.raw"), 3_000);
    write_file(&root.join("b/data"), 50); // extensionless, in subfolder b
    write_file(&root.join("noext"), 7); // extensionless, in root (not a subfolder)

    let tree = scan(&root, true, 0);

    // .raw lives in a (10000) and b (3000), ranked by bytes descending.
    let raw = tree.folders_by_extension("raw", 10);
    assert_eq!(raw.len(), 2);
    assert_eq!(&*raw[0].name, "a");
    assert_eq!((raw[0].ext_bytes, raw[0].ext_count), (10_000, 1));
    assert_eq!(&*raw[1].name, "b");
    assert_eq!(raw[1].ext_bytes, 3_000);

    // .txt only in a.
    let txt = tree.folders_by_extension("txt", 10);
    assert_eq!(txt.len(), 1);
    assert_eq!(&*txt[0].name, "a");
    assert_eq!(txt[0].ext_bytes, 10);

    // The (no ext) sentinel matches extensionless files; only b/data is in a
    // subfolder (root's `noext` lives in the root, which isn't listed).
    let none = tree.folders_by_extension("(no ext)", 10);
    assert_eq!(none.len(), 1);
    assert_eq!(&*none[0].name, "b");
    assert_eq!(none[0].ext_bytes, 50);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn cache_save_load_roundtrip() {
    let root = temp_dir("cache");
    write_file(&root.join("a.txt"), 10);
    fs::create_dir_all(root.join("sub")).unwrap();
    write_file(&root.join("sub/b.bin"), 5_000);
    let tree = scan(&root, true, 0);

    macdirstat::cache::save(&tree, 42.0);
    let (back, ms) = macdirstat::cache::load(&tree.root_path).expect("cache loads back");

    assert_eq!(ms, 42.0);
    assert_eq!(back.root_path, tree.root_path);
    assert_eq!(back.root.file_count, tree.root.file_count);
    assert_eq!(back.root.dir_count, tree.root.dir_count);
    assert_eq!(back.root.size, tree.root.size);
    assert_eq!(back.root.children.len(), tree.root.children.len());

    let orig: Vec<(String, u64, u64)> = tree
        .extensions
        .iter()
        .map(|s| (s.ext.to_string(), s.bytes, s.count))
        .collect();
    let loaded: Vec<(String, u64, u64)> = back
        .extensions
        .iter()
        .map(|s| (s.ext.to_string(), s.bytes, s.count))
        .collect();
    assert_eq!(orig, loaded, "extension stats survive the cache round-trip");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn filenode_is_compact() {
    // Memory footprint guard (mirrors the const assertion in tree.rs).
    assert_eq!(std::mem::size_of::<FileNode>(), 72);
}

#[test]
fn build_fs_path_and_resolve_path_roundtrip() {
    let root = temp_dir("paths");
    fs::create_dir_all(root.join("sub")).unwrap();
    write_file(&root.join("sub/file.txt"), 7);

    let tree = scan(&root, true, 0);

    // Find the index path to "sub" then to "file.txt".
    let sub_idx = tree
        .root
        .children
        .iter()
        .position(|c| &*c.name == "sub")
        .expect("sub present");
    let sub = &tree.root.children[sub_idx];
    let file_idx = sub
        .children
        .iter()
        .position(|c| &*c.name == "file.txt")
        .expect("file present");

    let path = vec![sub_idx, file_idx];
    let node = tree.root.resolve_path(&path).expect("resolve");
    assert_eq!(&*node.name, "file.txt");
    assert_eq!(node.size, 7);
    assert!(!node.is_dir);

    let fs_path = tree.build_fs_path(&path).expect("fs path");
    assert_eq!(fs_path, root.join("sub/file.txt"));

    fs::remove_dir_all(&root).ok();
}
