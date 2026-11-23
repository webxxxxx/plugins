//! `plugin-store download`: fetch every package listed in docs/plugins.md.
//!
//! This deliberately calls the same `net::download` path the store uses for
//! installs, so proxy support, the DoH fallback for poisoned GitHub asset DNS,
//! and PLUGIN_STORE_MIRROR all apply here too.

use crate::{catalog, net};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_MANIFEST: &str = "docs/plugins.md";
const DEFAULT_OUTPUT: &str = "dist";

struct Options {
    manifest: PathBuf,
    output: PathBuf,
    only: Vec<String>,
    force: bool,
    dry_run: bool,
    help: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            manifest: PathBuf::from(DEFAULT_MANIFEST),
            output: PathBuf::from(DEFAULT_OUTPUT),
            only: Vec::new(),
            force: false,
            dry_run: false,
            help: false,
        };
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--manifest" => {
                    index += 1;
                    options.manifest = PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--manifest 缺少路径".to_string())?,
                    );
                }
                "--output" => {
                    index += 1;
                    options.output = PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--output 缺少目录".to_string())?,
                    );
                }
                "--only" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "--only 缺少包名".to_string())?;
                    if value.trim().is_empty() {
                        return Err("--only 不能为空".into());
                    }
                    options.only.push(value.clone());
                }
                "--force" => options.force = true,
                "--dry-run" => options.dry_run = true,
                "-h" | "--help" => options.help = true,
                other if other.starts_with("--manifest=") => {
                    options.manifest = PathBuf::from(&other["--manifest=".len()..]);
                }
                other if other.starts_with("--output=") => {
                    options.output = PathBuf::from(&other["--output=".len()..]);
                }
                other if other.starts_with("--only=") => {
                    let value = &other["--only=".len()..];
                    if value.trim().is_empty() {
                        return Err("--only 不能为空".into());
                    }
                    options.only.push(value.to_string());
                }
                other => {
                    return Err(format!("未知参数 {other}（--help 查看用法）"));
                }
            }
            index += 1;
        }
        if options.manifest.as_os_str().is_empty() {
            return Err("--manifest 不能为空".into());
        }
        if options.output.as_os_str().is_empty() {
            return Err("--output 不能为空".into());
        }
        Ok(options)
    }
}

pub fn print_help() {
    println!(
        "usage: plugin-store download [options]

把 docs/plugins.md 里的所有 .spk 下载到 dist/。

  --manifest <path>  插件列表 Markdown（默认 {DEFAULT_MANIFEST}）
  --output <dir>     下载目录（默认 {DEFAULT_OUTPUT}）
  --only <text>      只下载插件名/版本/平台/文件名包含该文本的包；可重复，匹配任一
  --force            重新下载已存在的包
  --dry-run          只打印文件名和下载地址
  -h, --help         显示这段帮助"
    );
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args)?;
    if options.help {
        print_help();
        return Ok(());
    }

    let text = fs::read_to_string(&options.manifest)
        .map_err(|error| format!("读取 {} 失败：{error}", options.manifest.display()))?;
    // The table is Markdown; a Qiniu `index.json` works too, picked by suffix
    // so `--manifest` stays one option instead of a second flag.
    let format = if options
        .manifest
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        catalog::Format::Json
    } else {
        catalog::Format::Markdown
    };
    let entries = format
        .parse(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", options.manifest.display()))?;

    let mut packages = Vec::with_capacity(entries.len());
    let mut seen = HashSet::new();
    for entry in entries {
        let file = asset_name(&entry.url)?;
        if !seen.insert(file.clone()) {
            return Err(format!("多个下载地址指向同一个文件名：{file}"));
        }
        packages.push((entry, file));
    }

    if !options.only.is_empty() {
        let needles: Vec<String> = options
            .only
            .iter()
            .map(|value| value.to_lowercase())
            .collect();
        packages.retain(|(entry, file)| matches_filter(entry, file, &needles));
        if packages.is_empty() {
            return Err(format!("没有匹配的包：{}", options.only.join(", ")));
        }
    }

    if options.dry_run {
        for (entry, file) in &packages {
            println!("{file}\t{}", entry.url);
        }
        println!("{} 个包；--dry-run 未写文件。", packages.len());
        return Ok(());
    }

    fs::create_dir_all(&options.output)
        .map_err(|error| format!("创建 {} 失败：{error}", options.output.display()))?;

    let total = packages.len();
    let started = Instant::now();
    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;
    let mut failures: Vec<(String, String)> = Vec::new();

    for (index, (entry, file)) in packages.iter().enumerate() {
        let destination = options.output.join(file);
        if destination.exists() && !destination.is_file() {
            failures.push((
                file.clone(),
                format!("目标不是普通文件：{}", destination.display()),
            ));
            continue;
        }
        if options.force {
            match fs::remove_file(&destination) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    failures.push((
                        file.clone(),
                        format!("删除 {} 失败：{error}", destination.display()),
                    ));
                    continue;
                }
            }
        }
        let existed = destination
            .metadata()
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false);
        let label = format!("[{}/{}] {file}", index + 1, total);
        let size_hint = entry.size_bytes;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut progress = |done: u64, total: Option<u64>| {
            if last_emit.elapsed() < Duration::from_millis(250) {
                return;
            }
            let total = total.or((size_hint > 0).then_some(size_hint));
            let percent = total
                .filter(|total| *total > 0)
                .map(|total| (done.saturating_mul(100) / total).min(100));
            match (percent, total) {
                (Some(percent), Some(total)) => {
                    eprint!(
                        "\r{label}  {} / {}  {:>3}%",
                        human_size(done),
                        human_size(total),
                        percent
                    );
                }
                _ => eprint!("\r{label}  {}", human_size(done)),
            }
            let _ = io::stderr().flush();
            last_emit = Instant::now();
        };
        let cancelled = || false;

        match net::download(
            &entry.url,
            &destination,
            size_hint,
            &mut progress,
            &cancelled,
        ) {
            Ok(size) => {
                clear_progress();
                if existed {
                    skipped += 1;
                    println!("{label} 已存在（{}）", human_size(size));
                } else {
                    downloaded += 1;
                    println!("{label} 下载完成（{}）", human_size(size));
                }
                bytes += size;
            }
            Err(error) => {
                clear_progress();
                eprintln!("{label} 失败：{error}");
                failures.push((file.clone(), error));
            }
        }
    }

    let elapsed = started.elapsed();
    println!(
        "完成：下载 {downloaded}，跳过 {skipped}，失败 {}；共 {}，用时 {:.1}s。",
        failures.len(),
        human_size(bytes),
        elapsed.as_secs_f64()
    );
    if failures.is_empty() {
        println!("输出目录：{}", options.output.display());
        Ok(())
    } else {
        eprintln!("失败的包：");
        for (file, error) in &failures {
            eprintln!("  {file}: {error}");
        }
        Err(format!("{} 个包下载失败", failures.len()))
    }
}

