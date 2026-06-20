use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub ignore_cloud_storage: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { ignore_cloud_storage: true }
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
        let _ = std::fs::write(path, format!("ignore_cloud_storage={}\n", self.ignore_cloud_storage));
    }

    fn parse(content: &str) -> Self {
        let mut s = Self::default();
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("ignore_cloud_storage=") {
                s.ignore_cloud_storage = val.trim() == "true";
            }
        }
        s
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
