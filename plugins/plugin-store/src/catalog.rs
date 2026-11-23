//! The catalog is a generated Markdown table in the plugins repository.
//!
//! It is deliberately parsed as text: the file is the contract the release
//! workflow already writes, and a JSON schema would be a second source of
//! truth. The parser is strict about rows it has entered (a malformed row is
//! an error), but ignores any table before the expected header.

use std::collections::BTreeMap;

use serde_json::Value;

pub const PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub size: String,
    pub size_bytes: u64,
    pub url: String,
    /// Optional device id encoded in the asset file name, after the platform
    /// (svcd's device-restricted package convention).
    pub device: Option<String>,
}

#[derive(Debug, Default)]
pub struct Page {
    pub items: Vec<Entry>,
    pub total: usize,
    pub page: usize,
    pub pages: usize,
}

/// The two shapes a catalog travels in.
///
/// `docs/plugins.md` is the Markdown table the GitHub workflow has always
/// written; `index.json` is the navigation file the Qiniu mirror keeps in its
/// bucket root. They carry the same rows, so everything above this module —
/// cache, freshness guard, UI — works with whichever one answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Json,
}

impl Format {
    pub fn parse(self, text: &str) -> Result<Vec<Entry>, String> {
        match self {
            Format::Markdown => parse(text),
            Format::Json => parse_json(text),
        }
    }

    /// One cache file per format: switching sources must not feed yesterday's
    /// bytes to today's parser.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Json => "json",
        }
    }
}

pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut header = false;
    let mut entries = Vec::new();

    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if !line.starts_with('|') || !line.ends_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 5 {
            continue;
        }
        if !header {
            if cells[0] == "插件" {
                header = true;
            }
            continue;
        }
        if is_separator(cells[0]) {
            continue;
        }

        let name = cells[0];
        let version = cells[1];
        let platform = cells[2];
        let size = cells[3];
        let url = extract_link(cells[4]).ok_or_else(|| {
            format!("第 {} 行的下载地址不是链接: {}", number + 1, cells[4])
        })?;

        if name.is_empty() || version.is_empty() || platform.is_empty() {
            return Err(format!("第 {} 行的插件/版本/平台为空", number + 1));
        }
        if !is_download_url(url) {
            return Err(format!("第 {} 行的下载地址不是 http(s) .spk: {}", number + 1, url));
        }

        entries.push(Entry {
            name: name.to_string(),
            version: version.to_string(),
            platform: platform.to_string(),
            size: if size.is_empty() {
                "-".into()
            } else {
                size.to_string()
            },
            size_bytes: parse_size(size).unwrap_or(0),
            device: device_from_asset(url, name, version, platform),
            url: url.to_string(),
        });
    }

    if !header {
        return Err("找不到插件表头".into());
    }
    if entries.is_empty() {
        return Err("插件表里没有数据行".into());
    }

    Ok(canonicalize(entries))
}

/// Parse the Qiniu mirror's `index.json` (`schema: 1`).
///
/// Strict about the fields the UI needs and about the download address,
/// because a catalog that half-parses is worse than one that is rejected: the
/// plugin then keeps the previous list instead of showing rows it cannot
/// install. `size` and `device` are optional — `size` only feeds the human
/// column, and a missing `device` is read from the asset name exactly like the
/// Markdown table does.
pub fn parse_json(text: &str) -> Result<Vec<Entry>, String> {
    let document: Value =
        serde_json::from_str(text).map_err(|error| format!("index.json 不是合法 JSON: {error}"))?;
    let schema = document.get("schema").and_then(Value::as_u64);
    if schema != Some(1) {
        return Err(format!(
            "index.json 的 schema 不是 1（是 {}）",
            document.get("schema").map_or("缺失".into(), Value::to_string)
        ));
    }
    let rows = document
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("index.json 缺少 packages 数组")?;
    if rows.is_empty() {
        return Err("index.json 的 packages 是空的".into());
    }

    let mut entries = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let line = index + 1;
        let field = |name: &str| -> Result<String, String> {
            row.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("index.json 第 {line} 行缺少 {name}"))
        };
        let name = field("plugin")?;
        let version = field("version")?;
        let platform = field("platform")?;
        let url = field("url")?;
        if !is_download_url(&url) {
            return Err(format!("index.json 第 {line} 行的下载地址不是 http(s) .spk: {url}"));
        }
        let size_bytes = row.get("size").and_then(Value::as_u64).unwrap_or(0);
        let device = row
            .get("device")
            .and_then(Value::as_str)
            .and_then(valid_device)
            .or_else(|| device_from_asset(&url, &name, &version, &platform));
        entries.push(Entry {
            name,
            version,
            platform,
            size: if size_bytes > 0 {
                human_size(size_bytes)
            } else {
                "-".into()
            },
            size_bytes,
            url,
            device,
        });
    }
    Ok(canonicalize(entries))
}

