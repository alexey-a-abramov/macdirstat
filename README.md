# MacDirStat

[![CI](https://github.com/alexey-a-abramov/macdirstat/actions/workflows/ci.yml/badge.svg)](https://github.com/alexey-a-abramov/macdirstat/actions/workflows/ci.yml)
[![Security audit](https://github.com/alexey-a-abramov/macdirstat/actions/workflows/ci.yml/badge.svg?event=push&label=security%20audit)](https://github.com/alexey-a-abramov/macdirstat/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/alexey-a-abramov/macdirstat)](https://github.com/alexey-a-abramov/macdirstat/releases/latest)

A disk usage visualizer for macOS, inspired by [WinDirStat](https://windirstat.net/) and [WizTree](https://diskanalyzer.com/). I loved the functionality of these tools but was never really satisfied with the alternatives available on macOS, so I built my own.

## Features

- **Treemap visualization** with cushion shading (matching WinDirStat's look)
- **Directory tree** with collapsible nodes, keyboard navigation, and size annotations
- **Tree and treemap stay in sync** — selecting a rectangle scrolls the tree to it, and navigating (double-click, breadcrumb, back/forward) expands the tree to match the treemap's current folder
- **Approximate size indicator** — directory sizes are marked with "~" whenever the scan left content out (small-file threshold, excluded/cloud folders, unreadable directories), so you know when a total is a lower bound rather than exact
- **File type breakdown** showing extension statistics with color coding
- **Fast scanning** using macOS-native `getattrlistbulk` syscall with parallel tree building via rayon
- **Delete files/folders** directly from the UI — ⌘Delete for instant delete, Delete for native macOS confirmation dialog
- **Scans your home folder on startup** (or a path passed on the command line); open any other folder later via the File menu
- **Stoppable scans** — hit Stop at any time to halt scanning and inspect what was found so far

## Screenshot

<img src="https://github.com/MichaelStromberg/macdirstat/blob/main/screenshot.png?raw=true" alt="MacDirStat screenshot" width="750" />

## Download

Grab the prebuilt app from the [latest release](https://github.com/alexey-a-abramov/macdirstat/releases/latest) — download the zip, unzip it, and drag `MacDirStat.app` into `/Applications`. It's ad-hoc code-signed (not notarized), so on first launch right-click the app → **Open** once to let Gatekeeper through.

## Building

Requires Rust (2024 edition). macOS only — uses platform-specific APIs for fast directory scanning.

```sh
cargo build --release
```

### Build and install in one command

```sh
./scripts/bundle-mac.sh --install
```

This builds the release binary, assembles `MacDirStat.app`, copies it into
`/Applications` (replacing any previous copy), and launches it. To let it scan
protected locations, add it under **System Settings → Privacy & Security →
Full Disk Access**.

Leave off `--install` to just build the bundle at `target/release/bundle/MacDirStat.app`
without touching `/Applications`.

## Usage

```sh
# Launch — scans your home folder
cargo run --release

# Scan a specific directory
cargo run --release -- /path/to/scan
```

## How it works

MacDirStat scans directories using the macOS `getattrlistbulk` syscall, which retrieves multiple directory entries with their attributes in a single kernel call — avoiding per-file overhead. Directory traversal is parallelized across cores using rayon, with `openat()` for efficient relative path resolution.

The treemap uses squarified layout from the `treemap` crate with cushion-shaded rendering, producing the familiar WinDirStat look where each file is a colored rectangle sized proportionally to its disk usage.

## Security

CI runs [`cargo audit`](https://docs.rs/cargo-audit) against the RustSec advisory
database on every push to `main`. `.cargo/audit.toml` documents the only two
advisories currently suppressed — both are Linux-only accessibility code paths
(pulled in transitively by egui/eframe's `accesskit` feature) that never
compile into the macOS binary; each entry there links back to the
`cargo tree` trace that proves it.

## License

[GPL-3.0](LICENSE)
