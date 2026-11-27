//! `plugin-tools` — the toolchain that used to be Node scripts.
//!
//! Four verbs, no runtime dependency beyond this binary:
//!
//! * `template <name>` — create a project from `templates/<lang>`.
//! * `build` — run the template's `build.json`, then `plugin bundle`.
//! * `devkey` — generate the machine-local Ed25519 signing key.
//! * `sync` — refresh the vendored SDK files inside every plugin.
//!
//! The template is data (`build.json` + a directory tree), never code here: the
//! driver knows no language, it only runs the command a recipe names.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Directories that never travel from a template into a project.
const SKIP_DIRS: &[&str] = &[
    "target", "dist", "out", "node_modules", ".git", ".idea", ".vscode",
];

pub const LANGUAGES: &[&str] = &["rust", "c", "shell", "powershell", "script"];

// 目标三元组取自 plugin-manager 的 docs/core-selection-spec.md §7。
pub const PLATFORMS: &[&str] = &[
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "armv7-unknown-linux-musleabihf",
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// Where `templates/` and `sdk/` come from: a local checkout, or a git
/// repository cloned once into a cache directory.
pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    /// Open the template source.
    ///
    /// `spec` is a local path or a git URL (`--template-repo`). Without one,
    /// `PLUGIN_TOOLS_REPO` is consulted, and finally the checkout found by
    /// walking up from the current directory.
    pub fn open(spec: Option<&str>, refresh: bool) -> Result<Self, String> {
        if let Some(spec) = spec.map(str::trim).filter(|value| !value.is_empty()) {
            return Self::from_spec(spec, refresh);
        }
        match std::env::var("PLUGIN_TOOLS_REPO") {
            Ok(value) if !value.trim().is_empty() => Self::from_spec(value.trim(), refresh),
            _ => Self::walk_up(),
        }
    }

    fn from_spec(spec: &str, refresh: bool) -> Result<Self, String> {
        if is_git_spec(spec) {
            return Self::clone(spec, refresh);
        }
        let root = PathBuf::from(spec);
        check_repo_layout(&root)?;
        Ok(Repo { root })
    }

    fn walk_up() -> Result<Self, String> {
        let mut current = std::env::current_dir().map_err(|error| error.to_string())?;
        loop {
            if current.join("templates").is_dir() && current.join("sdk").is_dir() {
                return Ok(Repo { root: current });
            }
            if !current.pop() {
                return Err(
                    "cannot find the plugin repository (templates/ + sdk/); pass --template-repo <path|git url>"
                        .into(),
                );
            }
        }
    }

    /// Clone once per URL and reuse it: a template run should not pay for a
    /// network round trip, and `--refresh` is how a stale cache is dropped.
    fn clone(spec: &str, refresh: bool) -> Result<Self, String> {
        let cache = repo_cache(spec);
        if refresh {
            let _ = fs::remove_dir_all(&cache);
        }
        if !(cache.join("templates").is_dir() && cache.join("sdk").is_dir()) {
            let _ = fs::remove_dir_all(&cache);
            if let Some(parent) = cache.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {}", parent.display(), error))?;
            }
            println!("$ git clone --depth 1 {spec} {}", cache.display());
            let status = Command::new("git")
                .args(["clone", "--depth", "1", spec])
                .arg(&cache)
                .status()
                .map_err(|error| {
                    format!("git clone {spec}: {error} (a repository spec needs git on PATH)")
                })?;
            if !status.success() {
                return Err(format!("git clone {spec} exited {}", status.code().unwrap_or(-1)));
            }
            check_repo_layout(&cache)?;
        }
        Ok(Repo { root: cache })
    }

    pub fn template(&self, language: &str) -> PathBuf {
        self.root.join("templates").join(language)
    }

    pub fn template_dir(&self) -> PathBuf {
        self.root.join("templates")
    }
}

/// A git URL (`https://…`, `ssh://…`, `git@host:…`) or a path ending in `.git`.
///
/// Everything else is a local directory, which keeps `--template-repo .` honest
/// in a checkout whose path happens to contain a colon.
pub fn is_git_spec(spec: &str) -> bool {
    spec.starts_with("git@") || spec.contains("://") || spec.ends_with(".git")
}

fn check_repo_layout(root: &Path) -> Result<(), String> {
    if root.join("templates").is_dir() && root.join("sdk").is_dir() {
        return Ok(());
    }
    Err(format!(
        "{}: not a template repository (it needs templates/ and sdk/)",
        root.display()
    ))
}

/// `/tmp/plugin-tools/templates/<hash>-<slug>`: one clone per URL, stable across
/// runs, and readable enough to find by hand.
fn repo_cache(spec: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in spec.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let slug: String = spec
        .chars()
        .map(|value| if value.is_ascii_alphanumeric() { value } else { '-' })
        .collect();
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_matches('-');
    std::env::temp_dir()
        .join("plugin-tools")
        .join("templates")
        .join(format!("{hash:016x}-{slug}"))
}

/// The fields of `package/manifest.json` this toolchain needs.
///
/// `name` is deliberately absent: it is the display name (any UTF-8) and only
/// the host's bundler consumes it. The identity is `id`, which falls back to the
/// project directory name; `icons` is checked here because a missing icon file
/// is a packaging error, not something to discover at install time.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub exec: Exec,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
    #[serde(default)]
    pub compat: Option<Compat>,
}

/// Device compatibility ids (`compat.devices`); empty/absent means universal.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Compat {
    #[serde(default)]
    pub devices: Vec<String>,
}

impl Manifest {
    /// The device the artifact name carries: the first declared compat id.
    ///
    /// The signed manifest keeps the whole list; the host's bundler only appends
    /// the first id to the file name because it is a human hint, not the
    /// authority. The toolchain follows the same rule so a local build does not
    /// silently produce a universal-looking artifact for a device-restricted
    /// package.
    pub fn device(&self) -> Option<&str> {
        self.compat
            .as_ref()
            .and_then(|compat| compat.devices.first())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Exec {
    pub path: String,
}

/// The icon formats a package may declare (`icons: {png?, ico?, icns?}`).
const ICON_FORMATS: &[&str] = &["png", "ico", "icns"];

/// The id the host keeps for its own service registry.
const RESERVED_IDS: &[&str] = &["svc"];

pub fn read_manifest(root: &Path) -> Result<Manifest, String> {
    let path = root.join("package").join("manifest.json");
    let value = read_json(&path)?;
    serde_json::from_value(value).map_err(|error| format!("parse {}: {}", path.display(), error))
}

/// A JSON file, with the path in both the read and the parse error.
fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {}", path.display(), error))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {}", path.display(), error))
}

/// The same, refusing anything but an object. `describe` says what a non-object
/// means for this particular file.
fn read_object(path: &Path, describe: &str) -> Result<Value, String> {
    let value = read_json(path)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(format!("{}: {describe}", path.display()))
    }
}

/// Write a JSON file the way the repo writes them: pretty, and ending in a newline.
pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("write {}: {}", path.display(), error))?;
    text.push('\n');
    fs::write(path, text).map_err(|error| format!("write {}: {}", path.display(), error))
}

/// `package/manifest.json` as authored, before any platform overlay is applied.
///
/// A plugin whose entry differs per platform keeps only what every platform
/// shares here; the entry itself lives in `platforms/`. See [`merged_manifest`].
pub fn read_base(root: &Path) -> Result<Value, String> {
    read_object(
        &root.join("package").join("manifest.json"),
        "expected a JSON object",
    )
}

/// The manifest fields the toolchain needs *before* it picks a platform: the
/// version (it names the artifact) and the identity (it names everything else).
///
/// `version` therefore cannot come from an overlay, which is why
/// [`merge_manifest`] refuses to change it.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub version: String,
    #[serde(default)]
    pub id: Option<String>,
}

