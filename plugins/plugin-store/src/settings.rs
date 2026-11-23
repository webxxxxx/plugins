//! The plugin's own config file.
//!
//! `package/config/config.json` is the template the host copies on install;
//! after that the live copy is whatever `SVCD_CONFIG` points at, and
//! `plugin editconfig plugin-store` is how a user edits it. The format is
//! private to this plugin — svcd neither parses nor validates it.
//!
//! ```json
//! {
//!   "github_token": "github_pat_...",
//!   "release_cache_minutes": 15
//! }
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use svcd_plugin::log_warn;

/// How long a resolved release may be reused without asking GitHub again.
/// Zero is allowed and means "always revalidate with the ETag".
pub const DEFAULT_CACHE_MINUTES: u64 = 15;
/// A day is plenty; anything larger is a typo, not a policy.
pub const MAX_CACHE_MINUTES: u64 = 24 * 60;
/// The host's file name for a plugin's active config.
pub const CONFIG_NAME: &str = "config";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Read-only GitHub token from the config file. Environment variables and
    /// the legacy `<data_dir>/github-token` file still work as fallbacks.
    pub github_token: Option<String>,
    /// Reuse window for the release lookup; `Duration::ZERO` disables it.
    pub cache_ttl: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            github_token: None,
            cache_ttl: Duration::from_secs(DEFAULT_CACHE_MINUTES * 60),
        }
    }
}

impl Settings {
    /// Read the active config file, falling back to defaults.
    ///
    /// Two locations are tried: `SVCD_CONFIG` when the host exports it, then
    /// the host's own `<data>/plugins/<id>/config` — the path `editconfig`
    /// writes. A missing file is the normal "not configured" case; an
    /// unreadable one is worth a log line but must not stop the plugin.
    pub fn load(config_path: Option<&Path>, data_dir: &Path) -> Self {
        let candidates: Vec<PathBuf> = match config_path {
            Some(path) => vec![path.to_path_buf(), data_dir.join(CONFIG_NAME)],
            None => vec![data_dir.join(CONFIG_NAME)],
        };
        for path in candidates {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    log_warn!("读取 {} 失败，改用默认值: {error}", path.display());
                    return Self::default();
                }
            };
            return match serde_json::from_str::<Value>(&text) {
                Ok(value) => Self::from_json(&value),
                Err(error) => {
                    log_warn!("{} 不是合法 JSON，改用默认值: {error}", path.display());
                    Self::default()
                }
            };
        }
        Self::default()
    }

    pub fn from_json(value: &Value) -> Self {
        let mut settings = Settings::default();

        match value.get("github_token").and_then(Value::as_str) {
            Some(raw) => match crate::net::clean_token(raw) {
                Some(token) => settings.github_token = Some(token),
                None if !raw.trim().is_empty() => log_warn!(
                    "配置里的 github_token 不合法（超过 {} 字节或含控制字符），已忽略",
                    crate::net::MAX_TOKEN_BYTES
                ),
                None => {}
            },
            None => {}
        }

        if let Some(minutes) = value.get("release_cache_minutes").and_then(Value::as_u64) {
            let minutes = minutes.min(MAX_CACHE_MINUTES);
            settings.cache_ttl = Duration::from_secs(minutes * 60);
        }

        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plugin-store-settings-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_hosts_config_file_is_found_without_svcd_config() {
        let dir = temp_dir("host");
        fs::write(
            dir.join(CONFIG_NAME),
            r#"{"github_token":"from-config","release_cache_minutes":5}"#,
        )
        .unwrap();
        let settings = Settings::load(None, &dir);
        assert_eq!(settings.github_token.as_deref(), Some("from-config"));
        assert_eq!(settings.cache_ttl, Duration::from_secs(5 * 60));

        // SVCD_CONFIG wins when the host exports it.
        let other = dir.join("other.json");
        fs::write(&other, r#"{"github_token":"from-env-var"}"#).unwrap();
        let settings = Settings::load(Some(&other), &dir);
        assert_eq!(settings.github_token.as_deref(), Some("from-env-var"));

        // Broken JSON falls back instead of failing the plugin.
        fs::write(dir.join(CONFIG_NAME), "{").unwrap();
        assert_eq!(Settings::load(None, &dir), Settings::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_apply_when_keys_are_absent() {
        let settings = Settings::from_json(&serde_json::json!({}));
        assert_eq!(settings.github_token, None);
        assert_eq!(
            settings.cache_ttl,
            Duration::from_secs(DEFAULT_CACHE_MINUTES * 60)
        );
    }

    #[test]
    fn the_token_is_trimmed_and_a_bad_one_is_dropped() {
        let settings =
            Settings::from_json(&serde_json::json!({ "github_token": "  github_pat_x\n" }));
        assert_eq!(settings.github_token.as_deref(), Some("github_pat_x"));

        let settings = Settings::from_json(&serde_json::json!({ "github_token": "bad\u{7}token" }));
        assert_eq!(settings.github_token, None);

        let settings = Settings::from_json(&serde_json::json!({ "github_token": 42 }));
        assert_eq!(settings.github_token, None);
    }

    #[test]
    fn the_cache_window_is_bounded_and_zero_disables_it() {
        let minutes = |value: u64| {
            Settings::from_json(&serde_json::json!({ "release_cache_minutes": value })).cache_ttl
        };
        assert_eq!(minutes(5), Duration::from_secs(5 * 60));
        assert_eq!(minutes(0), Duration::ZERO);
        assert_eq!(
            minutes(100_000),
            Duration::from_secs(MAX_CACHE_MINUTES * 60)
        );
    }
}