/// A package is identified by name, version, platform and (when present)
/// device. Duplicates collapse to the last one; BTreeMap also gives the UI a
/// deterministic order.
fn canonicalize(entries: Vec<Entry>) -> Vec<Entry> {
    let mut unique = BTreeMap::new();
    for entry in entries {
        unique.insert(
            (
                entry.name.clone(),
                entry.version.clone(),
                entry.platform.clone(),
                entry.device.clone(),
            ),
            entry,
        );
    }
    let mut entries: Vec<Entry> = unique.into_values().collect();
    entries.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.platform.cmp(&b.platform))
            .then_with(|| a.device.cmp(&b.device))
    });
    entries
}

/// A package address: `http(s)://…spk`, query string allowed.
///
/// The scheme is *syntax* here, not trust. Whether plaintext is acceptable is
/// the caller's decision — `check_scheme` — because it depends on the source
/// the list came from, and a parser that cannot say "http" cannot tell you that
/// the answer was http.
fn is_download_url(url: &str) -> bool {
    let plain = url.split('?').next().unwrap_or(url);
    (url.starts_with("https://") || url.starts_with("http://")) && plain.ends_with(".spk")
}

/// Reject plaintext package addresses when the list itself arrived over https.
///
/// A catalog fetched over https has no business pointing at `http://`: that
/// would downgrade exactly the hop the user paid for. A catalog fetched over
/// http may name http packages — refusing them would not add any security, it
/// would only break the mirror the user chose.
pub fn check_scheme(entries: &[Entry], require_https: bool) -> Result<(), String> {
    if !require_https {
        return Ok(());
    }
    let insecure: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.url.starts_with("http://"))
        .map(|entry| entry.url.as_str())
        .collect();
    if insecure.is_empty() {
        return Ok(());
    }
    Err(format!(
        "列表来自 https，但 {} 个包的地址是 http（第一个：{}）",
        insecure.len(),
        insecure[0]
    ))
}

fn is_separator(cell: &str) -> bool {
    !cell.is_empty()
        && cell
            .chars()
            .all(|character| character == '-' || character == ':' || character.is_whitespace())
}

fn extract_link(cell: &str) -> Option<&str> {
    let start = cell.find("](")? + 2;
    let rest = &cell[start..];
    let end = rest.find(')')?;
    Some(rest[..end].trim())
}

/// svcd names a device-restricted asset
/// <name>-<version>-<platform>-<device>.spk. The suffix is a human hint; the
/// host still re-checks manifest compat, but for the list it is the only
/// device fact the generated table exposes.
fn device_from_asset(url: &str, name: &str, version: &str, platform: &str) -> Option<String> {
    let file = url.split('/').next_back()?.split('?').next()?;
    let stem = file.strip_suffix(".spk")?;
    let marker = format!("-{version}-{platform}");
    let position = stem.find(&marker)?;
    let file_plugin = &stem[..position];
    let after_platform = &stem[position + marker.len()..];

    // Canonical layout: <name>-<version>-<platform>-<device>.spk
    if let Some(device) = after_platform.strip_prefix('-') {
        if let Some(device) = valid_device(device) {
            return Some(device);
        }
    }

    // Also accept the device on either side of the version, e.g. a table that
    // says "sshd-sr6110" while the asset is still named "sshd-...".
    if file_plugin != name {
        for (longer, shorter) in [(file_plugin, name), (name, file_plugin)] {
            if let Some(device) = longer.strip_prefix(&format!("{shorter}-")) {
                if let Some(device) = valid_device(device) {
                    return Some(device);
                }
            }
        }
    }
    None
}