pub fn parse_meta(base: &Value) -> Result<Meta, String> {
    serde_json::from_value(base.clone()).map_err(|error| format!("package/manifest.json: {error}"))
}

/// The manifest one platform actually ships, once `platforms/` has had its say.
///
/// Layers apply in order: `platforms/<family>.json` (every triple in that family
/// shares it), then `platforms/<triple>.json` (only where this triple differs
/// from its family). Each layer is the same flat object merged over the previous
/// one, so there is one set of rules — top-level keys replace, `null` deletes —
/// however many layers a plugin uses.
///
/// `None` means no layer changed anything for this platform, so `package/` is
/// packaged as it is.
pub fn merged_manifest(
    root: &Path,
    base: &Value,
    platform: &str,
) -> Result<Option<Value>, String> {
    let mut merged = base.clone();
    let mut changed = false;
    for scope in [family(platform), platform] {
        let Some(own) = read_overlay(root, scope)? else {
            continue;
        };
        merged = merge_manifest(&merged, &own)?;
        changed = true;
    }
    Ok(changed.then_some(merged))
}

/// Refuse an overlay named after neither a target triple nor a family: a file
/// nothing would ever read is a typo, not a spare.
pub fn check_overlay_names(root: &Path) -> Result<(), String> {
    let mut known: Vec<&str> = PLATFORMS.to_vec();
    known.extend_from_slice(FAMILIES);
    for name in overlays(root)? {
        if !known.contains(&name.as_str()) {
            return Err(format!(
                "platforms/{name}.json: not a target triple or a family; use one of {}",
                known.join(", ")
            ));
        }
    }
    Ok(())
}

/// Parse a manifest view into the fields the toolchain builds with.
pub fn parse_manifest(value: &Value, label: &str) -> Result<Manifest, String> {
    if value.get("exec").and_then(|exec| exec.get("path")).is_none() {
        return Err(format!(
            "{label}: no exec.path — put the entry in platforms/<family>.json (unix / windows), in platforms/<triple>.json, or write it in package/manifest.json"
        ));
    }
    serde_json::from_value(value.clone()).map_err(|error| format!("{label}: {error}"))
}

/// The plugin's identity: manifest `id`, else the project directory name.
///
/// Same rule the host's bundler applies. The id ends up in directory names, log
/// file names, `<id>.<capability>` tool names and `/plugin-ui/<id>/`, so it must
/// stay ASCII; the display name (`name`) is what may be Chinese.
pub fn plugin_id(root: &Path, manifest: &Manifest) -> Result<String, String> {
    plugin_id_of(root, manifest.id.as_deref())
}

/// [`plugin_id`] for a manifest that has not been parsed into a [`Manifest`]
/// yet — the identity is platform-neutral, so it is read from the base.
pub fn plugin_id_of(root: &Path, id: Option<&str>) -> Result<String, String> {
    let id = match id {
        Some(id) => id.to_string(),
        None => root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
    };
    if !valid_name(&id) {
        return Err(format!(
            "invalid plugin id \"{id}\": expected [a-z0-9][a-z0-9_-]{{0,63}} in manifest.json's \"id\" (or in the project directory name)"
        ));
    }
    if RESERVED_IDS.contains(&id.as_str()) {
        return Err(format!(
            "id \"{id}\" is reserved: the host keeps it for its own service registry"
        ));
    }
    Ok(id)
}

/// Fail before the bundler does when a declared icon is not a real package file.
pub fn assert_icons(root: &Path, manifest: &Manifest) -> Result<(), String> {
    for (format, path) in &manifest.icons {
        if !ICON_FORMATS.contains(&format.as_str()) {
            return Err(format!(
                "manifest.json: icons.{format} is not a supported format ({}); got \"{path}\"",
                ICON_FORMATS.join(", ")
            ));
        }
        let file = root.join("package").join(path);
        let metadata = fs::metadata(&file)
            .map_err(|error| format!("icons.{format}: {} is missing ({error})", path))?;
        if !metadata.is_file() {
            return Err(format!("icons.{format}: {} is not a file", path));
        }
        if metadata.len() == 0 {
            return Err(format!("icons.{format}: {} is empty", path));
        }
    }
    Ok(())
}

/// `build.json` beside `package/`; absent means the payload is already source.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Recipe {
    #[serde(default)]
    pub build: Option<BuildSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildSpec {
    pub command: Vec<String>,
    pub artifact: String,
    #[serde(default)]
    pub env: HashMap<String, HashMap<String, String>>,
}

pub fn read_recipe(root: &Path) -> Result<Option<Recipe>, String> {
    let path = root.join("build.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {}", path.display(), error))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("parse {}: {}", path.display(), error))
}

/// Expand `{name}`, `{version}` and `{platform}` in a recipe string.
///
/// `{name}` is the plugin id (see [`plugin_id`]): recipes name files, and the
/// display name may be Chinese.
pub fn expand(value: &str, id: &str, version: &str, platform: &str) -> String {
    expand_unit(value, id, version, platform, "")
}

/// Same placeholders plus `{device}`, for a recipe that has to tell the
/// universal build from the device variant (which binary to copy).
pub fn expand_unit(value: &str, id: &str, version: &str, platform: &str, device: &str) -> String {
    value
        .replace("{name}", id)
        .replace("{version}", version)
        .replace("{platform}", platform)
        .replace("{device}", device)
}

/// `{cc}` is the C compiler a recipe runs: `$CC` when the build sets one,
/// `gcc` otherwise.
///
/// This is how one `build.json` serves both a native build and a cross one —
/// the command array cannot vary by platform, but the compiler can come from
/// the environment (`tools/cross/install.sh` points `CC` at the musl toolchain).
pub fn compiler(variable: Option<&str>) -> String {
    match variable.map(str::trim) {
        Some(cc) if !cc.is_empty() => cc.to_string(),
        _ => "gcc".to_string(),
    }
}

