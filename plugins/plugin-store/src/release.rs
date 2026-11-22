//! GitHub release lookup for svcd's own upgrade path.
//!
//! svcd links no TLS, so the whole network half of an upgrade lives here: find
//! the newest stable release, pick one asset by file name, and hand its bytes
//! back as a blob. The host verifies the sha256 from `SHA256SUMS`; this module
//! never applies `PLUGIN_STORE_MIRROR`, because that would let a mirror choose
//! the bytes of the daemon itself.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use svcd_plugin::BlobWriter;

use crate::net::MAX_DOWNLOAD_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub assets: Vec<Asset>,
}

/// Parse one `/releases/latest` body. The endpoint already skips prereleases,
/// but a proxy or a hand-made response may not: treat either marker as an error
/// rather than risk downgrading the daemon to an unfinished build.
pub fn parse(body: &str) -> Result<Release, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| format!("GitHub release JSON: {error}"))?;
    parse_value(value)
}

fn parse_value(value: Value) -> Result<Release, String> {
    if value.get("draft").and_then(Value::as_bool).unwrap_or(false)
        || value
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err("最新 release 是预发布或草稿，不用来自动升级".into());
    }
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .ok_or("GitHub release 没有 tag_name")?
        .to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    if version.is_empty() {
        return Err(format!("tag {tag} 里没有版本号"));
    }
    let version = version.to_string();

    let entries = value
        .get("assets")
        .and_then(Value::as_array)
        .ok_or("GitHub release 没有 assets")?;
    let mut assets = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.get("state").and_then(Value::as_str) != Some("uploaded")
            && entry.get("state").is_some()
        {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("release 资产没有文件名")?;
        let size = entry
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("资产 {name} 没有大小"))?;
        let url = entry
            .get("browser_download_url")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
            .ok_or_else(|| format!("资产 {name} 没有下载地址"))?;
        assets.push(Asset {
            name: name.to_string(),
            size,
            url: url.to_string(),
        });
    }

    Ok(Release {
        tag,
        version,
        assets,
    })
}

/// The `download: false` answer: everything the host needs to decide whether an
/// upgrade exists, and nothing it would have to fetch bytes for.
pub fn check_json(release: &Release, asset: &Asset) -> Value {
    json!({
        "tag": release.tag,
        "version": release.version,
        "asset": {
            "name": asset.name,
            "size": asset.size,
        },
    })
}

/// GitHub's REST root for one repository's `/releases/latest`.
pub const API: &str = "https://api.github.com";

/// The transport is injected so parsing and picking never touch the network;
/// `main` passes `net::fetch_json`.
pub fn fetch(
    repo: &str,
    transport: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Release, String> {
    let url = latest_url(repo)?;
    parse(&transport(&url)?)
}

pub fn latest_url(repo: &str) -> Result<String, String> {
    let repo = repo.trim();
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("repo 必须是 owner/name 的形式：{repo:?}"));
    };
    for (label, part) in [("owner", owner), ("name", name)] {
        if part.is_empty()
            || part.len() > 100
            || part == "."
            || part == ".."
            || !part.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
        {
            return Err(format!("repo 的 {label} 不合法：{part:?}"));
        }
    }
    Ok(format!("{API}/repos/{owner}/{name}/releases/latest"))
}

/// Exactly one asset must match: a release that grows a second name matching
/// the pattern is a contract change, and guessing which one is the upgrade
/// would be a silent way to install the wrong binary.
pub fn pick<'a>(release: &'a Release, pattern: &str, platform: &str) -> Result<&'a Asset, String> {
    validate_pattern(pattern)?;
    validate_platform(platform)?;
    let pattern = expand(pattern.trim(), platform);
    let hits: Vec<&Asset> = release
        .assets
        .iter()
        .filter(|asset| wildcard_match(&pattern, &asset.name))
        .collect();

    match hits.as_slice() {
        [] => {
            let names: Vec<&str> = release
                .assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect();
            let names = if names.is_empty() {
                "无".to_string()
            } else {
                names.join("、")
            };
            Err(format!(
                "没有资产匹配 {pattern}（这个 release 有：{names}）"
            ))
        }
        [asset] => {
            if asset.size > MAX_DOWNLOAD_BYTES {
                return Err(format!(
                    "资产 {} 有 {} MiB，超过 {} MiB 的下载上限",
                    asset.name,
                    asset.size.div_ceil(1024 * 1024),
                    MAX_DOWNLOAD_BYTES / 1024 / 1024
                ));
            }
            Ok(asset)
        }
        _ => {
            let names: Vec<&str> = hits.iter().map(|asset| asset.name.as_str()).collect();
            Err(format!(
                "资产 {pattern} 匹配到 {} 个：{}；请把 pattern 写得更精确",
                hits.len(),
                names.join("、")
            ))
        }
    }
}

