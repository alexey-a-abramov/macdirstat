# macdirstat

macdirstat is a **macOS-only** disk-usage visualizer — a native WinDirStat/WizTree clone written in **Rust (2024 edition)**, built on **egui/eframe**. It scans disks fast by going below the standard filesystem APIs to the native `getattrlistbulk(2)` syscall (parallelized with rayon), then renders the results as a collapsible directory tree, an extension/type breakdown, and a squarified, cushion-shaded treemap. **License: GPL-3.0. Version: 0.5.0.**

It is macOS/Darwin-only by design: the scanner depends on `getattrlistbulk`, `openat`, and APFS/SSD assumptions, and will not compile or link off macOS.

---

## Quick start / commands

```bash
# Build
cargo build --release           # release binary (use this for any perf testing)
cargo build                     # debug

# Run
cargo run --release             # launches; opens a native folder picker at $HOME
cargo run --release -- /path    # scan a specific path passed as argv[1]

# Quality gates (standard cargo; run before committing non-trivial changes)
cargo fmt
cargo clippy
# Note: the repo currently ships NO tests — `cargo test` is a no-op until tests are added.
```

Notes:
- `Cargo.lock` and `benches/` are **gitignored** — there are no committed benchmark targets and no committed lockfile, so a fresh clone must resolve/build to materialize a lock. Edition 2024 requires a recent stable toolchain.
- `data/` and `logs/` at the repo root are local scratch/benchmark output, not part of the app.

---

## Architecture & module map

Layering is bottom-up: **scan → model → ui → app**. Lower layers never depend on higher ones.

| Module | File(s) | Responsibility |
|---|---|---|
| `main` | `src/main.rs` | Thin binary shim: init logging + panic hook, parse optional CLI path, build `NativeOptions` (1200x800, icon), call `eframe::run_native`. |
| `lib` | `src/lib.rs` | Crate root; re-exports modules. `format_size()` (binary GB/MB/KB/B) lives here, used everywhere. |
| `app` | `src/app.rs` | egui/eframe shell. Owns the `AppState` machine (Scanning → Loaded), auto-scans `$HOME` on startup, composes panels (menu bar, status bar, tree side panel, central treemap + breadcrumb), drives scans (cancellable), selection, settings, delete/Finder/clipboard actions. |
| `model` | `src/model/tree.rs`, `src/model/color.rs` | The in-memory `FileTree`/`FileNode` representation, the model-side `FileTree::scan` entry point, size/count aggregation, inode + firmlink dedup, extension histogram, and extension→color palette mapping. |
| `scan` | `src/scan/getattrlistbulk.rs`, `src/scan/mod.rs` | Native macOS directory walker: `getattrlistbulk` FFI, `open_dir`/`openat_dir`/`close_dir`, packed-buffer parsing, `DirEntry`. |
| `ui` | `src/ui/mod.rs`, `src/ui/tree_view.rs`, `src/ui/treemap_view.rs` | Two read-mostly egui views + the shared `ContextAction` enum returned to the app. |
| `settings` | `src/settings.rs` | User-tunable scan settings + flat `key=value` persistence under `~/.config/macdirstat/settings.txt`. |
| `logging` | `src/logging.rs` | Hand-rolled file logger + global panic hook → `~/Library/Logs/MacDirStat/macdirstat.log`. |
| `objc_ffi` | `src/objc_ffi.rs` | Tiny `objc_msgSend` bridge — **only** used to patch Info.plist strings for the native About panel. macOS-gated. Not a scanning concern. |

---

## Data flow (one scan, end to end)