/// Run the recipe and copy the artifact to `payload/bin/`.
///
/// Returns the exec path the packaged manifest should use — on Windows the
/// artifact keeps its `.exe` suffix, and `CreateProcess` does not guess it.
///
/// `{name}` in the recipe expands to the plugin id, never to the display name:
/// it names files, and a display name may be Chinese.
pub fn build_from_recipe(
    root: &Path,
    manifest: &Manifest,
    id: &str,
    recipe: Option<&Recipe>,
    platform: &str,
    device: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(spec) = recipe.and_then(|recipe| recipe.build.as_ref()) else {
        return Ok(None);
    };
    if spec.command.is_empty() {
        return Err("build.json: build.command must be a non-empty array".into());
    }
    if spec.artifact.trim().is_empty() {
        return Err("build.json: build.artifact is required".into());
    }

    // `--device` is the unit's device; `SVCD_DEVICE` carries the same value so a
    // recipe does not have to receive it as a positional argument.
    let device = device.unwrap_or_default();
    let artifact = expand_unit(&spec.artifact, id, &manifest.version, platform, device);
    let artifact_path = root.join(&artifact);
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }
    // Clear what the previous platform produced under the same name. Windows
    // compilers add `.exe`, so a native build and a cross build of one plugin
    // leave two candidates side by side — and the exact-path lookup below would
    // happily package the other platform's binary.
    let _ = fs::remove_file(&artifact_path);
    if let Some(name) = artifact_path.file_name().and_then(|name| name.to_str()) {
        let _ = fs::remove_file(artifact_path.with_file_name(stale_twin(name)));
    }

    let cc = compiler(std::env::var("CC").ok().as_deref());
    let program =
        expand_unit(&spec.command[0], id, &manifest.version, platform, device).replace("{cc}", &cc);
    let args: Vec<String> = spec.command[1..]
        .iter()
        .map(|part| expand_unit(part, id, &manifest.version, platform, device).replace("{cc}", &cc))
        .collect();

    let mut command = Command::new(&program);
    command.args(&args).current_dir(root);
    command.env("SVCD_DEVICE", device);
    for layer in [spec.env.get("*"), spec.env.get(platform)].into_iter().flatten() {
        for (key, value) in layer {
            command.env(
                key,
                expand_unit(value, id, &manifest.version, platform, device),
            );
        }
    }
    println!("$ {program} {}", args.join(" "));
    let status = command
        .status()
        .map_err(|error| format!("{program}: {error}"))?;
    if !status.success() {
        return Err(format!("{program} exited {}", status.code().unwrap_or(-1)));
    }

    let built = [artifact_path.clone(), root.join(format!("{artifact}.exe"))]
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| format!("build.json: no artifact at {artifact} (or {artifact}.exe)"))?;

    let built_name = built
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let exec_base = Path::new(&manifest.exec.path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "manifest.exec.path is missing".to_string())?
        .to_string();

    // The copy target is the artifact's own name. Only when the artifact *is*
    // the exec binary does the `.exe` rewrite apply; an extra binary (the daemon
    // a wrapper runs) keeps its name and must not touch `exec.path`.
    let (final_name, renamed) = if built_name.eq_ignore_ascii_case(&exec_base) {
        (built_name.clone(), false)
    } else if built_name.to_lowercase() == format!("{}.exe", exec_base.to_lowercase()) {
        (format!("{exec_base}.exe"), true)
    } else {
        (built_name.clone(), false)
    };

    let target = root.join("package").join("payload").join("bin").join(&final_name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }
    // One plugin, several platforms: the previous platform's binary would
    // otherwise stay beside this one and the packer takes everything under
    // package/.
    let stale = target.with_file_name(stale_twin(&final_name));
    if stale != target {
        let _ = fs::remove_file(&stale);
    }
    fs::copy(&built, &target)
        .map_err(|error| format!("copy {}: {}", built.display(), error))?;
    // Only rewrite `exec.path` when the artifact was renamed to carry the
    // manifest's name plus `.exe`.
    if renamed {
        Ok(Some(format!("payload/bin/{final_name}")))
    } else {
        Ok(None)
    }
}

/// The same binary under the other platform's name: `demo` ↔ `demo.exe`.
///
/// A payload directory is shared by every platform a plugin builds for, so the
/// build that follows must remove the name the previous one left behind.
fn stale_twin(name: &str) -> String {
    match name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
    {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => format!("{name}.exe"),
    }
}

/// Fail early when the package about to be bundled would ship nothing.
///
/// Measured on the package that actually goes to the packer: a `script` project
/// keeps its entries and SDKs outside `package/`, so its own payload can be empty
/// until [`stage_package`] has picked this platform's half.
pub fn assert_payload(package_dir: &Path) -> Result<(), String> {
    let payload = package_dir.join("payload");
    if !payload.is_dir() || count_files(&payload)? == 0 {
        return Err(format!(
            "{} is empty: build first or add the files the plugin ships",
            payload.display()
        ));
    }
    Ok(())
}

fn count_files(dir: &Path) -> Result<usize, String> {
    let mut count = 0;
    let entries = fs::read_dir(dir).map_err(|error| format!("read {}: {}", dir.display(), error))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_files(&path)?;
        } else {
            count += 1;
        }
    }
    Ok(count)
}

/// Per-platform manifest overlay: `platforms/<triple>.json`, beside `package/`.
///
/// One `package/manifest.json` serves every platform a plugin declares, but the
/// keys that name an entry point exist once: `exec`, `lifecycle` and
/// `interpreter`. A shell script needs `sh`, a PowerShell script needs
/// `powershell`, and no single value is both. So the entry moves into an
/// overlay, and the base manifest keeps only what all platforms share.
///
/// An overlay may be named after a target triple or after a family
/// (`unix` / `windows`): the family one applies to every triple in it, so the
/// platforms that differ in nothing do not need a file each. See
/// [`merged_manifest`] for the order.
///
/// The directory sits outside `package/` on purpose: the bundler ships
/// everything under `package/`, so an overlay stored there would be payload.
/// It may also hold the entry scripts themselves — see [`platform_scripts`].
pub fn overlay_path(root: &Path, platform: &str) -> PathBuf {
    root.join("platforms").join(format!("{platform}.json"))
}

/// The names an overlay may have besides a target triple.
pub const FAMILIES: &[&str] = &["unix", "windows"];

/// The OS family a target triple belongs to. A shell script cannot run on
/// Windows, and a PowerShell script needs a different interpreter name there.
pub fn family(platform: &str) -> &'static str {
    if platform.contains("-windows-") {
        "windows"
    } else {
        "unix"
    }
}

/// The families this platform set spans, `unix` first.
pub fn families(platforms: &[String]) -> Vec<&'static str> {
    let mut found = Vec::new();
    for platform in platforms {
        let family = family(platform);
        if !found.contains(&family) {
            found.push(family);
        }
    }
    found.sort_unstable();
    found
}

/// The languages that can target this platform set.
///
/// `rust` / `c` build one binary per platform, so they always work. `shell`
/// cannot run on Windows at all, and `powershell` runs on both families only as
/// two different interpreters (`pwsh` on Unix, `powershell` on Windows) — which
/// a single manifest cannot name, so it is offered per family only. A set that
/// spans both families is the case `script` exists for: it scaffolds a POSIX
/// script for Unix and a PowerShell script for Windows, with one manifest per
/// platform picking the right pair.
pub fn languages_for(platforms: &[String]) -> Vec<&'static str> {
    let families = families(platforms);
    let mut available = vec!["rust", "c"];
    if families.len() < 2 {
        if families.first() != Some(&"windows") {
            available.push("shell");
        }
        available.push("powershell");
    } else {
        available.push("script");
    }
    available
}


/// The overlay a plugin declares for one name — a triple or a family.
pub fn read_overlay(root: &Path, platform: &str) -> Result<Option<Value>, String> {
    let path = overlay_path(root, platform);
    if !path.is_file() {
        return Ok(None);
    }
    let value = read_object(
        &path,
        "an overlay is a JSON object holding the keys that differ for that family or platform",
    )?;
    Ok(Some(value))
}

/// The names `platforms/` holds an overlay for: target triples and families.
pub fn overlays(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("platforms");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("read {}: {}", dir.display(), error))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {}", dir.display(), error))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(triple) = name.strip_suffix(".json") {
            found.push(triple.to_string());
        }
    }
    found.sort();
    Ok(found)
}

/// Merge one overlay into the base manifest.
///
/// Top-level keys replace wholesale — `lifecycle` is one key, so restate every
/// hook the platform needs — and `null` removes a key. `id` and `version` are
/// refused: they name the artifact and the release, so they cannot differ per
/// platform.
pub fn merge_manifest(base: &Value, overlay: &Value) -> Result<Value, String> {
    let Some(base) = base.as_object() else {
        return Err("package/manifest.json: expected a JSON object".into());
    };
    let Some(patch) = overlay.as_object() else {
        return Err("platforms overlay: expected a JSON object".into());
    };
    let mut merged = base.clone();
    for (key, value) in patch {
        if matches!(key.as_str(), "id" | "version") {
            return Err(format!(
                "platforms overlay must not change \"{key}\": it names the artifact and the release"
            ));
        }
        if value.is_null() {
            merged.remove(key);
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(merged))
}

/// A script's OS family, by extension: what `payload/bin/` would get.
fn script_family(name: &str) -> Option<&'static str> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "sh" => Some("unix"),
        "ps1" | "psm1" | "cmd" | "bat" => Some("windows"),
        _ => None,
    }
}