pub fn expand(pattern: &str, platform: &str) -> String {
    pattern.replace("{platform}", platform)
}

/// Fetch the picked asset into `temp/incoming/` and return its path, which the
/// caller hands back as a `$blob`. Nothing is cached: every upgrade asks for
/// the newest bytes, and a failed attempt leaves no `.part` file behind.
pub fn download_to(
    temp: &Path,
    asset: &Asset,
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    // The SDK owns the `incoming/` naming convention; it only reserves the
    // name here, and the download creates the bytes.
    let writer = BlobWriter::create_in(temp, &asset.name, "application/octet-stream")?;
    let path = writer.path().to_path_buf();
    drop(writer);
    if let Err(error) = fs::remove_file(&path) {
        return Err(format!("{}: {error}", path.display()));
    }

    let mut progress = |_: u64, _: Option<u64>| {};
    match crate::net::download_direct(&asset.url, &path, asset.size, &mut progress, cancelled) {
        Ok(_) => Ok(path),
        Err(error) => {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(crate::net::part_path(&path));
            Err(error)
        }
    }
}

/// File-name wildcard matching: `*` spans anything, `?` spans one character.
/// Everything else is literal, so a name such as `svcd-1.0.0+build.zip` needs
/// no escaping.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut after_star = 0usize;

    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            after_star = ni;
        } else if let Some(at) = star {
            pi = at + 1;
            after_star += 1;
            ni = after_star;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn validate_pattern(pattern: &str) -> Result<(), String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err("pattern 不能为空".into());
    }
    if pattern.chars().count() > 256 {
        return Err("pattern 太长（最多 256 个字符）".into());
    }
    if pattern
        .chars()
        .any(|character| matches!(character, '/' | '\\') || character.is_control())
    {
        return Err("pattern 只能是文件名，不能带路径分隔符".into());
    }
    Ok(())
}