1. **Start** — On startup `App` immediately scans `$HOME` (or the path given as `argv[1]`) on a background thread — there is **no startup folder picker**. "Open Folder…" (File menu / ⌘O) picks a different folder later.
2. **Spawn scan** — `spawn_scan` creates an `mpsc` channel, an `Arc<AtomicU64>` progress counter, and an `Arc<AtomicBool>` cancel flag, snapshots settings (`excluded_paths`, `skip_duplicate_inodes`, `min_file_size_bytes`), and runs `FileTree::scan` on a **background `std::thread`**. State → `Scanning`.
3. **Traverse** — `FileTree::scan` opens the root dir, then `build_node_fd` recursively walks it: `scan_dir_entries_fd` issues `getattrlistbulk` into a thread-local 256 KB buffer, entries are partitioned into files/dirs, filters applied (exclusion / min-size / inode dedup), and subdirectories are descended via `openat` — in parallel with rayon `par_iter` when a dir has ≥ 2 subdirs. `build_child` checks the cancel flag first, so a **Stop** unwinds the recursion promptly and returns the partial tree.
4. **Aggregate** — Each node precomputes `size`, `file_count`, `dir_count` bottom-up; children are sorted by size descending. A per-thread extension→bytes map is drained (main thread + `rayon::broadcast`) into a global histogram (also size-sorted). The progress counter ticks once per counted file.
5. **Receive** — The UI thread (forcing `request_repaint` while scanning) polls the channel with `try_recv` each frame; on result it builds a `ColorMap` from the extensions and transitions to `Loaded`. If the cancel flag was set, the result is marked `partial` and the status bar shows a ⚠ indicator.
6. **Render** — egui draws the tree view (left side panel), the cushion treemap (central panel, layout cached into a CPU-rasterized texture), and the breadcrumb. Selection is a `TreePath` (`Vec<usize>` index path) shared `&mut` by both views.
7. **Act** — User actions flow back: clicking a rect/row sets selection; the breadcrumb rescans an ancestor; context menus and the status bar emit Delete / CopyPath / Reveal-in-Finder. Deletes mutate the tree in place (subtract from ancestors, remove node, rebuild extensions, recompute `ColorMap`) and **invalidate the cached layout + texture**.

**Stopping a scan:** while `Scanning`, the central panel shows a prominent **Stop** button (plus a compact one in the status bar). Clicking it calls `request_stop`, which sets the cancel flag; the traversal unwinds, the partial tree is shown as `Loaded { partial: true }`, and a "⚠ Partial scan (stopped)" chip appears in the status bar. Rescan to get full results.

---

## Key subsystems in depth

### Scanning (`src/scan/getattrlistbulk.rs`)
- One `getattrlistbulk(2)` call returns N entries with `NAME | DEVID | OBJTYPE | FILEID | TOTALSIZE` inline — far fewer kernel round trips than `readdir + lstat`. ~91% of scan time is kernel syscalls; parallelization (rayon) is the only big lever.
- Buffer parsing is **manual and order-sensitive**: per-entry u32 length, then the 20-byte `attribute_set_t` (`ATTR_CMN_RETURNED_ATTRS`), then the name via a self-relative `attrreference_t` offset, then DEVID/OBJTYPE/FILEID/TOTALSIZE advanced field-by-field, each gated on the returned-attrs bitmask. Changing the requested attribute set without updating the cursor walk silently misparses. Reads are bounds-checked (`read_u32/i32/u64` return 0 out of range) to survive truncated buffers.
- Traversal is `openat()`-anchored (fd-relative, never re-resolving full paths). `open_dir`/`openat_dir` return raw fds the caller **must** pair with `close_dir`; `scan_dir_entries_fd` deliberately does not close its fd.
- macOS attr constants missing from `libc` are defined locally with explicit hex from `sys/attr.h`. All syscall/raw-pointer work is in narrow `unsafe` blocks; `std::mem::zeroed()` constructs POD C structs.

### Dedup (`src/model/tree.rs`, `ScanCtx`)
- When `skip_duplicate_inodes` is on: files are deduped by `(devid, fileid)`, directories by `(st_dev, st_ino)` from `fstat` on the opened fd. Two different sources — they must agree on a volume for firmlink-twin dropping to be consistent.
- A duplicate firmlink directory is **dropped entirely** (filter_map → `None`), not kept as an empty placeholder. The scan root is pre-registered in `seen_dirs` so its own firmlink twin elsewhere is skipped.
- `fileid == 0` (or `devid == 0`) means "attribute unavailable" → the file is never deduped (correctness tradeoff for non-APFS volumes that may double-count hardlinks).

### Optimization-mode size filter
- `Settings::min_file_size_bytes()` returns `min_file_size_mb * 1024 * 1024` **only when `optimization_mode` is on**, else 0 (disabled). Filtered files never enter the tree or the histogram, so totals in optimization mode are genuinely smaller. Default settings ship with `optimization_mode = true` and a 25 MiB floor — a default scan ignores files < 25 MiB.

