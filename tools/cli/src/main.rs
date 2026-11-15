//! CLI entry for `plugin-tools`. See `lib.rs` for the four verbs.

use plugin_tools::{
    build_from_recipe, devkey, host_triple, instantiate, pack, pascal, read_manifest, read_recipe,
    sync, valid_name, vendor_sdk, write_project, LANGUAGES, PLATFORMS, Project, Repo,
};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

const RESERVED: &[&str] = &["svc", "svcd", "mcp", "plugin", "core", "system", "svcd-plugin"];

fn fail(message: impl Into<String>) -> ! {
    eprintln!("plugin-tools: {}", message.into());
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().map(String::as_str) else {
        usage();
        std::process::exit(1);
    };
    let result = match verb {
        "template" => template(&args[1..]),
        "build" => build(&args[1..]),
        "dev" => dev(&args[1..]),
        "devkey" => devkey().map(|path| println!("{}", path.display())).map(|_| ()),
        "sync" => {
            let repo = Repo::discover(flag(&args[1..], "--repo").map(PathBuf::from).as_deref());
            match repo {
                Ok(repo) => {
                    let count = sync(&repo);
                    match count {
                        Ok(count) => {
                            println!("synced {count} plugin(s)");
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            }
        }
        "check" => check(&args[1..]),
        "-h" | "--help" | "help" => {
            usage();
            Ok(())
        }
        other => Err(format!("unknown verb {other}")),
    };
    if let Err(error) = result {
        fail(error);
    }
}

fn usage() {
    eprintln!(
        "usage: plugin-tools <template|build|devkey|sync|check> [options]

  template <name>   从 templates/<lang> 创建项目
      --lang <id>         rust | c | shell | powershell（省略则交互选择）
      --platforms <ids>   逗号分隔的目标三元组（省略则交互选择）
      --webui             带上 package/payload/web
      --desc <text>       清单里的一句话描述
      --dir <path>        创建到哪（默认 plugins）
      --force             覆盖已存在的目录

  build [options]   运行 build.json，然后 plugin bundle
      --dir <path>        插件目录（默认从 cwd 向上找）
      --platform <t>      目标三元组（默认本机）
      --key <pem>         用这把钥签名；不给则产出未签名包
      --out <file>        产物路径
      --no-verify         跳过 plugin verify

  dev [options]     打包 + 安装 + 启动（首装不自启，这里代做）
  devkey            生成机器级开发钥（~/.svcd-dev）
  sync              刷新各插件里 vendored 的 SDK 副本
  check <file.spk>  plugin verify 包装"
    );
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).map(String::as_str)
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

/// Flags that consume the next argument. Anything else starting with `-` is a
/// boolean, so it must not swallow the positional that follows.
const VALUE_FLAGS: &[&str] = &[
    "--lang", "--platforms", "--desc", "--dir", "--key", "--out", "--platform", "--repo",
];

fn positional<'a>(args: &'a [String], skip: usize) -> Option<&'a str> {
    let mut seen = 0;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with("--") && arg.contains('=') {
            index += 1;
            continue;
        }
        if VALUE_FLAGS.contains(&arg) {
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        if seen == skip {
            return Some(arg);
        }
        seen += 1;
        index += 1;
    }
    None
}

fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    Ok(line.trim().to_string())
}

fn menu(title: &str, items: &[&str], multi: bool) {
    println!("\n{title}");
    for (index, item) in items.iter().enumerate() {
        println!("  {:>2}) {item}", index + 1);
    }
    if multi {
        println!("  可多选，用逗号分隔；直接回车表示全选");
    }
}

fn parse_selection(text: &str, items: &[&str], multi: bool) -> Option<Vec<String>> {
    let text = text.trim();
    if text.is_empty() {
        return multi.then(|| items.iter().map(|item| item.to_string()).collect());
    }
    if text.eq_ignore_ascii_case("all") {
        return Some(items.iter().map(|item| item.to_string()).collect());
    }
    let mut ids = Vec::new();
    for part in text.split([',', '，', ' ']).filter(|part| !part.is_empty()) {
        let number: usize = part.parse().ok()?;
        if number < 1 || number > items.len() {
            return None;
        }
        let id = items[number - 1].to_string();
        if !ids.contains(&id) {
            ids.push(id);
        }
        if !multi && ids.len() > 1 {
            return None;
        }
    }
    Some(ids)
}