fn valid_device(device: &str) -> Option<String> {
    let device = device.trim();
    if device.is_empty()
        || device.len() > 64
        || !device
            .chars()
            .next()
            .map(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
            .unwrap_or(false)
        || !device.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
    {
        return None;
    }
    Some(device.to_string())
}

fn parse_size(text: &str) -> Option<u64> {
    let (number, unit) = text.trim().split_once(' ')?;
    let value: f64 = number.parse().ok()?;
    let multiplier = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = value * multiplier;
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

/// The `15.5 MiB` label the Markdown table carries as text and `index.json`
/// carries as a byte count.
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB"] {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 || next == "GiB" {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

/// True when `fetched` is strictly older than `current` for every row they
/// share: the mirror answered with a file that predates what is already in
/// hand, so applying it would move the UI backwards.
///
/// A row the current list does not have (a plugin released since) means the
/// fetched file is newer, and an unparsable version is treated as equal — the
/// guard only fires when the evidence is unambiguous.
pub fn is_older(fetched: &[Entry], current: &[Entry]) -> bool {
    if current.is_empty() || fetched.is_empty() {
        return false;
    }
    let key = |entry: &Entry| {
        (
            entry.name.clone(),
            entry.platform.clone(),
            entry.device.clone().unwrap_or_default(),
        )
    };
    let mut known: BTreeMap<_, &str> = BTreeMap::new();
    for entry in current {
        known.insert(key(entry), entry.version.as_str());
    }

    let mut strictly_older = false;
    for entry in fetched {
        let Some(existing) = known.get(&key(entry)) else {
            return false;
        };
        match compare_versions(&entry.version, existing) {
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Less => strictly_older = true,
            std::cmp::Ordering::Equal => {}
        }
    }
    strictly_older
}

/// Compare two `x.y.z[-pre]` versions numerically; anything unparsable
/// compares equal, which keeps [`is_older`] from guessing.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (numeric_core(left), numeric_core(right)) {
        (Some(left_core), Some(right_core)) => {
            let ordering = left_core.cmp(&right_core);
            if ordering != Ordering::Equal {
                return ordering;
            }
            // Same core: a pre-release sorts below the release.
            let pre = |version: &str| version.contains('-');
            pre(right).cmp(&pre(left))
        }
        _ => Ordering::Equal,
    }
}