/// The files directly in `dir`, as names. Empty when the directory is absent.
fn files_in(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir).map_err(|error| format!("read {}: {}", dir.display(), error))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {}", dir.display(), error))?;
        if entry.path().is_file() {
            found.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    found.sort();
    Ok(found)
}

/// The script files `platforms/` holds, whatever family they belong to.
///
/// They are the per-platform half of a cross-platform script plugin: only the
/// matching family travels (see [`materialize_scripts`]), so the other one never
/// enters a package.
pub fn platform_scripts(root: &Path) -> Result<Vec<String>, String> {
    Ok(files_in(&root.join("platforms"))?
        .into_iter()
        .filter(|name| script_family(name).is_some())
        .collect())
}

/// The shared libraries `platforms/lib/` holds, one family's worth each.
///
/// A script that lives outside `package/` needs its SDK to travel the same way:
/// `platforms/lib/svcd-plugin.sh` ends up in `payload/lib/` for unix platforms
/// only, and the PowerShell module for Windows only. A file with no script
/// extension belongs to no family, so it is refused here rather than silently
/// dropped or silently shipped to everyone.
pub fn platform_libs(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("platforms").join("lib");
    let mut found = Vec::new();
    for name in files_in(&dir)? {
        if script_family(&name).is_none() {
            return Err(format!(
                "platforms/lib/{name}: expected a .sh / .ps1 (that extension is what picks the family)"
            ));
        }
        found.push(name);
    }
    Ok(found)
}

fn materialize_scripts(
    root: &Path,
    package_dir: &Path,
    platform: &str,
) -> Result<Vec<String>, String> {
    let wanted = family(platform);
    let mut copied = Vec::new();
    // Entry scripts go to `payload/bin/`, shared libraries next to them in
    // `payload/lib/`; both are picked by family, so neither the other family's
    // script nor its SDK ever enters this package.
    for (source_dir, target_dir, names) in [
        ("platforms", "bin", platform_scripts(root)?),
        ("platforms/lib", "lib", platform_libs(root)?),
    ] {
        for name in names {
            if script_family(&name) != Some(wanted) {
                continue;
            }
            let target = package_dir.join("payload").join(target_dir);
            fs::create_dir_all(&target)
                .map_err(|error| format!("create {}: {}", target.display(), error))?;
            let source = root.join(source_dir).join(&name);
            let copy = target.join(&name);
            fs::copy(&source, &copy)
                .map_err(|error| format!("copy {}: {}", source.display(), error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&copy, fs::Permissions::from_mode(0o755));
            }
            copied.push(format!("payload/{target_dir}/{name}"));
        }
    }
    Ok(copied)
}

/// Every payload path a manifest names: `exec`, the lifecycle hooks, and the
/// `paths` of any capability that declares one.
fn manifest_paths(manifest: &Value) -> Vec<(String, String)> {
    fn walk(label: &str, value: &Value, found: &mut Vec<(String, String)>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let label = if label.is_empty() {
                        key.clone()
                    } else {
                        format!("{label}.{key}")
                    };
                    walk(&label, child, found);
                }
            }
            // Arrays hold as many payload paths as objects do: `capabilities[]`
            // is one array per capability, and `capabilities[].hook.path` is a
            // file like any other. Missing this arm made the stage check blind to
            // capability hooks while its own doc comment promised otherwise.
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    let label = if label.is_empty() {
                        index.to_string()
                    } else {
                        format!("{label}[{index}]")
                    };
                    walk(&label, child, found);
                }
            }
            Value::String(path) if value_is_payload_path(path) => {
                found.push((label.to_string(), path.clone()));
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk("", manifest, &mut found);
    found
}

fn value_is_payload_path(value: &str) -> bool {
    value.starts_with("payload/")
}

/// Copy `package/` to `stage_root/package`, add this platform's scripts from
/// `platforms/`, and write `manifest` over the bundled one.
///
/// The host reads a manifest from the filesystem, so a merged one has to exist
/// as a file — while the authored one must stay the base for every *other*
/// platform. A platform whose manifest differs therefore bundles a staged copy.
///
/// `exec` is the entry the build actually produced, when that differs from the
/// declared one (Windows adds `.exe`). It is written into the staged manifest so
/// the tarball describes the artifact inside it: the release pipeline bundles the
/// tarball later, without the `--exec` this build passes to the packer.
pub fn stage_package(
    root: &Path,
    stage_root: &Path,
    manifest: Option<&Value>,
    platform: &str,
    exec: Option<&str>,
) -> Result<PathBuf, String> {
    let source = root.join("package");
    let target = stage_root.join("package");
    if target.exists() {
        fs::remove_dir_all(&target)
            .map_err(|error| format!("clear {}: {}", target.display(), error))?;
    }
    copy_tree(&source, &target)?;
    let copied = materialize_scripts(root, &target, platform)?;
    let mut view = match manifest {
        Some(merged) => merged.clone(),
        // Nothing was merged, so the manifest already in `package/` is the one this
        // platform ships — and it is just as capable of naming a file the stage
        // does not hold.
        None => read_json(&target.join("manifest.json"))?,
    };
    if let Some(exec) = exec {
        // No `exec` to correct means the build produced something the manifest
        // never names; leave the file alone rather than invent one.
        if let Some(object) = view.get_mut("exec").and_then(Value::as_object_mut) {
            object.insert("path".to_string(), Value::String(exec.to_string()));
        }
    }
    if manifest.is_some() || exec.is_some() {
        write_json(&target.join("manifest.json"), &view)?;
    }
    // A package whose manifest points at a file the stage does not hold would
    // install and then fail at the first start. Catch it here, where the message
    // can still name the platform and the missing piece.
    for (label, payload_path) in manifest_paths(&view) {
        if !target.join(&payload_path).is_file() {
            let hint = if copied.is_empty() {
                format!(
                    "no script from platforms/ matches {}, so nothing was added to payload/bin/",
                    family(platform)
                )
            } else {
                format!("this platform got {}", copied.join(", "))
            };
            return Err(format!(
                "{label} points at {payload_path}, which is not in the package for {platform} ({hint})"
            ));
        }
    }
    Ok(target)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|error| format!("create {}: {}", to.display(), error))?;
    let entries =
        fs::read_dir(from).map_err(|error| format!("read {}: {}", from.display(), error))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {}", from.display(), error))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target)
                .map_err(|error| format!("copy {}: {}", source.display(), error))?;
        }
    }
    Ok(())
}

/// A throwaway stage in the system temp directory, removed when it goes out of
/// scope. `--stage` uses a path the caller keeps instead.
pub struct TempStage {
    root: PathBuf,
}

