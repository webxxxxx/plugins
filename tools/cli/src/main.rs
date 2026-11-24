//! CLI entry for `plugin-tools`. See `lib.rs` for the four verbs.

use plugin_tools::{
    build_from_recipe, check_overlay_names, devkey, host_triple, instantiate, languages_for,
    merged_manifest, overlays, pack, parse_manifest, parse_meta, pascal, plugin_id_of, read_base,
    read_recipe, stage_package, sync, valid_name, vendor_sdk, write_json, write_project, Package,
    Project, Repo, TempStage, FAMILIES, PLATFORMS,
};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

const RESERVED: &[&str] = &[
    "svc",
    "svcd",
    "mcp",
    "plugin",
    "core",
    "system",
    "svcd-plugin",
];

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
    // A dropped option is how `--platform=x` once turned into "build all seven".
    warn_unknown_options(&args[1..]);
    let result = match verb {
        "template" => template(&args[1..]),
        "build" => build(&args[1..]),
        "dev" => dev(&args[1..]),
        "devkey" => devkey()
            .map(|path| println!("{}", path.display()))
            .map(|_| ()),
        "sync" => Repo::open(
            repo_spec(&args[1..]).as_deref(),
            has(&args[1..], "--refresh"),
        )
        .and_then(|repo| sync(&repo))
        .map(|count| println!("synced {count} plugin(s)")),
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

  template <name>   从 templates/<lang> 创建项目（先选平台，再选语言）
      --platforms <ids>   逗号分隔的目标三元组（非交互时必填）
      --lang <id>         可选项由平台决定：rust | c | shell | powershell | script
      --dir <path>        生成到哪（相对当前目录，默认 plugins）
      --template-repo <p> 模板仓库：本地目录或 git 地址（默认从 cwd 向上找）
      --refresh           重新 clone 模板仓库（默认复用缓存）
      --conf <file>       从 JSON 读下面的参数（命令行优先）
      --ui <kind>         清单里的 ui.type：headerless | web | gui
                            headerless = 不带页面，清单写 {{\"type\":\"headerless\"}}（没有 entry）
                            web / gui  = 带模板页面与 toolkit，entry 为 index.html
      --desc <text>       清单里的一句话描述
      --force / --no-force   覆盖已存在的目录

  语言的可选范围跟着平台走：同一 OS 家族（全 unix 或全 windows）时 shell / powershell
  都能用；跨家族时只有静态编译语言（rust / c）能一份清单管两个平台，或者用 script：
  它给 unix 出 POSIX 脚本、给 windows 出 PowerShell 脚本，各平台的清单指向自己那份。

  --conf 是一份 JSON，键都是可选的（ui 与清单的 ui.type 同值）：
      {{ \"dir\": \"plugins\",
        \"template_repo\": \"../plugins\",       // 或 \"git@github.com:me/x.git\"
        \"lang\": \"script\",
        \"platforms\": [\"x86_64-unknown-linux-musl\", \"x86_64-pc-windows-msvc\"],
        \"desc\": \"…\", \"ui\": \"web\", \"force\": false }}
  命令行给了的参数覆盖文件里的（`--ui headerless` 不带页面、`--no-force` 关掉 force）；
  模板仓库是 git 地址时按 URL 缓存在临时目录下，
  --refresh 强制重新 clone。PLUGIN_TOOLS_REPO 环境变量在没有 --template-repo 时生效。

  build [options]   运行 build.json，然后 plugin bundle
      --dir <path>        插件目录（默认从 cwd 向上找）
      --platform <t>      只打这一个三元组（本地开发用这个）
      --key <pem>         用这把钥签名；不给则产出未签名包
      --out <file>        产物路径（仅在只打一个平台时生效）
      --stage <dir>       把包写到 <dir>/package 并从那里打包（发布流水线打 tar 用）
      --no-verify         跳过 plugin verify

  不给 --platform 时按插件声明的 platforms 打；.plugin.json 没写 platforms 就打全部 7 个。
  产物名是 <id>-<version>-<platform>.spk：id 取 manifest.json 的 id，缺省用目录名；
  显示名（manifest 的 name）可以是中文，但只给人看，不进文件名。

  平台差异写在 platforms/ 里（与 package/ 同级，不进包）：<家族>.json（unix / windows）
  与 <三元组>.json 都按顶层键并入 package/manifest.json，顺序是先家族、后三元组，
  同名键整段替换（lifecycle 是一个键），null 表示删掉这个键，id 与 version 不能改。
  文件名只能是 7 个三元组之一或 unix / windows，别的名字直接报错（没有构建会读到它）。
  platforms/ 下的脚本按扩展名分家族（.sh → unix，.ps1/.cmd/.bat → windows），构建时只把
  本平台家族的那几份复制到 payload/bin/，别的平台的脚本根本不会进包；platforms/lib/ 下的
  文件（SDK 副本）按同一规则进 payload/lib/。要按平台换入口时，
  package/manifest.json 只留平台无关的字段（身份、元数据、ui、config、capabilities），
  入口写在家族覆盖里——同家族的三元组共用一份，只有真不一样时才另加三元组覆盖。
  合并在临时目录里的副本上完成，所以有覆盖的平台从副本打包；没有 platforms/ 的插件
  打原目录。--stage 把这个副本固定下来——发布流水线要打 tar 的就是它，且必须与
  --platform 一起用；副本里缺了清单指的文件会直接报错，不留到安装期。

  dev [options]     打包 + 安装 + 启动（首装不自启，这里代做）
  devkey            生成机器级开发钥（~/.svcd-dev）
  sync [options]    刷新各插件里 vendored 的 SDK 副本（只刷新已有的，不新增）
      --template-repo <p>  模板仓库（同 template，可给本地目录或 git 地址）
      --refresh            重新 clone
  check <file.spk>  plugin verify 包装"
    );
}

/// Every option the four verbs accept. Only used to warn about a typo: an option
/// nobody reads used to be dropped in silence, and `--platform=x` then built
/// every platform instead of the one that was asked for.
const KNOWN_FLAGS: &[&str] = &[
    "--platforms",
    "--lang",
    "--dir",
    "--template-repo",
    "--repo",
    "--refresh",
    "--conf",
    "--ui",
    "--desc",
    "--force",
    "--no-force",
    "--platform",
    "--key",
    "--out",
    "--stage",
    "--no-verify",
    "--help",
    "-h",
];

fn warn_unknown_options(args: &[String]) {
    for arg in args {
        if !arg.starts_with('-') {
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if KNOWN_FLAGS.contains(&name) {
            continue;
        }
        eprintln!("warn: ignoring unknown option {arg} (run plugin-tools with no arguments for the list)");
    }
}

/// `--flag value` or `--flag=value`: both spellings are what people type, and a
/// value that is silently dropped turns `--platform=x` into "build them all".
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    if let Some(value) = args.iter().find_map(|arg| arg.strip_prefix(&prefix)) {
        return Some(value);
    }
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).map(String::as_str)
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

/// Flags that consume the next argument. Anything else starting with `-` is a
/// boolean, so it must not swallow the positional that follows.
const VALUE_FLAGS: &[&str] = &[
    "--lang",
    "--platforms",
    "--desc",
    "--dir",
    "--key",
    "--out",
    "--platform",
    "--stage",
    "--repo",
    "--template-repo",
    "--conf",
    "--ui",
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

/// Print a numbered menu plus one line of instructions, so the prompt never
/// promises more than the code does (an empty answer means different things for
/// platforms and for languages).
fn menu(title: &str, items: &[&str], hint: &str) {
    println!("\n{title}");
    for (index, item) in items.iter().enumerate() {
        println!("  {:>2}) {item}", index + 1);
    }
    println!("  {hint}");
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

/// Defaults for `template`, so an agent or a CI step can pass one `--conf`
/// instead of repeating every flag. A flag always wins over the file.
///
/// JSON because the rest of the toolchain is JSON (`manifest.json`,
/// `.plugin.json`); unknown keys are refused so a typo is not silently ignored.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Conf {
    dir: Option<String>,
    template_repo: Option<String>,
    lang: Option<String>,
    platforms: Option<Vec<String>>,
    desc: Option<String>,
    /// `headerless` | `web` | `gui`, the same three values `--ui` takes.
    ui: Option<Ui>,
    force: Option<bool>,
}

fn read_conf(path: &str) -> Result<Conf, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("read --conf {path}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "parse --conf {path}: {error}\n  known keys: dir, template_repo, lang, platforms, desc, ui, force"
        )
    })
}

/// `--template-repo` (what the docs say) or `--repo` (kept working).
fn repo_spec(args: &[String]) -> Option<String> {
    flag(args, "--template-repo")
        .or_else(|| flag(args, "--repo"))
        .map(str::to_string)
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

    // The three values are manage's, and the old boolean flags are gone: say so
    // instead of silently ignoring an argument the caller still passes.
    for alias in ["--webui", "--no-ui", "--no-webui"] {
        if has(args, alias) {
            return Err(format!(
                "{alias} was removed: use --ui web (or --ui headerless); the value is one of headerless, web, gui"
            ));
        }
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    // A config file supplies defaults for everything below; a flag always wins.
    let conf = match flag(args, "--conf") {
        Some(path) => read_conf(path)?,
        None => Conf::default(),
    };

    // Platforms first: they decide which languages can even target them.
    let host = host_triple().ok();
    let platforms_flag = flag(args, "--platforms")
        .map(str::to_string)
        .or_else(|| conf.platforms.as_ref().map(|list| list.join(",")));
    let platforms: Vec<String> = match platforms_flag {
        Some(list) => list
            .split([',', '，', ' '])
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        None if interactive => {
            menu("选择目标平台：", PLATFORMS, "可多选，用逗号分隔；直接回车 = 本机三元组");
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
            // Same rule as `--lang` below: a run with no terminal never guesses.
            // Guessing the host triple is what makes an agent-built plugin ship
            // for the caller's machine instead of the platforms it asked for.
            return Err(format!(
                "--platforms is required when stdin is not a terminal (one of: {})",
                PLATFORMS.join(", ")
            ));
        }
    };
    for platform in &platforms {
        if !PLATFORMS.iter().any(|id| *id == platform) {
            return Err(format!(
                "unknown platform \"{platform}\" (see --help for the list)"
            ));
        }
    }
    if platforms.is_empty() {
        return Err("select at least one platform".into());
    }

    // Then the language, from what this platform set can actually run.
    let available = languages_for(&platforms);
    let language_flag = flag(args, "--lang")
        .map(str::to_string)
        .or_else(|| conf.lang.clone());
    let language = match language_flag {
        Some(language) => language,
        None if interactive => {
            menu(
                &format!(
                    "选择开发语言（按平台过滤：{}）",
                    available.join(" / ")
                ),
                &available,
                "单选，输入编号",
            );
            let text = ask("语言编号 > ")?;
            parse_selection(&text, &available, false)
                .and_then(|ids| ids.into_iter().next())
                .ok_or_else(|| "无效的语言编号".to_string())?
        }
        None => {
            return Err(format!(
                "--lang is required when stdin is not a terminal; this platform set allows: {}",
                available.join(", ")
            ))
        }
    };
    if !available.iter().any(|id| *id == language) {
        return Err(format!(
            "language \"{language}\" cannot target {} (this platform set allows: {})",
            platforms.join(", "),
            available.join(", ")
        ));
    }

    // `--ui headerless|web|gui` is the manifest's own `ui.type`: with a page the
    // object carries `entry: index.html`, without one it carries no entry.
    // Resolved before anything is written, so a bad value creates nothing.
    let ui = match flag(args, "--ui") {
        Some(text) => Ui::parse(text)?,
        None => conf.ui.unwrap_or(Ui::Headerless),
    };

    let repo = Repo::open(
        repo_spec(args)
            .or_else(|| conf.template_repo.clone())
            .as_deref(),
        has(args, "--refresh"),
    )?;
    let source = repo.template(&language);
    if !source.is_dir() {
        return Err(format!(
            "no template for --lang {language} in {}",
            repo.template_dir().display()
        ));
    }
    // `--dir` resolves against the caller's directory, never against the template
    // repo: that repo may be a clone in the temp cache, and `--refresh` deletes
    // the cache — a project created inside it would go with it.
    let dir = flag(args, "--dir")
        .map(str::to_string)
        .or_else(|| conf.dir.clone())
        .unwrap_or_else(|| "plugins".to_string());
    let dir = PathBuf::from(dir);
    let dir = if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir()
            .map_err(|error| format!("read the current directory: {error}"))?
            .join(dir)
    };
    let project = dir.join(name);
    // Precedence per key: a flag, then the file, then the built-in default.
    // Booleans need a negative flag, or a `true` in the file could never be
    // turned off for one run.
    let flag_on = |on: &str, off: &str, fallback: Option<bool>| {
        if has(args, off) {
            false
        } else if has(args, on) {
            true
        } else {
            fallback.unwrap_or(false)
        }
    };
    let force = flag_on("--force", "--no-force", conf.force);
    if project.exists() && !force {
        return Err(format!(
            "{} already exists (use --force to overwrite)",
            project.display()
        ));
    }

    let desc = flag(args, "--desc")
        .map(str::to_string)
        .or_else(|| conf.desc.clone())
        .unwrap_or_else(|| format!("{name} plugin"));
    instantiate(&source, &project, name, &pascal(name), &desc)?;
    vendor_sdk(&repo, &project, &language)?;
    apply_ui(&project, ui)?;
    apply_entry(&project, name, &language, &platforms)?;
    write_project(
        &project,
        &Project {
            name: name.to_string(),
            language: language.clone(),
            platforms: platforms.clone(),
        },
    )?;

    println!("\ncreated {}", project.display());
    println!("  ui:        {}", ui.kind());
    println!("  language:  {language}");
    println!("  platforms: {}", platforms.join(", "));
    println!("next: cd \"{}\" && plugin-tools build", project.display());
    Ok(())
}

/// `ui.type` in the package manifest: `web` / `gui` serve `payload/web/`,
/// `headerless` declares no page. Same three values manage accepts — the option
/// exists so the generator writes the manifest the host documents, not a second
/// vocabulary of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ui {
    /// No page. The manifest still says `{"type": "headerless"}`, with no entry.
    Headerless,
    /// A page for the browser.
    Web,
    /// The same files for the desktop WebView.
    Gui,
}

