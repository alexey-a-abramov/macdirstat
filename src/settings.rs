use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub ignore_cloud_storage: bool,
    /// Skip files/directories whose (device, inode) pair has already been seen.
    /// Prevents double-counting hardlinks and macOS firmlinks (e.g. /Users ↔
    /// /System/Volumes/Data/Users).  Default: true.
    pub skip_duplicate_inodes: bool,
    /// When true, files smaller than `min_file_size_mb` MiB are ignored during
    /// scanning. Dramatically reduces scan time on directories with many small
    /// files. Default: true, 25 MiB.
    pub optimization_mode: bool,
    pub min_file_size_mb: u64,
    /// Advanced: number of worker threads for scanning. 0 = automatic (one per
    /// logical core). Capping this can help on slow/contended volumes where the
    /// default oversubscribes the disk; the default is optimal for fast SSDs.
    pub scan_threads: u64,
    /// User-defined absolute folder paths to skip entirely during scans.
    /// Useful for slow/network/cloud trees the built-in toggle doesn't cover.
    pub custom_excludes: Vec<String>,
    /// Show the floating info card (name/size/path) when an item is selected.
    pub show_info_card: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ignore_cloud_storage: true,
            skip_duplicate_inodes: true,
            optimization_mode: true,
            min_file_size_mb: 25,
            scan_threads: 0,
            custom_excludes: Vec::new(),
            show_info_card: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.serialize());
    }

    /// Serialize to the flat `key=value` settings format. Custom excludes are
    /// written as repeated `exclude=<path>` lines.
    fn serialize(&self) -> String {
        let mut out = format!(
            "ignore_cloud_storage={}\nskip_duplicate_inodes={}\noptimization_mode={}\nmin_file_size_mb={}\nscan_threads={}\nshow_info_card={}\n",
            self.ignore_cloud_storage,
            self.skip_duplicate_inodes,
            self.optimization_mode,
            self.min_file_size_mb,
            self.scan_threads,
            self.show_info_card,
        );
        for p in &self.custom_excludes {
            out.push_str("exclude=");
            out.push_str(p);
            out.push('\n');
        }
        out
    }

    fn parse(content: &str) -> Self {
        let mut s = Self::default();
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("ignore_cloud_storage=") {
                s.ignore_cloud_storage = val.trim() == "true";
            } else if let Some(val) = line.strip_prefix("skip_duplicate_inodes=") {
                s.skip_duplicate_inodes = val.trim() == "true";
            } else if let Some(val) = line.strip_prefix("optimization_mode=") {
                s.optimization_mode = val.trim() == "true";
            } else if let Some(val) = line.strip_prefix("min_file_size_mb=") {
                if let Ok(n) = val.trim().parse::<u64>() {
                    s.min_file_size_mb = n;
                }
            } else if let Some(val) = line.strip_prefix("scan_threads=") {
                if let Ok(n) = val.trim().parse::<u64>() {
                    s.scan_threads = n;
                }
            } else if let Some(val) = line.strip_prefix("exclude=") {
                let p = val.trim();
                if !p.is_empty() {
                    s.custom_excludes.push(p.to_string());
                }
            } else if let Some(val) = line.strip_prefix("show_info_card=") {
                s.show_info_card = val.trim() == "true";
            }
        }
        s
    }

    /// Returns the minimum file size in bytes for the optimization filter,
    /// or 0 if optimization mode is disabled.
    pub fn min_file_size_bytes(&self) -> u64 {
        if self.optimization_mode {
            self.min_file_size_mb * 1024 * 1024
        } else {
            0
        }
    }

    /// Absolute folder paths the scanner should skip: the built-in cloud roots
    /// (when enabled) plus any user-defined excludes.
    pub fn excluded_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if self.ignore_cloud_storage
            && let Ok(home) = std::env::var("HOME")
        {
            let home = PathBuf::from(home);
            // Modern macOS routes Google Drive / OneDrive / Dropbox / Box and
            // iCloud Drive through the File Provider extension under these two
            // roots; the legacy top-level mounts are covered as a fallback.
            paths.push(home.join("Library/CloudStorage"));
            paths.push(home.join("Library/Mobile Documents"));
            paths.push(home.join("Dropbox"));
            paths.push(home.join("OneDrive"));
            paths.push(home.join("Google Drive"));
        }
        for c in &self.custom_excludes {
            paths.push(PathBuf::from(c));
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_parse_roundtrip_with_excludes() {
        let mut s = Settings::default();
        s.optimization_mode = false;
        s.scan_threads = 6;
        s.custom_excludes = vec![
            "/Users/me/Library/Caches".to_string(),
            "/Volumes/Backup".to_string(),
        ];
        s.show_info_card = false;
        let parsed = Settings::parse(&s.serialize());
        assert_eq!(parsed, s);
    }

    #[test]
    fn excluded_paths_includes_cloud_and_custom() {
        let mut s = Settings::default();
        s.ignore_cloud_storage = true;
        s.custom_excludes = vec!["/tmp/skip-me".to_string()];
        let paths = s.excluded_paths();
        assert!(paths.iter().any(|p| p.ends_with("Library/CloudStorage")));
        assert!(
            paths
                .iter()
                .any(|p| p.ends_with("Library/Mobile Documents"))
        );
        assert!(paths.iter().any(|p| p == &PathBuf::from("/tmp/skip-me")));

        s.ignore_cloud_storage = false;
        let paths = s.excluded_paths();
        assert!(!paths.iter().any(|p| p.ends_with("Library/CloudStorage")));
        assert!(paths.iter().any(|p| p == &PathBuf::from("/tmp/skip-me")));
    }
}

fn settings_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("macdirstat")
            .join("settings.txt")
    })
}