fn numeric_core(version: &str) -> Option<Vec<u64>> {
    version
        .split(['-', '+'])
        .next()?
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// Filter by platform and (when the asset name carries one) device, search,
/// and cut one 50-row page. A device-suffixed asset is only visible on that
/// exact host device; no suffix means universal for the platform. page is
/// 1-based and clamped to the last page; an empty result reports page 1.
pub fn select(
    entries: &[Entry],
    target: &str,
    device: Option<&str>,
    scope: &str,
    query: &str,
    page: usize,
) -> Page {
    let query = query.trim().to_ascii_lowercase();
    let mut matched: Vec<Entry> = entries
        .iter()
        .filter(|entry| {
            if scope == "all" {
                return true;
            }
            if entry.platform != target {
                return false;
            }
            match entry.device.as_deref() {
                // A device-suffixed asset is only installable on that exact id.
                // No suffix means universal for the platform (the host still
                // re-checks manifest compat at install time).
                Some(required) => device == Some(required),
                None => true,
            }
        })
        .filter(|entry| query.is_empty() || entry.name.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect();

    let total = matched.len();
    let pages = total.div_ceil(PAGE_SIZE);
    let page = page.max(1).min(pages.max(1));
    let start = (page - 1) * PAGE_SIZE;
    let items = if start < total {
        matched
            .drain(start..(start + PAGE_SIZE).min(total))
            .collect()
    } else {
        Vec::new()
    };
    Page {
        items,
        total,
        page,
        pages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# 插件下载

| 插件 | 版本 | 平台 | 大小 | 下载地址 |
| --- | --- | --- | ---: | --- |
| meow | 1.0.1 | x86_64-unknown-linux-musl | 14.7 MiB | [meow.spk](https://github.com/webxxxxx/plugins/releases/download/meow-v1.0.1/meow-1.0.1-x86_64-unknown-linux-musl.spk) |
| sshd | 1.0.0 | x86_64-unknown-linux-gnu | 2.7 MiB | [sshd.spk](https://github.com/webxxxxx/plugins/releases/download/v0.1.1/sshd-1.0.0-x86_64-unknown-linux-gnu.spk) |
| meow | 1.0.1 | x86_64-unknown-linux-gnu | 14.5 MiB | [meow.spk](https://github.com/webxxxxx/plugins/releases/download/meow-v1.0.1/meow-1.0.1-x86_64-unknown-linux-gnu.spk) |
"#;

    #[test]
    fn parses_the_generated_table() {
        let entries = parse(SAMPLE).unwrap();
        assert_eq!(entries.len(), 3);
        let meow = entries
            .iter()
            .find(|entry| entry.platform == "x86_64-unknown-linux-musl")
            .unwrap();
        assert_eq!(meow.name, "meow");
        assert_eq!(meow.size_bytes, 15_414_067);
        assert!(meow.url.ends_with(".spk"));
    }

    const INDEX: &str = r#"{
      "schema": 1,
      "base_url": "https://dl.example.com",
      "packages": [
        {
          "plugin": "hysteria",
          "version": "1.0.0",
          "platform": "x86_64-unknown-linux-gnu",
          "device": null,
          "file": "hysteria-1.0.0-x86_64-unknown-linux-gnu.spk",
          "size": 17825792,
          "sha256": "aa",
          "url": "https://dl.example.com/hysteria-1.0.0-x86_64-unknown-linux-gnu.spk"
        },
        {
          "plugin": "sshd",
          "version": "1.0.0",
          "platform": "x86_64-unknown-linux-gnu",
          "device": "router1",
          "file": "sshd-1.0.0-x86_64-unknown-linux-gnu-router1.spk",
          "size": 640000,
          "url": "https://dl.example.com/sshd-1.0.0-x86_64-unknown-linux-gnu-router1.spk"
        }
      ]
    }"#;

    #[test]
    fn parses_the_qiniu_index_like_the_markdown_table() {
        let entries = parse_json(INDEX).unwrap();
        assert_eq!(entries.len(), 2);

        let hysteria = &entries[0];
        assert_eq!(hysteria.name, "hysteria");
        assert_eq!(hysteria.platform, "x86_64-unknown-linux-gnu");
        assert_eq!(hysteria.device, None);
        assert_eq!(hysteria.size_bytes, 17_825_792);
        assert_eq!(hysteria.size, "17.0 MiB");
        assert!(hysteria.url.ends_with(".spk"));

        // An explicit device wins, and the two sources agree on the identity.
        let sshd = entries
            .iter()
            .find(|entry| entry.name == "sshd")
            .unwrap();
        assert_eq!(sshd.device.as_deref(), Some("router1"));
        assert_eq!(sshd.size, "625.0 KiB");

        let markdown = parse(SAMPLE).unwrap();
        assert!(!is_older(&entries, &markdown));
        assert!(!is_older(&markdown, &entries));
    }

    #[test]
    fn a_missing_device_falls_back_to_the_asset_name() {
        let text = r#"{"schema":1,"packages":[{"plugin":"sshd","version":"1.0.0",
            "platform":"x86_64-unknown-linux-gnu","size":1,
            "url":"https://dl.example.com/sshd-1.0.0-x86_64-unknown-linux-gnu-router1.spk"}]}"#;
        let entries = parse_json(text).unwrap();
        assert_eq!(entries[0].device.as_deref(), Some("router1"));
    }

    #[test]
    fn rejects_an_index_that_would_install_the_wrong_thing() {
        let bad_schema = r#"{"schema":2,"packages":[]}"#;
        assert!(parse_json(bad_schema).unwrap_err().contains("schema"));

        let no_packages = r#"{"schema":1}"#;
        assert!(parse_json(no_packages).unwrap_err().contains("packages"));

        let empty = r#"{"schema":1,"packages":[]}"#;
        assert!(parse_json(empty).unwrap_err().contains("空的"));

        let not_spk = r#"{"schema":1,"packages":[{"plugin":"a","version":"1.0.0",
            "platform":"x86_64-unknown-linux-gnu","url":"https://dl.example.com/a.zip"}]}"#;
        assert!(parse_json(not_spk).unwrap_err().contains(".spk"));

        let no_url = r#"{"schema":1,"packages":[{"plugin":"a","version":"1.0.0","platform":"p"}]}"#;
        assert!(parse_json(no_url).unwrap_err().contains("url"));

        assert!(parse_json("{").unwrap_err().contains("JSON"));
    }

    #[test]
    fn plaintext_links_are_a_matter_of_where_the_list_came_from() {
        // Parsing is syntax: an http mirror has to be repres-entable at all.
        let http = r#"{"schema":1,"packages":[
            {"plugin":"a","version":"1.0.0","platform":"x86_64-unknown-linux-gnu",
             "url":"http://dl.example.com/a-1.0.0-x86_64-unknown-linux-gnu.spk"},
            {"plugin":"b","version":"1.0.0","platform":"x86_64-unknown-linux-gnu",
             "url":"https://dl.example.com/b-1.0.0-x86_64-unknown-linux-gnu.spk"}]}"#;
        let entries = parse_json(http).unwrap();
        assert_eq!(entries.len(), 2);

        // Fetched over http: http links are the same bargain, so they pass.
        assert!(check_scheme(&entries, false).is_ok());

        // Fetched over https: pointing at plaintext would downgrade the hop.
        let error = check_scheme(&entries, true).unwrap_err();
        assert!(error.contains("http"), "{error}");
        assert!(error.contains("a-1.0.0"), "{error}");

        // An https-only list passes both.
        let https = parse(SAMPLE).unwrap();
        assert!(check_scheme(&https, true).is_ok());
        assert!(check_scheme(&https, false).is_ok());
    }

    #[test]
    fn the_format_keeps_one_parser_per_wire_shape() {
        assert!(Format::Markdown.parse(SAMPLE).is_ok());
        assert!(Format::Markdown.parse(INDEX).is_err());
        assert!(Format::Json.parse(INDEX).is_ok());
        assert!(Format::Json.parse(SAMPLE).is_err());
        assert_eq!(Format::Markdown.extension(), "md");
        assert_eq!(Format::Json.extension(), "json");
    }

    #[test]
    fn filters_to_the_device_and_searches_by_name() {
        let entries = parse(SAMPLE).unwrap();
        let page = select(&entries, "x86_64-unknown-linux-gnu", None, "device", "", 1);
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        assert!(page
            .items
            .iter()
            .all(|entry| entry.platform.ends_with("gnu")));

        let page = select(
            &entries,
            "x86_64-unknown-linux-gnu",
            None,
            "device",
            "ssh",
            1,
        );
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "sshd");

        let page = select(&entries, "x86_64-pc-windows-msvc", None, "all", "MEOW", 1);
        assert_eq!(page.total, 2);
    }

    #[test]
    fn cuts_fifty_rows_per_page_and_clamps() {
        let entries: Vec<Entry> = (0..120)
            .map(|index| Entry {
                name: format!("plugin-{index:03}"),
                version: "1.0.0".into(),
                platform: "x86_64-unknown-linux-gnu".into(),
                size: "1 MiB".into(),
                size_bytes: 1024 * 1024,
                url: "https://github.com/x/y/releases/download/v1/x.spk".into(),
                device: None,
            })
            .collect();
        let page = select(&entries, "x86_64-unknown-linux-gnu", None, "device", "", 1);
        assert_eq!((page.total, page.pages, page.items.len()), (120, 3, 50));
        let page = select(&entries, "x86_64-unknown-linux-gnu", None, "device", "", 3);
        assert_eq!((page.page, page.items.len()), (3, 20));
        let page = select(&entries, "x86_64-unknown-linux-gnu", None, "device", "", 99);
        assert_eq!((page.page, page.items.len()), (3, 20));
    }

    const DEVICE_SAMPLE: &str = r#"