impl Ui {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "headerless" => Ok(Ui::Headerless),
            "web" => Ok(Ui::Web),
            "gui" => Ok(Ui::Gui),
            other => Err(format!(
                "--ui expects headerless, web or gui, got \"{other}\""
            )),
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Ui::Headerless => "headerless",
            Ui::Web => "web",
            Ui::Gui => "gui",
        }
    }

    /// The manifest object: an entry only where there is a page to open.
    fn manifest(self) -> serde_json::Value {
        match self {
            Ui::Headerless => serde_json::json!({ "type": self.kind() }),
            Ui::Web | Ui::Gui => {
                serde_json::json!({ "type": self.kind(), "entry": "index.html" })
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for Ui {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ui::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Put the page the caller asked for in place and write the matching `ui`.
///
/// `web` and `gui` ship the template's page plus the toolkit; only the declared
/// type differs, because manage serves `payload/web/` in both cases and the
/// desktop WebView loads the same files. `headerless` ships no page at all.
fn apply_ui(project: &Path, ui: Ui) -> Result<(), String> {
    let web = project.join("package").join("payload").join("web");
    if ui == Ui::Headerless {
        if web.is_dir() {
            std::fs::remove_dir_all(&web)
                .map_err(|error| format!("remove {}: {}", web.display(), error))?;
        }
    } else if !web.is_dir() {
        return Err("this template has no web ui to include".into());
    }
    edit_manifest(project, |manifest| manifest["ui"] = ui.manifest())
}

/// PowerShell's interpreter name differs by family (`powershell` on Windows,
/// `pwsh` elsewhere), and its `-File` flag has to be paired with the policy flag
/// only on Windows. A single manifest can hold one of them, so a set that spans
/// both families gets one `platforms/<triple>.json` per platform.
const POWERSHELL_ARGS: [&str; 4] = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"];
const PWSH_ARGS: [&str; 2] = ["-NoProfile", "-File"];

/// Where the entry lives, given the language and the platform set.
///
/// One family shares the manifest in `package/`. Two families cannot: the
/// interpreter (`sh` vs `powershell`, `pwsh` vs `powershell`) and often the
/// script itself differ, so the entry moves into `platforms/<triple>.json` for
/// every platform — and a `script` project also ships one script per family in
/// `platforms/`, which the build copies into `payload/bin/` for the matching
/// platform only.
fn apply_entry(
    project: &Path,
    name: &str,
    language: &str,
    platforms: &[String],
) -> Result<(), String> {
    let families = plugin_tools::families(platforms);
    let mixed = families.len() > 1;
    match language {
        "powershell" if !mixed => {
            let windows = families.first() == Some(&"windows");
            set_interpreter(project, windows)
        }
        "script" => {
            // The scripts live in `platforms/` (one per family) and are copied
            // into payload/bin/ by the build, so `package/` stays script-free.
            strip_entry(project)?;
            // One overlay per family, not per platform: every triple in a family
            // gets the same entry, so a five-platform project needs two files.
            for family in plugin_tools::families(platforms) {
                let windows = family == "windows";
                let script = format!("payload/bin/{name}.{}", if windows { "ps1" } else { "sh" });
                write_overlay(
                    project,
                    family,
                    &entry_manifest(
                        if windows { "powershell" } else { "sh" },
                        if windows { &POWERSHELL_ARGS } else { &[] },
                        &script,
                    ),
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn entry_manifest(name: &str, args: &[&str], exec: &str) -> serde_json::Value {
    let mut interpreter = serde_json::json!({ "name": name });
    if !args.is_empty() {
        interpreter["args"] = serde_json::json!(args);
    }
    serde_json::json!({
        "interpreter": interpreter,
        "exec": { "path": exec, "ready": "notify" }
    })
}

/// Read the generated `package/manifest.json`, hand it to `edit`, write it back.
///
/// Everything the generator does to a manifest is a small change to one key, so
/// the read/parse/write triple lives here instead of in each caller.
fn edit_manifest(
    project: &Path,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<(), String> {
    let manifest_path = project.join("package").join("manifest.json");
    let bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {}", manifest_path.display(), error))?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("parse manifest: {error}"))?;
    edit(&mut manifest);
    write_json(&manifest_path, &manifest)
}

/// Remove the entry from the shared manifest: every platform supplies its own.
fn strip_entry(project: &Path) -> Result<(), String> {
    edit_manifest(project, |manifest| {
        if let Some(object) = manifest.as_object_mut() {
            object.remove("exec");
            object.remove("interpreter");
            object.remove("lifecycle");
        }
    })
}

fn set_interpreter(project: &Path, windows: bool) -> Result<(), String> {
    edit_manifest(project, |manifest| {
        manifest["interpreter"] = if windows {
            serde_json::json!({ "name": "powershell", "args": POWERSHELL_ARGS })
        } else {
            serde_json::json!({ "name": "pwsh", "args": PWSH_ARGS })
        };
    })
}

fn write_overlay(project: &Path, platform: &str, manifest: &serde_json::Value) -> Result<(), String> {
    let dir = project.join("platforms");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("create {}: {}", dir.display(), error))?;
    write_json(&dir.join(format!("{platform}.json")), manifest)
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
            return Err(
                "no package/manifest.json found; run inside a plugin directory or pass --dir"
                    .into(),
            );
        }
    }
}

/// What one `build`/`dev` run produced.
struct Packed {
    spks: Vec<PathBuf>,
    id: String,
    signed: bool,
}

fn build(args: &[String]) -> Result<(), String> {
    let packed = pack_plugin(args)?;
    for spk in &packed.spks {
        println!(
            "\npacked {}{}",
            spk.display(),
            if packed.signed { "" } else { "  (unsigned)" }
        );
    }
    if !packed.signed {
        println!("host needs: allow_insecure_install = on   (restart svcd after changing it)");
    }
    println!(
        "next: plugin install \"{}\" && plugin start {}",
        packed.spks[0].display(),
        packed.id
    );
    Ok(())
}

/// An overlay no build touches is a file that silently never ships.
fn warn_unused_overlays(root: &Path, platforms: &[String]) {
    let Ok(found) = overlays(root) else {
        return;
    };
    for name in found {
        let used = if FAMILIES.contains(&name.as_str()) {
            platforms
                .iter()
                .any(|platform| plugin_tools::family(platform) == name)
        } else {
            platforms.iter().any(|platform| platform == &name)
        };
        if !used {
            eprintln!("warn: platforms/{name}.json: nothing in this build uses it");
        }
    }
}

/// Pack the plugin and install + start it: the local development loop.
fn dev(args: &[String]) -> Result<(), String> {
    let packed = pack_plugin(args)?;
    let host = host_triple().ok();
    let spk = host
        .and_then(|triple| {
            packed.spks.iter().find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.contains(triple))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(&packed.spks[0]);
    let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
    for verb in ["install", "start"] {
        // The host's commands take the id: the display name is not an identity
        // and may even be Chinese.
        let target = if verb == "install" {
            spk.display().to_string()
        } else {
            packed.id.clone()
        };
        let status = std::process::Command::new(&bin)
            .args([verb, target.as_str()])
            .status()
            .map_err(|error| format!("{bin}: {error}"))?;
        if !status.success() {
            return Err(format!(
                "{bin} {verb} exited {}",
                status.code().unwrap_or(-1)
            ));
        }
    }
    println!("started {}", packed.id);
    Ok(())
}

fn pack_plugin(args: &[String]) -> Result<Packed, String> {
    let root = find_plugin_root(flag(args, "--dir"))?;
    // The base manifest describes the plugin; the entry is whatever this
    // platform's overlays make of it. Only the platform-neutral part — the id
    // and the version that names the artifact — is read before a platform is
    // chosen, which is why an overlay may not change either.
    let base = read_base(&root)?;
    let meta = parse_meta(&base)?;
    let id = plugin_id_of(&root, meta.id.as_deref())?;
    let recipe = read_recipe(&root)?;
    let platforms = plugin_tools::resolve_platforms(&root, flag(args, "--platform"))?;
    let key = flag(args, "--key").map(PathBuf::from);
    let out = flag(args, "--out").map(PathBuf::from);
    let stage_root = flag(args, "--stage").map(PathBuf::from);
    if stage_root.is_some() && platforms.len() > 1 {
        return Err("--stage holds one package: pass --platform <triple> along with it".into());
    }
    check_overlay_names(&root)?;
    if flag(args, "--platform").is_none() {
        warn_unused_overlays(&root, &platforms);
    }

    let mut spks = Vec::new();
    // `platforms/` may hold the per-family entry scripts and their SDKs, and those
    // only exist in the package once a staged copy has chosen the matching ones.
    let has_scripts = !plugin_tools::platform_scripts(&root)?.is_empty()
        || !plugin_tools::platform_libs(&root)?.is_empty();
    if platforms.len() > 1 {
        if let Some(spec) = recipe.as_ref().and_then(|recipe| recipe.build.as_ref()) {
            let mentions = spec.artifact.contains("{platform}")
                || spec.command.iter().any(|part| part.contains("{platform}"));
            if !mentions {
                eprintln!(
                    "warn: build.json never mentions {{platform}}; the same artifact will be packaged for {} platforms (pass --platform <本机三元组> for a local build)",
                    platforms.len()
                );
            }
        }
    }
    for platform in &platforms {
        let merged = merged_manifest(&root, &base, platform)?;
        let view = merged.as_ref().unwrap_or(&base);
        let manifest = parse_manifest(view, &format!("manifest for {platform}"))?;
        plugin_tools::assert_icons(&root, &manifest)?;
        // The recipe decides how to reach a platform; `{platform}` is available to
        // it, so `cargo build --target {platform}` is a one-line change.
        let exec = build_from_recipe(&root, &manifest, &id, recipe.as_ref(), platform)?;
        // A platform overlay replaces `exec` wholesale, so the recipe's rename —
        // which only exists to match the base exec name — no longer applies.
        let exec = if base.get("exec") == view.get("exec") {
            exec
        } else {
            None
        };
        // A merged manifest has to exist as a file before the host reads the
        // package, while the authored one must stay the base for the other
        // platforms. So a platform whose manifest differs — or one that brings
        // its own scripts, or any platform the caller wants as a tarball-ready
        // tree — bundles a staged copy.
        let temp;
        let package_dir = if merged.is_some() || stage_root.is_some() || has_scripts {
            let stage = match &stage_root {
                Some(dir) => dir.clone(),
                None => {
                    temp = TempStage::new()?;
                    temp.path().to_path_buf()
                }
            };
            let staged = stage_package(&root, &stage, merged.as_ref(), platform, exec.as_deref())?;
            println!("stage: {}", staged.display());
            staged
        } else {
            root.join("package")
        };
        // Measured on the package that goes to the packer: a `script` project's
        // payload only exists once its family's half has been copied in.
        plugin_tools::assert_payload(&package_dir)?;
        let package = Package {
            id: &id,
            version: &manifest.version,
            platform,
        };
        let spk = pack(
            &root,
            &package_dir,
            &package,
            key.as_deref(),
            if platforms.len() == 1 {
                out.as_deref()
            } else {
                None
            },
            exec.as_deref(),
        )?;
        if !has(args, "--no-verify") {
            let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
            let _ = std::process::Command::new(&bin)
                .args(["verify", spk.to_str().unwrap_or_default(), "--json"])
                .status();
        }
        spks.push(spk);
    }
    Ok(Packed {
        spks,
        id,
        signed: key.is_some(),
    })
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
