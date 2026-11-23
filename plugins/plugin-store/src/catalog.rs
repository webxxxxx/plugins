//! The catalog is a generated Markdown table in the plugins repository.
//!
//! It is deliberately parsed as text: the file is the contract the release
//! workflow already writes, and a JSON schema would be a second source of
//! truth. The parser is strict about rows it has entered (a malformed row is
//! an error), but ignores any table before the expected header.

use std::collections::BTreeMap;

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
            format!(
                "第 {} 行的下载地址不是 https 链接: {}",
                number + 1,
                cells[4]
            )
        })?;

        if name.is_empty() || version.is_empty() || platform.is_empty() {
            return Err(format!("第 {} 行的插件/版本/平台为空", number + 1));
        }
        if !url.starts_with("https://") || !url.split('?').next().unwrap_or(url).ends_with(".spk") {
            return Err(format!("第 {} 行的下载地址不是 .spk: {}", number + 1, url));
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

    // A package is identified by name, version, platform and (when present)
    // device. Duplicates collapse to the last one; BTreeMap also gives the UI
    // a deterministic order.
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
    Ok(entries)
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