impl TempStage {
    pub fn new() -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "plugin-tools-stage-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).map_err(|error| format!("create {}: {}", root.display(), error))?;
        Ok(TempStage { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One package to build: identity, version, target triple and the device the
/// artifact name carries (first `compat.devices` id, if any).
pub struct Package<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub platform: &'a str,
    pub device: Option<&'a str>,
}

/// Where a package lands: the host names artifacts `<id>-<version>-<platform>.spk`
/// because the display name may be Chinese and must not reach a file name. A
/// device-restricted package appends the first compat id
/// (`<id>-<version>-<platform>-<device>.spk`), matching `plugin bundle`.
fn spk_path(root: &Path, package: &Package, out: Option<&Path>) -> PathBuf {
    out.map(Path::to_path_buf).unwrap_or_else(|| {
        let name = match package.device {
            Some(device) => format!(
                "{}-{}-{}-{}.spk",
                package.id, package.version, package.platform, device
            ),
            None => format!(
                "{}-{}-{}.spk",
                package.id, package.version, package.platform
            ),
        };
        root.join("dist").join(name)
    })
}

/// Pack a built package directory with the host's `plugin bundle`.
///
/// `package_dir` is normally `package/`; a platform with an overlay passes the
/// staged copy [`stage_package`] produced (the merged manifest lives there).
/// `root` still decides where the `.spk` lands.
pub fn pack(
    root: &Path,
    package_dir: &Path,
    package: &Package,
    key: Option<&Path>,
    out: Option<&Path>,
    exec: Option<&str>,
) -> Result<PathBuf, String> {
    let spk = spk_path(root, package, out);
    if let Some(parent) = spk.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }

    let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
    let mut args: Vec<String> = vec![
        "bundle".into(),
        package_dir.display().to_string(),
        "--platform".into(),
        package.platform.into(),
        // Pass the resolved id explicitly: the toolchain already decided between
        // `manifest.json`'s `id` and the directory name, and the host must not
        // re-derive it from a directory that may be called anything.
        "--id".into(),
        package.id.into(),
        "-o".into(),
        spk.display().to_string(),
    ];
    if let Some(exec) = exec {
        args.push("--exec".into());
        args.push(exec.into());
    }
    match key {
        Some(key) => {
            args.push("--key".into());
            args.push(key.display().to_string());
        }
        None => args.push("--unsigned".into()),
    }

    println!("$ {bin} {}", args.join(" "));
    let status = Command::new(&bin)
        .args(&args)
        .status()
        .map_err(|error| format!("{bin}: {error}"))?;
    if !status.success() {
        return Err(format!("{bin} exited {}", status.code().unwrap_or(-1)));
    }
    Ok(spk)
}

/// Host triple for the machine this runs on.
pub fn host_triple() -> Result<&'static str, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        _ => Err(format!(
            "cannot guess a target triple for {os}/{arch}; pass --platform"
        )),
    }
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 64
        && name
            .chars()
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || matches!(value, '-' | '_'))
}

pub fn pascal(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn is_text_file(name: &str) -> bool {
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    matches!(
        extension,
        "json" | "rs" | "c" | "h" | "toml" | "html" | "mjs" | "sh" | "ps1" | "psm1" | "md" | "txt"
    )
}

/// Copy a template tree, substituting the placeholders in text files.
pub fn instantiate(
    from: &Path,
    to: &Path,
    name: &str,
    pascal_name: &str,
    desc: &str,
) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|error| format!("create {}: {}", to.display(), error))?;
    let entries = fs::read_dir(from).map_err(|error| format!("read {}: {}", from.display(), error))?;
    for entry in entries.flatten() {
        let source = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if source.is_dir() && SKIP_DIRS.contains(&file_name.as_str()) {
            continue;
        }
        let target = to.join(file_name.replace("__NAME__", name));
        if source.is_dir() {
            instantiate(&source, &target, name, pascal_name, desc)?;
            continue;
        }
        if is_text_file(&file_name) {
            let text = fs::read_to_string(&source)
                .map_err(|error| format!("read {}: {}", source.display(), error))?;
            let rendered = text
                .replace("__NAME_PASCAL__", pascal_name)
                .replace("__NAME__", name)
                .replace("__DESC__", desc);
            fs::write(&target, rendered)
                .map_err(|error| format!("write {}: {}", target.display(), error))?;
        } else {
            fs::copy(&source, &target)
                .map_err(|error| format!("copy {}: {}", source.display(), error))?;
        }
        if file_name.ends_with(".sh") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o755));
            }
        }
    }
    Ok(())
}

/// Copy the shared SDK files a language needs into the project.
pub fn vendor_sdk(repo: &Repo, project: &Path, language: &str) -> Result<(), String> {
    let package = project.join("package");
    let web = package.join("payload").join("web");
    if web.is_dir() {
        fs::copy(
            repo.root.join("sdk").join("web").join("plugin-ui.mjs"),
            web.join("plugin-ui.mjs"),
        )
        .map_err(|error| format!("vendor web toolkit: {error}"))?;
    }

    // The SDK travels with the script that uses it: a single-family project keeps
    // its entry in `package/payload/bin/`, so the SDK sits in `package/payload/lib/`
    // and ships as it is. A `script` project keeps its entries outside the package
    // (`platforms/`), so the SDKs go there too — one per family, copied into the
    // stage by [`materialize_scripts`], which is what keeps the PowerShell module
    // out of a Linux package and the POSIX one out of Windows.
    let lib = match language {
        "script" => project.join("platforms").join("lib"),
        _ => package.join("payload").join("lib"),
    };
    match language {
        "shell" => {
            fs::create_dir_all(&lib).map_err(|error| format!("create {}: {}", lib.display(), error))?;
            fs::copy(
                repo.root.join("sdk").join("shell").join("svcd-plugin.sh"),
                lib.join("svcd-plugin.sh"),
            )
            .map_err(|error| format!("vendor shell sdk: {error}"))?;
        }
        // `script` ships a POSIX script and a PowerShell one, so the project needs
        // both SDKs; each package gets only the half its scripts use.
        "script" => {
            fs::create_dir_all(&lib).map_err(|error| format!("create {}: {}", lib.display(), error))?;
            fs::copy(
                repo.root.join("sdk").join("shell").join("svcd-plugin.sh"),
                lib.join("svcd-plugin.sh"),
            )
            .map_err(|error| format!("vendor shell sdk: {error}"))?;
            fs::copy(
                repo.root.join("sdk").join("powershell").join("SvcdPlugin.ps1"),
                lib.join("SvcdPlugin.ps1"),
            )
            .map_err(|error| format!("vendor powershell sdk: {error}"))?;
        }
        "powershell" => {
            fs::create_dir_all(&lib).map_err(|error| format!("create {}: {}", lib.display(), error))?;
            fs::copy(
                repo.root.join("sdk").join("powershell").join("SvcdPlugin.ps1"),
                lib.join("SvcdPlugin.ps1"),
            )
            .map_err(|error| format!("vendor powershell sdk: {error}"))?;
        }
        "c" => {
            let src = project.join("src");
            fs::create_dir_all(&src).map_err(|error| format!("create {}: {}", src.display(), error))?;
            fs::copy(
                repo.root.join("sdk").join("c").join("svcd_plugin.h"),
                src.join("svcd_plugin.h"),
            )
            .map_err(|error| format!("vendor c sdk: {error}"))?;
        }
        _ => {}
    }
    fs::create_dir_all(package.join("payload").join("bin"))
        .map_err(|error| format!("create payload/bin: {error}"))?;
    Ok(())
}

