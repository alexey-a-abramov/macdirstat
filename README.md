# MacDirStat

A disk usage visualizer for macOS, inspired by [WinDirStat](https://windirstat.net/) and [WizTree](https://diskanalyzer.com/). I loved the functionality of these tools but was never really satisfied with the alternatives available on macOS, so I built my own.

## Features

- **Treemap visualization** with cushion shading (matching WinDirStat's look)
- **Directory tree** with collapsible nodes, keyboard navigation, and size annotations
- **File type breakdown** showing extension statistics with color coding
- **Fast scanning** using macOS-native `getattrlistbulk` syscall with parallel tree building via rayon
- **Delete files/folders** directly from the UI — ⌘Delete for instant delete, Delete for native macOS confirmation dialog
- **Scans your home folder on startup** (or a path passed on the command line); open any other folder later via the File menu
- **Stoppable scans** — hit Stop at any time to halt scanning and inspect what was found so far

## Screenshot

<img src="https://github.com/MichaelStromberg/macdirstat/blob/main/screenshot.png?raw=true" alt="MacDirStat screenshot" width="750" />

## Building

Requires Rust (2024 edition). macOS only — uses platform-specific APIs for fast directory scanning.

```sh
cargo build --release
```

### Installable app bundle

To build a double-clickable `MacDirStat.app` you can drag into `/Applications`:

```sh
./scripts/bundle-mac.sh
cp -R target/release/bundle/MacDirStat.app /Applications/
```

The bundle is ad-hoc code-signed, so on first launch right-click → **Open** once.
To let it scan protected locations, add it under **System Settings → Privacy &
Security → Full Disk Access**.

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

## License

[GPL-3.0](LICENSE)