### Model (`src/model/tree.rs`)
- `FileNode`: `name: Box<str>`, `children: Box<[FileNode]>`, `size: u64`, `rect: NodeRect` (f32 ×4), `file_count: u32`, `dir_count: u32`, `is_dir: bool`. **Exactly 72 bytes** — guarded by a `const _: assert!(size_of == 72)` in `tree.rs`. `Box<str>`/`Box<[T]>` over `String`/`Vec`, an f32 `NodeRect` (egui is f32; cushion math converts to f64 at use), and u32 counts (`saturating_add`; 4.29 B ceiling is far beyond any real volume) keep per-node RSS low at millions of nodes. Scan-path hash maps/sets use `FxHashMap`/`FxHashSet` (rustc-hash), not SipHash.
- Nodes are addressed by `TreePath = Vec<usize>` (child-index path). Mutation is a matched pair: `remove_child` fixes the direct parent; `subtract_from_ancestors` fixes root + intermediate ancestors (it deliberately skips the direct parent). All decrements use `saturating_sub`.
- `FileNode.rect` is default-initialized and only meaningful **after the UI layout pass** writes into it.

### Treemap (`src/ui/treemap_view.rs`)
- `layout_node` feeds child sizes into the `treemap` crate's squarified `TreemapLayout` and stores each node's rect **on the model** (so the tree must be passed `&mut`).
- Cushion shading is the WinDirStat algorithm: per-node parabolic ridges accumulate into a 4-coefficient quadratic surface; `render_cushion_image` CPU-rasterizes per-pixel Phong-ish diffuse+ambient shading into a `ColorImage`, uploaded as a `NEAREST` texture.
- The texture is regenerated **only when the canvas `Rect` changes**. After a tree mutation that doesn't resize the canvas, the app must manually reset `cached_layout_rect` and `treemap_texture` to `None` (delete does this).
- `find_node_at` hit-tests a pixel to a `TreePath` by descending nested rects — returns the deepest containing node (a click in a parent's padding selects the parent).

### Tree view (`src/ui/tree_view.rs`)
- Collapsible rows with hand-drawn folder icons, zebra striping, right-aligned size column, and alpha-faded long filenames. Caps at `MAX_RENDERED_ITEMS = 2000` with an "… and N more" marker. Up/Down arrows navigate the flattened visible rows. Ancestors auto-expand on selection change (gated so manual collapse isn't fought).

### Settings (`src/settings.rs`)
- Five fields: `ignore_cloud_storage`, `skip_duplicate_inodes`, `optimization_mode`, `min_file_size_mb`, and `scan_threads` (Advanced: 0 = auto/one-per-core; >0 runs the scan in a dedicated rayon pool — `FileTree::scan` then broadcasts on *that* pool to drain its workers' ext maps). **No serde/toml** — a flat `key=value` text file. `load()` is infallible (falls back to `Default`); `save()` swallows all I/O errors. `excluded_paths()` returns `$HOME/Library/CloudStorage` and `.../Mobile Documents` when `ignore_cloud_storage`.

### Logging + panic (`src/logging.rs`)
- Custom `log::Log` impl behind the `log` facade (one process-wide `static LOGGER`). Own target logs Info+, framework targets (eframe/winit/wgpu) Warn+. Single-file rotation at 5 MiB (→ `.log.old`). `install_panic_hook` records location + message + backtrace, then chains the previous hook. Timestamps via raw `libc::localtime_r` to avoid a chrono/time dependency.

---

## Conventions & principles

- **Threading:** only `FileTree::scan` runs off-thread (background thread + `mpsc` channel; UI polls `try_recv` each frame). **Never block the UI for scanning.** Cancellation is cooperative — a shared `Arc<AtomicBool>` is checked in the traversal; the thread is never killed. Note: `rfd` folder pickers and the `osascript` delete confirmation *do* block the UI thread by nature.
- **Immediate-mode UI:** panels are re-registered every frame in a fixed order, **`CentralPanel` last**. Scanning/empty states reuse the same panel IDs (`menu_bar`, `status_bar`, `tree_view`) so layout doesn't jump.
- **Borrow-conflict discipline (load-bearing):** UI closures collect intents into local `bool`/`Option`, then act *after* the closure. Deferred fields (`pending_scan`, `open_settings_requested`) are set inside a `&mut self.state` borrow and consumed at the `App` level afterward — moving them inside the `match` would double-borrow.
- **Cache-invalidation discipline:** after any tree mutation, reset `cached_layout_rect` + `treemap_texture` to `None` and recompute `color_map`, or the treemap renders stale geometry/colors.
- **`unsafe` discipline:** narrowly scoped, comment-justified blocks for syscalls/FFI/POD struct init. The `objc_msgSend` bridge transmutes per call arity — an arity/type mismatch is UB.
- **Infallible side effects:** settings/logging I/O never panics; errors are swallowed (`let _ = …`, `if let Ok`).
- **Commits:** atomic, one logical change each (the dedup feature landed as a sequence: fetch devid/fileid → dedup logic → Settings field → UI toggle → behavior refinement). Semantic 0.x versioning with explicit bump commits. Throwaway benchmark/exploration code is deleted once decisions lock in.
- **Scope:** stay within the requested change; minimal diffs; don't add unrelated UI/APIs.

---

## Platform notes / gotchas

- **macOS-only.** `getattrlistbulk`, `openat`, `fstat`, the attribute-buffer layout, the `~/Library/Logs` convention, and `objc_ffi` (`#[cfg(target_os = "macos")]`) are all Darwin-specific. No fallback exists.
- **Delete is permanent** — `execute_delete` uses `fs::remove_file`/`remove_dir_all`, **not** the macOS Trash. `Cmd+Delete` bypasses confirmation; bare Delete shows the native osascript dialog (defaults to Cancel, only proceeds on exact `button returned:Delete`).
- **Extension color = size rank, not the extension string** — color is `palette[rank % 18]`, so the same extension can change color across scans or after `rebuild_extensions`.
- **`raw_extension` does not normalize case** (`JPG` ≠ `jpg`); extensionless files bucket under the literal `(no ext)`; a trailing-dot name has empty extension; a leading-dot dotfile (`.gitignore`) yields extension `gitignore`.
- **Root node's `name`** is the full display path of the scanned root (every other node's name is its `DirEntry` basename); `build_fs_path` relies on `root_path` separately.
- **Settings "Cancel"** reloads from disk, discarding in-memory edits made while the window was open. `min_file_size_mb` is clamped to ≥ 1.
- **APFS contention:** thread-pool oversubscription is *slower*; default `num_cpus` is optimal. Treemap coordinate scaling between the `treemap` crate and egui Painter is a known foot-gun (an early 2x-scale bug).
- **Gitignored:** `Cargo.lock`, `benches/`, and `CLAUDE.md` are not committed.

---

## Where things live (task → file)

| Task | Edit |
|---|---|
| Change directory-walk / syscall / buffer parsing | `src/scan/getattrlistbulk.rs` |
| Change tree building, aggregation, or inode/firmlink dedup | `src/model/tree.rs` (`build_node_fd`, `ScanCtx`) |
| Tweak treemap colors / palette / brightness | `src/model/color.rs` |
| Change treemap layout, cushion shading, or hit-testing | `src/ui/treemap_view.rs` |
| Change the directory tree view (rows, nav, fade, icons) | `src/ui/tree_view.rs` |
| Add/change a context-menu action | `src/ui/mod.rs` (`ContextAction`) + `handle_context_action` in `src/app.rs` |
| Add a setting | `src/settings.rs` (struct + `Default` + `save` + `parse`) → wire into Settings window & `start_scan` in `src/app.rs` |
| Adjust the menu bar / status bar / breadcrumb | `src/app.rs` (`show_menu_bar`, `show_status_bar`, `show_breadcrumb`) |
| Change scan orchestration / state machine / startup folder | `src/app.rs` (`AppState`, `spawn_scan`, `default_scan_dir`, `update`) |
| Change the Stop button / cancellation behavior | `src/app.rs` (`show_scanning_panes`, `compact_stop_button`, `request_stop`) + cancel check in `src/model/tree.rs` (`build_child`) |
| Change delete / Finder / clipboard behavior | `src/app.rs` (`execute_delete`, `handle_finder_action`, `native_confirm_delete`) |
| Change logging, rotation, or the panic hook | `src/logging.rs` |
| Change the native About panel metadata | `src/objc_ffi.rs` + `configure_about_panel_text` in `src/app.rs` |
| Change window options / icon / CLI arg handling | `src/main.rs` |
| Change byte formatting | `src/lib.rs` (`format_size`) |