/// Refresh vendored SDK copies in every plugin under `plugins/` and `samples/`.
pub fn sync(repo: &Repo) -> Result<usize, String> {
    let mut count = 0;
    for base in ["plugins", "samples"] {
        let dir = repo.root.join(base);
        if !dir.is_dir() {
            continue;
        }
        let entries = fs::read_dir(&dir).map_err(|error| format!("read {}: {}", dir.display(), error))?;
        for entry in entries.flatten() {
            let package = entry.path().join("package");
            if !package.join("manifest.json").is_file() {
                continue;
            }
            // Refresh, never inject: a package whose page does not import the
            // toolkit (meow ships a two-line redirect to YACD) must not grow a
            // 15 KiB file that would then have to be committed.
            let toolkit = package.join("payload").join("web").join("plugin-ui.mjs");
            if toolkit.is_file() {
                let _ = fs::copy(
                    repo.root.join("sdk").join("web").join("plugin-ui.mjs"),
                    &toolkit,
                );
            }
            let header = entry.path().join("src").join("svcd_plugin.h");
            if header.is_file() {
                let _ = fs::copy(
                    repo.root.join("sdk").join("c").join("svcd_plugin.h"),
                    header,
                );
            }
            let lib = package.join("payload").join("lib");
            if lib.is_dir() {
                refresh_sdk(&repo.root, &lib);
            }
            // A `script` project keeps its SDKs outside the package, one per
            // family; refresh whichever ones it actually has.
            let platform_lib = entry.path().join("platforms").join("lib");
            if platform_lib.is_dir() {
                refresh_sdk(&repo.root, &platform_lib);
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Refresh the SDK files a lib directory already holds; never add one.
///
/// A shell plugin must not ship the PowerShell module and vice versa, so what a
/// project vendors is what it keeps getting.
fn refresh_sdk(repo: &Path, lib: &Path) {
    for (name, from) in [
        ("svcd-plugin.sh", ["sdk", "shell", "svcd-plugin.sh"]),
        ("SvcdPlugin.ps1", ["sdk", "powershell", "SvcdPlugin.ps1"]),
    ] {
        if lib.join(name).exists() {
            let source = from.iter().fold(repo.to_path_buf(), |path, part| path.join(part));
            let _ = fs::copy(source, lib.join(name));
        }
    }
}

/// Project metadata written at scaffold time (stays in the repo, not the .spk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub language: String,
    pub platforms: Vec<String>,
    /// Device-restricted packages built next to the universal one for the same
    /// triple. A plugin that ships a kernel module needs this: the module only
    /// matches one firmware generation, while every other host on that triple
    /// takes the portable build.
    #[serde(default)]
    pub variants: Vec<Variant>,
}

/// A device-restricted build of one or more platforms.
///
/// The variant, not the base manifest, owns `compat.devices` for its package:
/// the same triple also ships a universal build, so the restriction cannot live
/// in a manifest both share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    /// Compatibility id the package is limited to (`sr6110`).
    pub device: String,
    /// Triples this variant builds. Required: a variant that silently applied
    /// to every platform would put a kernel module in the portable packages.
    #[serde(default)]
    pub platforms: Vec<String>,
}

/// One package to build: a target triple plus the device whose compatibility
/// list the package narrows itself to (`None` = universal for the triple).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildUnit {
    pub platform: String,
    pub device: Option<String>,
}

pub fn write_project(project: &Path, meta: &Project) -> Result<(), String> {
    let path = project.join(".plugin.json");
    let text = serde_json::to_string_pretty(meta).map_err(|error| error.to_string())?;
    fs::write(&path, format!("{text}\n")).map_err(|error| format!("write {}: {}", path.display(), error))
}

/// Read `.plugin.json` when it exists; a plugin without one declares nothing.
pub fn read_project(project: &Path) -> Result<Project, String> {
    let path = project.join(".plugin.json");
    if !path.exists() {
        return Ok(Project {
            name: String::new(),
            language: String::new(),
            platforms: Vec::new(),
            variants: Vec::new(),
        });
    }
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {}", path.display(), error))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {}", path.display(), error))
}

/// Target triples to build: `--platform` wins, then the plugin's own declaration,
/// then "all seven" — a plugin that does not name platforms is portable.
pub fn resolve_platforms(project: &Path, explicit: Option<&str>) -> Result<Vec<String>, String> {
    if let Some(platform) = explicit {
        return Ok(vec![platform.to_string()]);
    }
    let declared = read_project(project)?.platforms;
    if declared.is_empty() {
        return Ok(PLATFORMS.iter().map(|value| value.to_string()).collect());
    }
    Ok(declared)
}

/// One package per platform, plus one per applicable variant.
///
/// The universal package for a triple always exists; a variant adds a second
/// package for that triple, narrowed to one device. `--device` keeps only that
/// variant, and refusing an unknown id here is what stops a typo from quietly
/// producing the universal package the caller did not ask for.
pub fn resolve_units(
    project: &Path,
    platform: Option<&str>,
    device: Option<&str>,
) -> Result<Vec<BuildUnit>, String> {
    let platforms = resolve_platforms(project, platform)?;
    let variants = read_project(project)?.variants;
    let mut units: Vec<BuildUnit> = platforms
        .iter()
        .map(|platform| BuildUnit {
            platform: platform.clone(),
            device: None,
        })
        .collect();
    let explicit = platform.is_some();
    for variant in &variants {
        let device = variant.device.trim();
        if device.is_empty() {
            return Err(".plugin.json: variants[].device must not be empty".into());
        }
        if variant.platforms.is_empty() {
            return Err(format!(
                ".plugin.json: variant {device} must list the platforms it applies to"
            ));
        }
        for target in &variant.platforms {
            // An explicit --platform means one package: a variant for another
            // triple simply does not take part. Without it, a platform outside
            // the declaration is a typo the build must not paper over.
            if !platforms.contains(target) {
                if !explicit {
                    return Err(format!(
                        ".plugin.json: variant {device} lists {target}, which is not in platforms"
                    ));
                }
                continue;
            }
            let unit = BuildUnit {
                platform: target.clone(),
                device: Some(device.to_string()),
            };
            if units.contains(&unit) {
                return Err(format!(
                    ".plugin.json: variant {device} is declared twice for {target}"
                ));
            }
            units.push(unit);
        }
    }
    if let Some(want) = device {
        units.retain(|unit| unit.device.as_deref() == Some(want));
        match units.len() {
            0 => {
                return Err(format!(
                    "no build unit for --device {want}: .plugin.json declares no such variant on {}",
                    platform.unwrap_or("the declared platforms")
                ))
            }
            1 => {}
            _ => {
                return Err(format!(
                    "--device {want} builds {} platforms; also pass --platform <triple>",
                    units.len()
                ))
            }
        }
    }
    Ok(units)
}

/// The manifest a device variant ships: the platform-merged one, with
/// `compat.devices` replaced by the variant's device.
///
/// Replacing rather than merging is deliberate — the base manifest is shared
/// with the universal package, which must stay installable everywhere.
pub fn with_device_compat(manifest: Value, device: &str) -> Value {
    let mut manifest = manifest;
    manifest["compat"] = serde_json::json!({ "devices": [device] });
    manifest
}