fn template(args: &[String]) -> Result<(), String> {
    let name = positional(args, 0).ok_or_else(|| "template needs a <name>".to_string())?;
    if !valid_name(name) {
        return Err(format!(
            "\"{name}\" is not a valid plugin name: [a-z0-9][a-z0-9_-]{{0,63}}, no dots"
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!("\"{name}\" is reserved"));
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

    let language = match flag(args, "--lang") {
        Some(language) => language.to_string(),
        None if interactive => {
            menu("选择开发语言：", LANGUAGES, false);
            match ask("语言编号 > ")? {
                text => parse_selection(&text, LANGUAGES, false)
                    .and_then(|ids| ids.into_iter().next())
                    .ok_or_else(|| "无效的语言编号".to_string())?,
            }
        }
        None => return Err("--lang is required when stdin is not a terminal".into()),
    };
    if !LANGUAGES.iter().any(|id| *id == language) {
        return Err(format!(
            "unknown language \"{language}\" ({})",
            LANGUAGES.join(", ")
        ));
    }

    let host = host_triple().ok();
    let platforms: Vec<String> = match flag(args, "--platforms") {
        Some(list) => list
            .split([',', '，', ' '])
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        None if interactive => {
            menu("选择目标平台：", PLATFORMS, true);
            let prompt = match host {
                Some(triple) if PLATFORMS.iter().any(|id| *id == triple) => {
                    format!("平台编号（默认 {triple}） > ")
                }
                _ => "平台编号 > ".to_string(),
            };
            let text = ask(&prompt)?;
            if text.is_empty() {
                match host.filter(|triple| PLATFORMS.iter().any(|id| id == triple)) {
                    Some(triple) => vec![triple.to_string()],
                    None => return Err("select at least one platform".into()),
                }
            } else {
                parse_selection(&text, PLATFORMS, true)
                    .ok_or_else(|| "无效的平台编号".to_string())?
            }
        }
        None => {
            let triple = host.ok_or_else(|| {
                "--platforms is required when stdin is not a terminal".to_string()
            })?;
            vec![triple.to_string()]
        }
    };
    for platform in &platforms {
        if !PLATFORMS.iter().any(|id| *id == platform) {
            return Err(format!("unknown platform \"{platform}\" (see --help for the list)"));
        }
    }

    let repo = Repo::discover(flag(args, "--repo").map(PathBuf::from).as_deref())?;
    let source = repo.template(&language);
    if !source.is_dir() {
        return Err(format!("no template for --lang {language}"));
    }
    let dir = flag(args, "--dir").unwrap_or("plugins");
    let project = repo.root.join(dir).join(name);
    if project.exists() && !has(args, "--force") {
        return Err(format!(
            "{} already exists (use --force to overwrite)",
            project.display()
        ));
    }

    let desc = flag(args, "--desc").map(str::to_string).unwrap_or_else(|| format!("{name} plugin"));
    instantiate(&source, &project, name, &pascal(name), &desc)?;
    vendor_sdk(&repo, &project, &language)?;
    apply_webui(&project, has(args, "--webui"))?;
    write_project(
        &project,
        &Project {
            name: name.to_string(),
            language: language.clone(),
            platforms: platforms.clone(),
        },
    )?;

    println!("\ncreated {}", project.display());
    println!("  language:  {language}");
    println!("  platforms: {}", platforms.join(", "));
    println!("next: cd \"{}\" && plugin-tools build", project.display());
    Ok(())
}

fn apply_webui(project: &Path, enabled: bool) -> Result<(), String> {
    let web = project.join("package").join("payload").join("web");
    let manifest_path = project.join("package").join("manifest.json");
    let bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {}", manifest_path.display(), error))?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("parse manifest: {error}"))?;

    if enabled {
        if !web.is_dir() {
            return Err("this template has no web ui to include".into());
        }
        if manifest.get("ui").is_none() {
            manifest["ui"] = serde_json::json!({ "type": "web", "entry": "index.html" });
            write_manifest(&manifest_path, &manifest)?;
        }
        return Ok(());
    }

    if web.is_dir() {
        std::fs::remove_dir_all(&web).map_err(|error| format!("remove {}: {}", web.display(), error))?;
    }
    if manifest.get("ui").is_some() {
        if let Some(object) = manifest.as_object_mut() {
            object.remove("ui");
        }
        write_manifest(&manifest_path, &manifest)?;
    }
    Ok(())
}

fn write_manifest(path: &Path, manifest: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
    std::fs::write(path, format!("{text}\n")).map_err(|error| format!("write {}: {}", path.display(), error))
}

fn find_plugin_root(dir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(dir) = dir {
        return Ok(PathBuf::from(dir));
    }
    let mut current = std::env::current_dir().map_err(|error| error.to_string())?;
    loop {
        if current.join("package").join("manifest.json").is_file() {
            return Ok(current);
        }
        if !current.pop() {
            return Err("no package/manifest.json found; run inside a plugin directory or pass --dir".into());
        }
    }
}

fn build(args: &[String]) -> Result<(), String> {
    let (spk, manifest, signed) = pack_plugin(args)?;
    println!(
        "\npacked {}{}",
        spk.display(),
        if signed { "" } else { "  (unsigned)" }
    );
    if !signed {
        println!("host needs: allow_insecure_install = on   (restart svcd after changing it)");
    }
    println!(
        "next: plugin install \"{}\" && plugin start {}",
        spk.display(),
        manifest.name
    );
    Ok(())
}

/// Pack the plugin and install + start it: the local development loop.
fn dev(args: &[String]) -> Result<(), String> {
    let (spk, manifest, _signed) = pack_plugin(args)?;
    let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
    for verb in ["install", "start"] {
        let target = if verb == "install" {
            spk.display().to_string()
        } else {
            manifest.name.clone()
        };
        let status = std::process::Command::new(&bin)
            .args([verb, target.as_str()])
            .status()
            .map_err(|error| format!("{bin}: {error}"))?;
        if !status.success() {
            return Err(format!("{bin} {verb} exited {}", status.code().unwrap_or(-1)));
        }
    }
    println!("started {}", manifest.name);
    Ok(())
}

fn pack_plugin(args: &[String]) -> Result<(PathBuf, plugin_tools::Manifest, bool), String> {
    let root = find_plugin_root(flag(args, "--dir"))?;
    let manifest = read_manifest(&root)?;
    let recipe = read_recipe(&root)?;
    let platform = match flag(args, "--platform") {
        Some(platform) => platform.to_string(),
        None => std::env::var("SVCD_TARGET").unwrap_or(host_triple()?.to_string()),
    };

    let exec = build_from_recipe(&root, &manifest, recipe.as_ref(), &platform)?;
    plugin_tools::assert_payload(&root)?;

    let key = flag(args, "--key").map(PathBuf::from);
    let out = flag(args, "--out").map(PathBuf::from);
    let spk = pack(&root, &manifest, &platform, key.as_deref(), out.as_deref(), exec.as_deref())?;

    if !has(args, "--no-verify") {
        let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
        let _ = std::process::Command::new(&bin)
            .args(["verify", spk.to_str().unwrap_or_default(), "--json"])
            .status();
    }
    Ok((spk, manifest, key.is_some()))
}

fn check(args: &[String]) -> Result<(), String> {
    let file = positional(args, 0).ok_or_else(|| "check needs a <file.spk>".to_string())?;
    let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
    let status = std::process::Command::new(&bin)
        .args(["verify", file])
        .status()
        .map_err(|error| format!("{bin}: {error}"))?;
    if !status.success() {
        return Err(format!("verify exited {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}
