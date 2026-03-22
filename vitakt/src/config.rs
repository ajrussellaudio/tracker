use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

// ── Config schema ─────────────────────────────────────────────────────────────

/// Global vitakt config, loaded from `~/.config/vitakt/config.toml`.
///
/// All fields are optional in the TOML file; missing fields use the defaults
/// defined by `Default::default()`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Config {
    /// Bookmark directories shown in the sample browser bookmark overlay.
    #[serde(default)]
    pub bookmarks: Vec<String>,

    /// Command for an external terminal file-picker (e.g. `"yazi"`).
    /// When set, pressing `e` in the sample browser launches it.
    #[serde(default)]
    pub file_browser: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { bookmarks: Vec::new(), file_browser: None }
    }
}

// ── Loader / saver ────────────────────────────────────────────────────────────

impl Config {
    /// Load the global config from `~/.config/vitakt/config.toml`.
    ///
    /// Never panics.  If the file is missing, unreadable, or malformed,
    /// returns `Config::default()` with a warning printed to stderr.
    pub fn load() -> Self {
        let path = match config_path() {
            Some(p) => p,
            None => return Self::default(),
        };

        if !path.exists() {
            return Self::default();
        }

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "vitakt: config warning: could not read {:?}: {} — using defaults",
                    path, e
                );
                return Self::default();
            }
        };

        match toml::from_str::<Self>(&raw) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "vitakt: config warning: malformed TOML in {:?}: {} — using defaults",
                    path, e
                );
                Self::default()
            }
        }
    }

    /// Persist the config to `~/.config/vitakt/config.toml`.
    ///
    /// Creates the directory and file if they do not exist.
    /// Uses an atomic write (write to temp file, then rename) to avoid
    /// corrupting the existing file on a partial write.
    pub fn save(&self) -> Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(self)?;
        // Atomic write: write to a temp file alongside the target, then rename.
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &serialized)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }
}

// ── Path helper ───────────────────────────────────────────────────────────────

fn config_path() -> Option<PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(
        std::path::Path::new(&home)
            .join(".config")
            .join("vitakt")
            .join("config.toml"),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Mutex to serialise tests that mutate HOME.
    static HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set HOME to a temp directory for the duration of the closure, then restore it.
    fn with_tmp_home<F: FnOnce(std::path::PathBuf)>(f: F) {
        let _guard = HOME_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir()
            .join(format!("vitakt_config_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.to_str().unwrap());
        f(tmp.clone());
        match original_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn nonexistent_config_file_returns_defaults() {
        with_tmp_home(|_tmp| {
            let cfg = Config::load();
            assert_eq!(cfg, Config::default());
            assert!(cfg.bookmarks.is_empty());
            assert!(cfg.file_browser.is_none());
        });
    }

    #[test]
    fn complete_valid_config_loads_correctly() {
        with_tmp_home(|tmp| {
            let dir = tmp.join(".config").join("vitakt");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("config.toml"),
                r#"
bookmarks = ["/home/user/samples", "/mnt/audio"]
file_browser = "yazi"
"#,
            )
            .unwrap();
            let cfg = Config::load();
            assert_eq!(cfg.bookmarks, vec!["/home/user/samples", "/mnt/audio"]);
            assert_eq!(cfg.file_browser, Some("yazi".to_string()));
        });
    }

    #[test]
    fn missing_optional_fields_return_defaults() {
        with_tmp_home(|tmp| {
            let dir = tmp.join(".config").join("vitakt");
            std::fs::create_dir_all(&dir).unwrap();
            // Only bookmarks set; file_browser omitted.
            std::fs::write(dir.join("config.toml"), r#"bookmarks = ["/samples"]"#).unwrap();
            let cfg = Config::load();
            assert_eq!(cfg.bookmarks, vec!["/samples"]);
            assert!(cfg.file_browser.is_none());
        });
    }

    #[test]
    fn unrecognised_fields_do_not_panic() {
        with_tmp_home(|tmp| {
            let dir = tmp.join(".config").join("vitakt");
            std::fs::create_dir_all(&dir).unwrap();
            // "future_field" is unknown; must not cause an error.
            std::fs::write(
                dir.join("config.toml"),
                r#"
bookmarks = ["/samples"]
future_field = "some value"
"#,
            )
            .unwrap();
            // Should load without panicking; known fields parsed correctly.
            let cfg = Config::load();
            assert_eq!(cfg.bookmarks, vec!["/samples"]);
        });
    }

    #[test]
    fn save_to_nonexistent_path_creates_file_with_correct_content() {
        with_tmp_home(|tmp| {
            let cfg = Config {
                bookmarks: vec!["/samples/drums".to_string()],
                file_browser: Some("ranger".to_string()),
            };
            cfg.save().unwrap();
            let path = tmp.join(".config").join("vitakt").join("config.toml");
            assert!(path.exists(), "config file should have been created");
            let content = std::fs::read_to_string(&path).unwrap();
            let loaded: Config = toml::from_str(&content).unwrap();
            assert_eq!(loaded.bookmarks, vec!["/samples/drums"]);
            assert_eq!(loaded.file_browser, Some("ranger".to_string()));
        });
    }

    #[test]
    fn save_then_modify_and_save_preserves_all_fields() {
        with_tmp_home(|tmp| {
            // Write an initial config.
            let cfg1 = Config {
                bookmarks: vec!["/samples/drums".to_string()],
                file_browser: Some("yazi".to_string()),
            };
            cfg1.save().unwrap();

            // Load, add a bookmark, save again.
            let mut cfg2 = Config::load();
            cfg2.bookmarks.push("/samples/synths".to_string());
            cfg2.save().unwrap();

            // Reload and verify both bookmarks and file_browser survive.
            let cfg3 = Config::load();
            assert_eq!(
                cfg3.bookmarks,
                vec!["/samples/drums", "/samples/synths"],
                "both bookmarks should be present after re-save"
            );
            assert_eq!(
                cfg3.file_browser,
                Some("yazi".to_string()),
                "file_browser should be preserved across save/load cycle"
            );
            let path = tmp.join(".config").join("vitakt").join("config.toml");
            assert!(path.exists());
        });
    }
}
