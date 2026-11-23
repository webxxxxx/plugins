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

/// Which catalog the store prefers.
///
/// This is a priority, not an exclusive choice: the other source stays in the
/// fallback chain, so a wrong `qiniu_base` or a blocked GitHub degrades to the
/// other list instead of an empty page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogSource {
    /// `docs/plugins.md` on GitHub and its raw proxies.
    #[default]
    Github,
    /// `index.json` in the bucket root of the Qiniu mirror.
    Qiniu,
}

impl CatalogSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CatalogSource::Github => "github",
            CatalogSource::Qiniu => "qiniu",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "github" => Some(CatalogSource::Github),
            "qiniu" => Some(CatalogSource::Qiniu),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Read-only GitHub token from the config file. Environment variables and
    /// the legacy `<data_dir>/github-token` file still work as fallbacks.
    pub github_token: Option<String>,
    /// Reuse window for the release lookup; `Duration::ZERO` disables it.
    pub cache_ttl: Duration,
    /// Which list to ask first.
    pub catalog_source: CatalogSource,
    /// Base URL of the Qiniu bucket that holds `index.json`, without a
    /// trailing slash. `None` means the mirror is not configured.
    pub qiniu_base: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            github_token: None,
            cache_ttl: Duration::from_secs(DEFAULT_CACHE_MINUTES * 60),
            catalog_source: CatalogSource::Github,
            qiniu_base: None,
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

        if let Some(raw) = value.get("github_token").and_then(Value::as_str) {
            match crate::net::clean_token(raw) {
                Some(token) => settings.github_token = Some(token),
                None if !raw.trim().is_empty() => log_warn!(
                    "配置里的 github_token 不合法（超过 {} 字节或含控制字符），已忽略",
                    crate::net::MAX_TOKEN_BYTES
                ),
                None => {}
            }
        }

        if let Some(minutes) = value.get("release_cache_minutes").and_then(Value::as_u64) {
            let minutes = minutes.min(MAX_CACHE_MINUTES);
            settings.cache_ttl = Duration::from_secs(minutes * 60);
        }

        match value.get("catalog_source") {
            Some(Value::String(raw)) => match CatalogSource::parse(raw) {
                Some(source) => settings.catalog_source = source,
                None => log_warn!(
                    "配置里的 catalog_source 不是 github 或 qiniu（是 {raw:?}），按 github 处理"
                ),
            },
            Some(other) => log_warn!(
                "配置里的 catalog_source 必须是字符串（是 {other}），按 github 处理"
            ),
            None => {}
        }

        if let Some(raw) = value.get("qiniu_base").and_then(Value::as_str) {
            match clean_base(raw) {
                Some(base) => {
                    if base.starts_with("http://") {
                        // Allowed on purpose: a bucket without a certificate
                        // still has to be usable. The signature verifies the
                        // bytes; only the list's freshness is exposed.
                        log_warn!(
                            "qiniu_base 用的是 http（{base}）：目录和包都走明文，能开证书就换成 https"
                        );
                    }
                    settings.qiniu_base = Some(base);
                }
                None if !raw.trim().is_empty() => log_warn!(
                    "配置里的 qiniu_base 必须是 http:// 或 https:// 开头的地址（是 {raw:?}），已忽略"
                ),
                None => {}
            }
        }

        settings
    }
}

/// Normalise the bucket base: http or https, no trailing slash, and tolerate a
/// pasted `…/index.json` (that is the URL people test with).
///
/// The scheme is kept as given: the store's parsing policy follows it, so an
/// http bucket is not silently upgraded to a URL its certificate cannot serve.
fn clean_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let base = trimmed.strip_suffix("/index.json").unwrap_or(trimmed);
    let base = base.trim_end_matches('/');
    if base.len() > 512 {
        return None;
    }
    let host = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))?;
    if host.is_empty() || host.chars().any(|character| character.is_whitespace() || character.is_control()) {
        return None;
    }
    Some(base.to_string())
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
    fn the_catalog_source_defaults_to_github_and_accepts_qiniu() {
        let settings = Settings::from_json(&serde_json::json!({}));
        assert_eq!(settings.catalog_source, CatalogSource::Github);
        assert_eq!(settings.qiniu_base, None);

        let settings = Settings::from_json(&serde_json::json!({ "catalog_source": " QiNiu " }));
        assert_eq!(settings.catalog_source, CatalogSource::Qiniu);

        // An unknown word is a typo, not a reason to stop: fall back to GitHub.
        for raw in [serde_json::json!("gitee"), serde_json::json!(""), serde_json::json!(3)] {
            let settings = Settings::from_json(&serde_json::json!({ "catalog_source": raw }));
            assert_eq!(settings.catalog_source, CatalogSource::Github);
        }
        assert_eq!(CatalogSource::Qiniu.as_str(), "qiniu");
    }

    #[test]
    fn the_qiniu_base_keeps_its_scheme_and_is_normalised() {
        let base = |value: serde_json::Value| {
            Settings::from_json(&serde_json::json!({ "qiniu_base": value })).qiniu_base
        };
        assert_eq!(
            base(serde_json::json!("https://dl.example.com/")),
            Some("https://dl.example.com".into())
        );
        // People paste the URL they just tested in a browser.
        assert_eq!(
            base(serde_json::json!("https://dl.example.com/index.json")),
            Some("https://dl.example.com".into())
        );
        assert_eq!(
            base(serde_json::json!("  https://dl.example.com//  ")),
            Some("https://dl.example.com".into())
        );
        // A bucket without a certificate stays reachable, and stays http: the
        // parsing policy follows what the user actually has.
        assert_eq!(
            base(serde_json::json!("http://dl.example.com/")),
            Some("http://dl.example.com".into())
        );
        assert_eq!(base(serde_json::json!("dl.example.com")), None);
        assert_eq!(base(serde_json::json!("ftp://dl.example.com")), None);
        assert_eq!(base(serde_json::json!("https://")), None);
        assert_eq!(base(serde_json::json!("")), None);
        assert_eq!(base(serde_json::json!(42)), None);
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
