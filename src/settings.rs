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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ignore_cloud_storage: true,
            skip_duplicate_inodes: true,
            optimization_mode: true,
            min_file_size_mb: 25,
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
        let _ = std::fs::write(
            path,
            format!(
                "ignore_cloud_storage={}\nskip_duplicate_inodes={}\noptimization_mode={}\nmin_file_size_mb={}\n",
                self.ignore_cloud_storage,
                self.skip_duplicate_inodes,
                self.optimization_mode,
                self.min_file_size_mb,
            ),
        );
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

    pub fn excluded_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if self.ignore_cloud_storage {
            if let Ok(home) = std::env::var("HOME") {
                let home = PathBuf::from(home);
                paths.push(home.join("Library/CloudStorage"));
                paths.push(home.join("Library/Mobile Documents"));
            }
        }
        paths
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
