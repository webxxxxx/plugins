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

#[derive(Debug)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug)]
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

/// The daemon this capability exists for. `repo` may be passed explicitly, but
/// svcd's own upgrade never has to name it.
pub const DEFAULT_REPO: &str = "webxxxxx/plugin-manager";

/// The checksum list the host uses after downloading. Its presence is
/// advertised as `sumsAsset`, so a release without it is refused before any
/// bytes move.
pub const SUMS_ASSET: &str = "SHA256SUMS";

/// The `download: false` answer: every asset the release carries, plus whether
/// a checksum list exists. The host picks its archive by target suffix and
/// refuses to upgrade when `sumsAsset` is null.
pub fn release_json(release: &Release) -> Value {
    let assets: Vec<Value> = release
        .assets
        .iter()
        .map(|asset| {
            json!({
                "name": asset.name,
                "size": asset.size,
            })
        })
        .collect();
    let sums = release
        .assets
        .iter()
        .find(|asset| asset.name == SUMS_ASSET)
        .map(|asset| asset.name.clone());
    json!({
        "tag": release.tag,
        "version": release.version,
        "assets": assets,
        "sumsAsset": sums,
    })
}

/// GitHub's REST root for one repository's `/releases/latest`.
pub const API: &str = "https://api.github.com";

/// Same lookup against another API root; tests point this at a local server so
/// the whole capability can be exercised without GitHub.
pub fn fetch_at(
    base: &str,
    repo: &str,
    transport: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Release, String> {
    let url = latest_url_at(base, repo)?;
    parse(&transport(&url)?)
}

pub fn latest_url_at(base: &str, repo: &str) -> Result<String, String> {
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
    Ok(format!(
        "{}/repos/{owner}/{name}/releases/latest",
        base.trim_end_matches('/')
    ))
}

/// Pick one asset for download by its exact file name. The names come from the
/// `release_json` answer, so the host never guesses with a wildcard; a typo
/// fails loudly. This is also where the 256 MiB budget is enforced, before any
/// bytes move.
pub fn download_asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("asset 不能为空".into());
    }
    let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
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
        return Err(format!("release 里没有资产 {name}（可选：{names}）"));
    };
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
    fn a_check_result_lists_every_asset_and_marks_the_checksum_list() {
        let release = parse(RELEASE).unwrap();

        let value = release_json(&release);

        assert_eq!(value["tag"], "v0.1.5");
        assert_eq!(value["version"], "0.1.5");
        assert_eq!(value["assets"].as_array().unwrap().len(), 2);
        assert_eq!(value["assets"][0]["name"], "SHA256SUMS");
        assert_eq!(
            value["assets"][1]["name"],
            "svcd-0.1.5-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(value["assets"][1]["size"], 1_234_567);
        assert_eq!(value["sumsAsset"], "SHA256SUMS");
    }

    #[test]
    fn a_release_without_sums_reports_a_null_sums_asset() {
        let release = release_of(&[("svcd-0.1.5-x86_64-pc-windows-msvc.zip", 1_234)]);

        let value = release_json(&release);

        assert!(value["sumsAsset"].is_null(), "{value}");
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
    fn finds_an_asset_by_its_exact_name() {
        let release = release_of(&[
            ("SHA256SUMS", 320),
            ("svcd-0.1.5-x86_64-pc-windows-msvc.zip", 1_234),
        ]);

        let asset = download_asset(&release, "svcd-0.1.5-x86_64-pc-windows-msvc.zip").unwrap();
        assert_eq!(asset.size, 1_234);

        // The checksum list is its own lookup, so the host calls the same
        // capability twice: once for the archive, once for SHA256SUMS.
        assert_eq!(
            download_asset(&release, "SHA256SUMS").unwrap().name,
            "SHA256SUMS"
        );
    }

    #[test]
    fn an_unknown_asset_lists_the_candidates() {
        let release = release_of(&[
            ("SHA256SUMS", 320),
            ("svcd-0.1.5-aarch64-unknown-linux-gnu.tar.gz", 2_345),
        ]);

        let error = download_asset(&release, "svcd-0.1.5-x86_64.zip").unwrap_err();

        assert!(
            error.contains("svcd-0.1.5-aarch64-unknown-linux-gnu.tar.gz"),
            "{error}"
        );
        assert!(error.contains("SHA256SUMS"), "{error}");
    }

    #[test]
    fn an_asset_over_the_download_budget_is_refused() {
        let release = release_of(&[("svcd-big.zip", MAX_DOWNLOAD_BYTES + 1)]);

        let error = download_asset(&release, "svcd-big.zip").unwrap_err();

        assert!(error.contains("256 MiB"), "{error}");
    }

    #[test]
    fn fetches_the_latest_release_at_the_github_endpoint() {
        let seen = std::cell::RefCell::new(String::new());

        let release = fetch_at(API, "webxxxxx/plugin-manager", &|url| {
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
                fetch_at(API, repo, &|_| Ok(RELEASE.to_string())).is_err(),
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
        // Same path the plugin uses: an optional token (env or data_dir) and
        // the ETag cache. Run with GITHUB_TOKEN set to exercise the 5000/hour
        // branch; without it, this needs anonymous quota to be available.
        let dir = std::env::temp_dir().join("plugin-store-live-release");
        let _ = std::fs::remove_dir_all(&dir);
        let token = crate::net::github_token(&dir);
        let transport = |url: &str| crate::net::fetch_json(url, token.as_deref(), Some(&dir));
        let release = fetch_at(API, DEFAULT_REPO, &transport).expect("latest release");

        let value = release_json(&release);

        assert_eq!(value["sumsAsset"], SUMS_ASSET);
        assert!(value["assets"].as_array().unwrap().len() > 1);
        assert!(!release.tag.is_empty());

        // The second call must be answered from the cached body: GitHub sends
        // 304 for the same ETag, and 304 does not consume quota.
        let cached = fetch_at(API, DEFAULT_REPO, &transport).expect("cached release");
        assert_eq!(cached.tag, release.tag);
        assert!(dir.join("release-cache.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