/// Generate the machine-local Ed25519 signing key as PKCS#8 / SPKI PEM.
pub fn devkey() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "no home directory".to_string())?;
    let dir = PathBuf::from(home).join(".svcd-dev");
    let key_path = dir.join("dev.pem");
    if key_path.exists() {
        return Ok(key_path);
    }
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {}", dir.display(), error))?;

    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|error| format!("random: {error}"))?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let public = signing.verifying_key().to_bytes();

    // Fixed DER prefixes for Ed25519 PKCS#8 and SPKI, matching what the host reads.
    const PRIVATE_PREFIX: [u8; 16] = [
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    const PUBLIC_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    let mut der = PRIVATE_PREFIX.to_vec();
    der.extend_from_slice(&seed);
    fs::write(&key_path, pem("PRIVATE KEY", &der))
        .map_err(|error| format!("write {}: {}", key_path.display(), error))?;

    let mut der = PUBLIC_PREFIX.to_vec();
    der.extend_from_slice(&public);
    fs::write(dir.join("dev.pub"), pem("PUBLIC KEY", &der))
        .map_err(|error| format!("write dev.pub: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));
    }
    Ok(key_path)
}

fn pem(label: &str, der: &[u8]) -> String {
    let body = BASE64.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sync` refreshes the SDK copies a plugin already ships and never adds
    /// one: a UI that does not import the toolkit must stay without it.
    #[test]
    fn sync_refreshes_vendored_sdks_without_injecting_them() {
        let root = std::env::temp_dir().join(format!("plugin-tools-sync-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sdk = root.join("sdk");
        fs::create_dir_all(sdk.join("web")).unwrap();
        fs::create_dir_all(sdk.join("c")).unwrap();
        fs::create_dir_all(sdk.join("shell")).unwrap();
        fs::create_dir_all(sdk.join("powershell")).unwrap();
        fs::write(sdk.join("web/plugin-ui.mjs"), "export const v = 2;\n").unwrap();
        fs::write(sdk.join("c/svcd_plugin.h"), "/* v2 */\n").unwrap();
        fs::write(sdk.join("shell/svcd-plugin.sh"), "# v2\n").unwrap();
        fs::write(sdk.join("powershell/SvcdPlugin.ps1"), "# v2\n").unwrap();

        // A plugin with a plain page and no SDK copy: nothing may be injected.
        let plain = root.join("plugins/plain");
        fs::create_dir_all(plain.join("package/payload/web")).unwrap();
        fs::write(plain.join("package/manifest.json"), "{}").unwrap();
        fs::write(plain.join("package/payload/web/index.html"), "<p>hi</p>").unwrap();

        // A plugin that does vendor the toolkit: the copy is refreshed.
        let vendor = root.join("samples/vendor");
        fs::create_dir_all(vendor.join("package/payload/web")).unwrap();
        fs::write(vendor.join("package/manifest.json"), "{}").unwrap();
        fs::write(vendor.join("package/payload/web/plugin-ui.mjs"), "stale\n").unwrap();

        assert_eq!(sync(&Repo { root: root.clone() }).unwrap(), 2);
        assert!(
            !plain.join("package/payload/web/plugin-ui.mjs").exists(),
            "sync must not drop the toolkit into a page that never used it"
        );
        assert_eq!(
            fs::read_to_string(vendor.join("package/payload/web/plugin-ui.mjs")).unwrap(),
            "export const v = 2;\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A spec is a git URL only when it says so; a checkout path that happens to
    /// contain a colon (`C:\plugins`) must stay a path.
    #[test]
    fn only_urls_and_dot_git_paths_count_as_repositories() {
        for url in [
            "https://github.com/me/templates.git",
            "ssh://git@host/templates",
            "git@github.com:me/templates.git",
            "file:///tmp/templates",
            "../somewhere/templates.git",
        ] {
            assert!(is_git_spec(url), "{url} should be a git spec");
        }
        for path in [".", "..", "../plugins", "/opt/templates", "C:\\plugins", "templates"] {
            assert!(!is_git_spec(path), "{path} should be a local path");
        }
    }

    #[test]
    fn a_second_platform_does_not_inherit_the_first_ones_binary() {
        assert_eq!(stale_twin("demo"), "demo.exe");
        assert_eq!(stale_twin("demo.exe"), "demo");
        // Nothing to strip means nothing to remove beyond the name itself.
        assert_eq!(stale_twin(".exe"), ".exe.exe");
    }

    /// A staged copy has to describe the artifact that is in it: Windows adds
    /// `.exe` to the binary, and the release pipeline bundles the tarball later,
    /// where the `--exec` this build used no longer applies.
    #[test]
    fn a_staged_copy_records_the_binary_the_build_produced() {
        let root =
            std::env::temp_dir().join(format!("plugin-tools-stage-exec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("package/payload/bin")).unwrap();
        fs::write(
            root.join("package/manifest.json"),
            r#"{
  "schema": 1,
  "id": "demo",
  "version": "1.0.0",
  "exec": { "path": "payload/bin/demo", "ready": "notify" }
}"#,
        )
        .unwrap();
        fs::write(root.join("package/payload/bin/demo.exe"), "MZ").unwrap();

        let stage = root.join("stage");
        let staged = stage_package(
            &root,
            &stage,
            None,
            "x86_64-pc-windows-msvc",
            Some("payload/bin/demo.exe"),
        )
        .unwrap();
        let bytes = fs::read(staged.join("manifest.json")).unwrap();
        let manifest: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(manifest["exec"]["path"], "payload/bin/demo.exe");
        // `ready` is the manifest's own field and must survive the rewrite.
        assert_eq!(manifest["exec"]["ready"], "notify");
        // The authored manifest is untouched: other platforms still get it.
        let bytes = fs::read(root.join("package/manifest.json")).unwrap();
        let base: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(base["exec"]["path"], "payload/bin/demo");
        let _ = fs::remove_dir_all(root);
    }


    #[test]
    fn the_compiler_defaults_to_gcc_unless_cc_says_otherwise() {
        assert_eq!(compiler(None), "gcc");
        assert_eq!(compiler(Some("")), "gcc");
        assert_eq!(compiler(Some("  ")), "gcc");
        assert_eq!(
            compiler(Some("/opt/cross/bin/aarch64-linux-musl-gcc")),
            "/opt/cross/bin/aarch64-linux-musl-gcc"
        );
    }

    #[test]
    fn placeholders_expand() {
        assert_eq!(
            expand("target/release/{name}-{version}-{platform}", "hello", "1.0.0", "x86_64-pc-windows-msvc"),
            "target/release/hello-1.0.0-x86_64-pc-windows-msvc"
        );
        // `{device}` is the variant's id, empty for the universal package.
        assert_eq!(
            expand_unit("out/{platform}/{device}", "wg", "1.0.0", "aarch64-unknown-linux-musl", "sr6110"),
            "out/aarch64-unknown-linux-musl/sr6110"
        );
        assert_eq!(
            expand_unit("out/{platform}/{device}", "wg", "1.0.0", "aarch64-unknown-linux-musl", ""),
            "out/aarch64-unknown-linux-musl/"
        );
    }

    #[test]
    fn names_follow_the_service_id_rules() {
        assert!(valid_name("wireguard"));
        assert!(valid_name("wire-guard_2"));
        assert!(!valid_name("Wireguard"));
        assert!(!valid_name("wire.guard"));
        assert!(!valid_name(""));
        assert!(!valid_name("-leading"));
    }

    #[test]
    fn pascal_case_handles_both_separators() {
        assert_eq!(pascal("hello-world_2"), "HelloWorld2");
    }

    #[test]
    fn the_menus_list_values_only() {
        assert_eq!(LANGUAGES, ["rust", "c", "shell", "powershell", "script"]);
        assert_eq!(PLATFORMS.len(), 7);
        assert!(PLATFORMS.contains(&"x86_64-pc-windows-msvc"));
        assert!(PLATFORMS.contains(&"x86_64-unknown-linux-musl"));
    }

    /// The language menu is derived from the platform set: a shell script cannot
    /// run on Windows, and only `script` produces one entry per family.
    #[test]
    fn languages_follow_the_platform_set() {
        let unix = |name: &str| name.to_string();
        let all_unix = vec![
            unix("x86_64-unknown-linux-musl"),
            unix("aarch64-apple-darwin"),
        ];
        assert_eq!(
            languages_for(&all_unix),
            ["rust", "c", "shell", "powershell"]
        );

        let all_windows = vec![unix("x86_64-pc-windows-msvc")];
        assert_eq!(languages_for(&all_windows), ["rust", "c", "powershell"]);

        let mixed = vec![
            unix("x86_64-unknown-linux-musl"),
            unix("x86_64-pc-windows-msvc"),
        ];
        assert_eq!(languages_for(&mixed), ["rust", "c", "script"]);
    }

    #[test]
    fn platforms_come_from_the_project_and_default_to_all_seven() {
        let root = std::env::temp_dir().join(format!("plugin-tools-plat-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);

        // No .plugin.json at all: portable, so every platform.
        assert_eq!(resolve_platforms(&root, None).unwrap().len(), 7);
        // An explicit --platform wins over everything.
        assert_eq!(
            resolve_platforms(&root, Some("x86_64-pc-windows-msvc")).unwrap(),
            vec!["x86_64-pc-windows-msvc".to_string()]
        );

        fs::write(
            root.join(".plugin.json"),
            r#"{"name":"x","language":"shell","platforms":["aarch64-apple-darwin"]}"#,
        )
        .unwrap();
        assert_eq!(
            resolve_platforms(&root, None).unwrap(),
            vec!["aarch64-apple-darwin".to_string()]
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A variant is one extra package next to the universal one, not a
    /// replacement: the same triple still ships the portable build.
    #[test]
    fn a_variant_adds_a_package_next_to_the_universal_one() {
        let root = std::env::temp_dir().join(format!("plugin-tools-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".plugin.json"),
            r#"{"name":"x","language":"shell",
                "platforms":["aarch64-unknown-linux-musl","x86_64-unknown-linux-musl"],
                "variants":[{"device":"sr6110","platforms":["aarch64-unknown-linux-musl"]}]}"#,
        )
        .unwrap();

        let all = resolve_units(&root, None, None).unwrap();
        assert_eq!(
            all,
            vec![
                BuildUnit {
                    platform: "aarch64-unknown-linux-musl".into(),
                    device: None
                },
                BuildUnit {
                    platform: "x86_64-unknown-linux-musl".into(),
                    device: None
                },
                BuildUnit {
                    platform: "aarch64-unknown-linux-musl".into(),
                    device: Some("sr6110".into())
                },
            ]
        );
        // --device names the variant; an id nothing builds is an error, not a
        // quietly universal package.
        assert_eq!(
            resolve_units(&root, None, Some("sr6110")).unwrap(),
            vec![BuildUnit {
                platform: "aarch64-unknown-linux-musl".into(),
                device: Some("sr6110".into())
            }]
        );
        assert!(resolve_units(&root, None, Some("sr6111")).is_err());
        // --platform narrows to that triple; both of its units still come back.
        assert_eq!(
            resolve_units(&root, Some("aarch64-unknown-linux-musl"), None)
                .unwrap()
                .len(),
            2
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A variant that names no platform, or one outside the declaration, would
    /// put a kernel module in packages meant to be portable.
    #[test]
    fn a_variant_must_name_the_platforms_it_builds() {
        let root = std::env::temp_dir().join(format!("plugin-tools-badvar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".plugin.json"),
            r#"{"name":"x","language":"shell","platforms":["aarch64-unknown-linux-musl"],
                "variants":[{"device":"sr6110"}]}"#,
        )
        .unwrap();
        assert!(resolve_units(&root, None, None).is_err());

        fs::write(
            root.join(".plugin.json"),
            r#"{"name":"x","language":"shell","platforms":["aarch64-unknown-linux-musl"],
                "variants":[{"device":"sr6110","platforms":["armv7-unknown-linux-musleabihf"]}]}"#,
        )
        .unwrap();
        assert!(resolve_units(&root, None, None).is_err());
        // Picking the declared platform by hand still skips the irrelevant
        // variant instead of failing.
        assert_eq!(
            resolve_units(&root, Some("aarch64-unknown-linux-musl"), None)
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_device_variant_replaces_the_compat_list() {
        let base = serde_json::json!({ "compat": { "devices": ["old"] }, "version": "1.0.0" });
        let narrowed = with_device_compat(base, "sr6110");
        assert_eq!(narrowed["compat"], serde_json::json!({ "devices": ["sr6110"] }));
        assert_eq!(narrowed["version"], "1.0.0");
    }

    #[test]
    fn a_missing_recipe_means_no_build_step() {
        let root = std::env::temp_dir().join(format!("plugin-tools-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);
        assert!(read_recipe(&root).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    /// A throwaway plugin root with a `package/manifest.json`.
    fn plugin_dir(tag: &str, manifest: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("plugin-tools-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("package")).unwrap();
        fs::write(root.join("package/manifest.json"), manifest).unwrap();
        root
    }

    fn manifest_with(extra: &str) -> String {
        format!(r#"{{"schema":1,"version":"1.0.0","exec":{{"path":"payload/bin/demo"}}{extra}}}"#)
    }

    #[test]
    fn plugin_id_comes_from_the_manifest_then_the_directory() {
        let root = plugin_dir(
            "id-manifest",
            &manifest_with(r#","id":"demo","name":"示例插件""#),
        );
        assert_eq!(
            plugin_id(&root, &read_manifest(&root).unwrap()).unwrap(),
            "demo"
        );

        // No `id` in the manifest: the directory name is the identity, the same
        // rule the host's bundler applies.
        let root = plugin_dir("id-dir", &manifest_with(r#","name":"显示名""#));
        assert_eq!(
            plugin_id(&root, &read_manifest(&root).unwrap()).unwrap(),
            root.file_name().unwrap().to_str().unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_id_rejects_what_the_host_would_reject() {
        let root = plugin_dir("id-bad", &manifest_with(r#","id":"示例","name":"x""#));
        let error = plugin_id(&root, &read_manifest(&root).unwrap()).unwrap_err();
        assert!(error.contains("示例"), "{error}");

        // An authored-but-empty id is an author mistake, not an absent id: the
        // host rejects it too, so falling back to the directory name would
        // silently package a plugin under a name its manifest did not choose.
        let root = plugin_dir("id-empty", &manifest_with(r#","id":"" "#));
        assert!(plugin_id(&root, &read_manifest(&root).unwrap()).is_err());

        let root = plugin_dir("id-reserved", &manifest_with(r#","id":"svc""#));
        let error = plugin_id(&root, &read_manifest(&root).unwrap()).unwrap_err();
        assert!(error.contains("svc"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn icons_must_exist_and_not_be_empty() {
        let root = plugin_dir(
            "icons",
            &manifest_with(r#","icons":{"png":"payload/icons/icon.png"}"#),
        );
        fs::create_dir_all(root.join("package/payload/icons")).unwrap();

        let error = assert_icons(&root, &read_manifest(&root).unwrap()).unwrap_err();
        assert!(error.contains("payload/icons/icon.png"), "{error}");

        fs::write(root.join("package/payload/icons/icon.png"), b"").unwrap();
        let error = assert_icons(&root, &read_manifest(&root).unwrap()).unwrap_err();
        assert!(error.contains("empty"), "{error}");

        fs::write(root.join("package/payload/icons/icon.png"), b"\x89PNG").unwrap();
        assert_icons(&root, &read_manifest(&root).unwrap()).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn icon_formats_are_the_three_the_host_accepts() {
        let root = plugin_dir(
            "icons-key",
            &manifest_with(r#","icons":{"svg":"payload/icons/icon.svg"}"#),
        );
        fs::create_dir_all(root.join("package/payload/icons")).unwrap();
        fs::write(root.join("package/payload/icons/icon.svg"), b"<svg/>").unwrap();

        let error = assert_icons(&root, &read_manifest(&root).unwrap()).unwrap_err();
        assert!(error.contains("svg"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_package_is_named_after_the_id_not_the_display_name() {
        let package = Package {
            id: "demo",
            version: "1.0.0",
            platform: "x86_64-unknown-linux-musl",
            device: None,
        };
        assert_eq!(
            spk_path(Path::new("/w/plugins/demo"), &package, None),
            Path::new("/w/plugins/demo/dist/demo-1.0.0-x86_64-unknown-linux-musl.spk")
        );
        let explicit = Path::new("/w/out.spk");
        assert_eq!(
            spk_path(Path::new("/w/plugins/demo"), &package, Some(explicit)),
            explicit
        );
    }

    #[test]
    fn a_device_restricted_package_carries_the_first_compat_id() {
        let package = Package {
            id: "demo",
            version: "1.0.0",
            platform: "aarch64-unknown-linux-musl",
            device: Some("sr6110"),
        };
        assert_eq!(
            spk_path(Path::new("/w/plugins/demo"), &package, None),
            Path::new("/w/plugins/demo/dist/demo-1.0.0-aarch64-unknown-linux-musl-sr6110.spk")
        );
    }
}