fn validate_platform(platform: &str) -> Result<(), String> {
    let platform = platform.trim();
    if platform.is_empty() || platform.len() > 128 {
        return Err("platform 为空或太长".into());
    }
    if !platform
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err("platform 只能是字母、数字和 - _ .".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
      "tag_name": "v0.1.5",
      "draft": false,
      "prerelease": false,
      "assets": [
        {
          "name": "SHA256SUMS",
          "size": 320,
          "browser_download_url": "https://example.test/SHA256SUMS"
        },
        {
          "name": "svcd-0.1.5-x86_64-pc-windows-msvc.zip",
          "size": 1234567,
          "browser_download_url": "https://example.test/svcd.zip"
        }
      ]
    }"#;

    #[test]
    fn parses_a_stable_release_and_its_assets() {
        let release = parse(RELEASE).unwrap();

        assert_eq!(release.tag, "v0.1.5");
        assert_eq!(release.version, "0.1.5");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(
            release.assets[1].name,
            "svcd-0.1.5-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(release.assets[1].size, 1_234_567);
        assert_eq!(release.assets[1].url, "https://example.test/svcd.zip");
    }

    #[test]
    fn keeps_a_tag_that_has_no_leading_v() {
        let release = parse(r#"{"tag_name": "0.1.5", "assets": []}"#).unwrap();

        assert_eq!(release.version, "0.1.5");
    }

    #[test]
    fn refuses_a_prerelease_candidate() {
        let body = r#"{
          "tag_name": "v2.0.0-rc.1",
          "draft": false,
          "prerelease": true,
          "assets": [
            {"name": "svcd.zip", "size": 1, "browser_download_url": "https://example.test/x"}
          ]
        }"#;

        let error = parse(body).unwrap_err();

        assert!(error.contains("预发布"), "{error}");
    }

    #[test]
    fn a_check_result_carries_the_metadata_but_no_archive() {
        let release = parse(RELEASE).unwrap();
        let asset = release.assets[1].clone();

        let value = check_json(&release, &asset);

        assert_eq!(value["tag"], "v0.1.5");
        assert_eq!(value["version"], "0.1.5");
        assert_eq!(value["asset"]["name"], asset.name);
        assert_eq!(value["asset"]["size"], 1_234_567);
        assert!(value.get("archive").is_none(), "{value}");
        assert!(value.get("sums").is_none(), "{value}");
    }

    fn release_of(assets: &[(&str, u64)]) -> Release {
        Release {
            tag: "v0.1.5".into(),
            version: "0.1.5".into(),
            assets: assets
                .iter()
                .map(|(name, size)| Asset {
                    name: (*name).into(),
                    size: *size,
                    url: format!("https://example.test/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn picks_the_one_asset_the_platform_pattern_names() {
        let release = release_of(&[
            ("SHA256SUMS", 320),
            ("svcd-0.1.5-x86_64-pc-windows-msvc.zip", 1_234),
            ("svcd-0.1.5-x86_64-unknown-linux-gnu.tar.gz", 2_345),
        ]);

        let asset = pick(&release, "svcd-*-{platform}.zip", "x86_64-pc-windows-msvc").unwrap();
        assert_eq!(asset.name, "svcd-0.1.5-x86_64-pc-windows-msvc.zip");

        // The checksum list is its own lookup, so the host can call the same
        // capability twice: once for the archive, once for SHA256SUMS.
        let sums = pick(&release, "SHA256SUMS", "x86_64-pc-windows-msvc").unwrap();
        assert_eq!(sums.name, "SHA256SUMS");
    }

    #[test]
    fn no_match_names_the_assets_that_were_there() {
        let release = release_of(&[
            ("SHA256SUMS", 320),
            ("svcd-0.1.5-aarch64-unknown-linux-gnu.tar.gz", 2_345),
        ]);

        let error = pick(&release, "svcd-*-{platform}.zip", "x86_64-pc-windows-msvc").unwrap_err();

        assert!(
            error.contains("svcd-0.1.5-aarch64-unknown-linux-gnu.tar.gz"),
            "{error}"
        );
        assert!(error.contains("SHA256SUMS"), "{error}");
    }

    #[test]
    fn ambiguous_matches_are_refused_with_every_candidate() {
        let release = release_of(&[("svcd-1.0.0-x.zip", 1), ("svcd-1.0.0-x.zip.sig", 1)]);

        let error = pick(&release, "svcd-*", "x").unwrap_err();

        assert!(error.contains("svcd-1.0.0-x.zip.sig"), "{error}");
        assert!(
            error.contains("更精确") || error.contains("多个"),
            "{error}"
        );
    }

    #[test]
    fn an_asset_over_the_download_budget_is_refused() {
        let release = release_of(&[("svcd-big.zip", MAX_DOWNLOAD_BYTES + 1)]);

        let error = pick(&release, "svcd-*", "x").unwrap_err();

        assert!(error.contains("256 MiB"), "{error}");
    }

    #[test]
    fn wildcards_match_inside_a_file_name() {
        assert!(wildcard_match("svcd-*-x86_64.zip", "svcd-0.1.5-x86_64.zip"));
        assert!(!wildcard_match(
            "svcd-*-x86_64.zip",
            "svcd-0.1.5-aarch64.zip"
        ));
        assert!(wildcard_match("SHA256SUMS", "SHA256SUMS"));
        assert!(!wildcard_match("SHA256SUMS", "SHA256SUMS.sig"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "abbc"));
    }

    #[test]
    fn fetches_the_latest_release_at_the_github_endpoint() {
        let seen = std::cell::RefCell::new(String::new());

        let release = fetch("webxxxxx/plugin-manager", &|url| {
            *seen.borrow_mut() = url.to_string();
            Ok(RELEASE.to_string())
        })
        .unwrap();

        assert_eq!(
            seen.borrow().as_str(),
            "https://api.github.com/repos/webxxxxx/plugin-manager/releases/latest"
        );
        assert_eq!(release.version, "0.1.5");
    }

    #[test]
    fn refuses_a_repo_that_is_not_owner_slash_name() {
        for repo in [
            "",
            "plugin-manager",
            "webxxxxx/",
            "/plugins",
            "webxxxxx/plugins/extra",
            "web xxxxx/plugins",
            "webxxxxx/..",
            "webxxxxx/plugins?x=1",
        ] {
            assert!(
                fetch(repo, &|_| Ok(RELEASE.to_string())).is_err(),
                "{repo:?}"
            );
        }
    }

    #[test]
    fn downloads_an_asset_into_the_incoming_directory_byte_for_byte() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = b"zip-payload".to_vec();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            )
            .unwrap();
            stream.write_all(&server_body).unwrap();
        });

        let temp = std::env::temp_dir().join(format!(
            "plugin-store-release-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        let asset = Asset {
            name: "svcd-0.1.5-x86_64-pc-windows-msvc.zip".into(),
            size: body.len() as u64,
            url: format!("http://{address}/asset.zip"),
        };

        let path = download_to(&temp, &asset, &|| false).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert_eq!(path.parent().unwrap(), temp.join("incoming"));
        assert!(!crate::net::part_path(&path).exists());

        let _ = std::fs::remove_dir_all(&temp);
        server.join().unwrap();
    }

    #[test]
    #[ignore = "hits api.github.com; run by hand when the network is available"]
    fn finds_the_latest_stable_release_of_the_daemon() {
        let release = fetch("webxxxxx/plugin-manager", &|url| {
            crate::net::fetch_json(url)
        })
        .expect("latest release");

        let sums = pick(&release, "SHA256SUMS", "x86_64-pc-windows-msvc").expect("checksum list");

        assert_eq!(sums.name, "SHA256SUMS");
        assert!(!release.tag.is_empty());
    }
}