| 插件 | 版本 | 平台 | 大小 | 下载地址 |
| --- | --- | --- | ---: | --- |
| universal | 1.0.0 | x86_64-unknown-linux-gnu | 1.0 MiB | [universal.spk](https://github.com/webxxxxx/plugins/releases/download/v1/universal-1.0.0-x86_64-unknown-linux-gnu.spk) |
| sshd | 1.0.0 | x86_64-unknown-linux-gnu | 2.8 MiB | [sshd.spk](https://github.com/webxxxxx/plugins/releases/download/v1/sshd-1.0.0-x86_64-unknown-linux-gnu-sr6110.spk) |
| sshd | 1.0.0 | x86_64-unknown-linux-gnu | 2.8 MiB | [sshd.spk](https://github.com/webxxxxx/plugins/releases/download/v1/sshd-1.0.0-x86_64-unknown-linux-gnu-sr6110-v2.spk) |
"#;

    const NAME_DEVICE_SAMPLE: &str = r#"
| 插件 | 版本 | 平台 | 大小 | 下载地址 |
| --- | --- | --- | ---: | --- |
| sshd-sr6110 | 1.0.0 | x86_64-unknown-linux-gnu | 2.8 MiB | [sshd.spk](https://github.com/webxxxxx/plugins/releases/download/v1/sshd-1.0.0-x86_64-unknown-linux-gnu.spk) |
| sshd | 1.0.0 | x86_64-unknown-linux-gnu | 2.8 MiB | [sshd.spk](https://github.com/webxxxxx/plugins/releases/download/v1/sshd-sr6110-1.0.0-x86_64-unknown-linux-gnu.spk) |
"#;

    #[test]
    fn finds_a_device_marker_on_either_side_of_the_version() {
        let entries = parse(NAME_DEVICE_SAMPLE).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .all(|entry| entry.device.as_deref() == Some("sr6110")));
    }

    #[test]
    fn parses_and_filters_the_device_encoded_in_the_asset_name() {
        let entries = parse(DEVICE_SAMPLE).unwrap();
        let restricted: Vec<&Entry> = entries
            .iter()
            .filter(|entry| entry.device.is_some())
            .collect();
        assert_eq!(restricted.len(), 2);
        assert_eq!(restricted[0].device.as_deref(), Some("sr6110"));
        assert_eq!(restricted[1].device.as_deref(), Some("sr6110-v2"));

        let page = select(
            &entries,
            "x86_64-unknown-linux-gnu",
            Some("sr6110"),
            "device",
            "",
            1,
        );
        assert_eq!(page.total, 2);
        assert!(page.items.iter().any(|entry| entry.name == "universal"));
        assert!(page
            .items
            .iter()
            .any(|entry| entry.device.as_deref() == Some("sr6110")));

        let page = select(
            &entries,
            "x86_64-unknown-linux-gnu",
            Some("other"),
            "device",
            "",
            1,
        );
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "universal");

        let page = select(&entries, "x86_64-unknown-linux-gnu", None, "device", "", 1);
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "universal");
    }

    #[test]
    fn rejects_a_broken_download_cell() {
        let bad = "| 插件 | 版本 | 平台 | 大小 | 下载地址 |\n| --- | --- | --- | --- | --- |\n| x | 1.0.0 | p | 1 MiB | nope |\n";
        assert!(parse(bad).unwrap_err().contains("下载地址"));
    }

    #[test]
    fn parses_units() {
        assert_eq!(parse_size("15.5 MiB"), Some(16_252_928));
        assert_eq!(parse_size("2.7 MiB"), Some(2_831_155));
        assert_eq!(parse_size("512 B"), Some(512));
        assert_eq!(parse_size("nope"), None);
    }

    fn entry(name: &str, version: &str, platform: &str) -> Entry {
        Entry {
            name: name.into(),
            version: version.into(),
            platform: platform.into(),
            size: "1 MiB".into(),
            size_bytes: 1024 * 1024,
            url: "https://example.invalid/x.spk".into(),
            device: None,
        }
    }

    /// The mirror that answered with last week's file must not replace a list
    /// that already knows about a newer release.
    #[test]
    fn an_older_catalog_is_recognised() {
        let gnu = "x86_64-unknown-linux-gnu";
        let current = vec![
            entry("plugin-store", "0.1.10", gnu),
            entry("meow", "1.0.1", gnu),
        ];

        // Same rows, older version: stale.
        assert!(is_older(
            &[
                entry("plugin-store", "0.1.6", gnu),
                entry("meow", "1.0.1", gnu)
            ],
            &current
        ));
        // Numeric order, not lexical: 0.1.10 is newer than 0.1.9.
        assert!(is_older(&[entry("plugin-store", "0.1.9", gnu)], &current));
        assert!(!is_older(&[entry("plugin-store", "0.1.11", gnu)], &current));
        // Identical rows are not older — a rebuilt package keeps its version.
        assert!(!is_older(&current, &current));
        // A row the current list does not know about means a new release.
        assert!(!is_older(
            &[
                entry("plugin-store", "0.1.6", gnu),
                entry("new", "0.1.0", gnu)
            ],
            &current
        ));
        // Ambiguous input never fires the guard.
        assert!(!is_older(&[], &current));
        assert!(!is_older(
            &[entry("plugin-store", "nightly", gnu)],
            &current
        ));
    }
}