fn matches_filter(entry: &catalog::Entry, file: &str, needles: &[String]) -> bool {
    let haystack = format!(
        "{} {} {} {} {} {}",
        entry.name,
        entry.version,
        entry.platform,
        entry.device.as_deref().unwrap_or(""),
        file,
        entry.url
    )
    .to_lowercase();
    needles.iter().any(|needle| haystack.contains(needle))
}

fn clear_progress() {
    eprint!("\r{}\r", " ".repeat(120));
    let _ = io::stderr().flush();
}

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn asset_name(url: &str) -> Result<String, String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("");
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('\\')
        || Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return Err(format!("下载地址没有安全的文件名：{url}"));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_safe_asset_names() {
        assert_eq!(
            asset_name("https://github.com/x/y/releases/download/v1/meow-1.0.0-x86_64.spk")
                .unwrap(),
            "meow-1.0.0-x86_64.spk"
        );
        assert_eq!(
            asset_name("https://github.com/x/y/releases/download/v1/../x.spk").unwrap(),
            "x.spk"
        );
        assert!(asset_name("https://github.com/x/y/releases/download/v1/").is_err());
    }

    #[test]
    fn parses_only_filters() {
        let options =
            Options::parse(&["--only".into(), "meow".into(), "--only=sshd".into()]).unwrap();
        assert_eq!(options.only, vec!["meow", "sshd"]);
        assert!(Options::parse(&["--only".into()]).is_err());
    }

    #[test]
    fn matches_name_file_and_platform_case_insensitively() {
        let entry = catalog::Entry {
            name: "meow".into(),
            version: "1.0.1".into(),
            platform: "x86_64-pc-windows-msvc".into(),
            size: "1.0 MiB".into(),
            size_bytes: 1024 * 1024,
            url: "https://github.com/webxxxxx/plugins/releases/download/meow-v1.0.1/meow-1.0.1-x86_64-pc-windows-msvc.spk".into(),
            device: None,
        };
        let needles = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_lowercase())
                .collect::<Vec<_>>()
        };
        let file = "meow-1.0.1-x86_64-pc-windows-msvc.spk";
        assert!(matches_filter(&entry, file, &needles(&["MEOW"])));
        assert!(matches_filter(&entry, file, &needles(&["windows"])));
        assert!(matches_filter(&entry, file, &needles(&["1.0.1"])));
        assert!(!matches_filter(&entry, file, &needles(&["sshd"])));
    }

    #[test]
    fn parses_manifest_and_output_options() {
        let options = Options::parse(&[
            "--manifest".into(),
            "docs/plugins.md".into(),
            "--output=dist".into(),
            "--force".into(),
        ])
        .unwrap();
        assert_eq!(options.manifest, PathBuf::from("docs/plugins.md"));
        assert_eq!(options.output, PathBuf::from("dist"));
        assert!(options.force);
    }
}
