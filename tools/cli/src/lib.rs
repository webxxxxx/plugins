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
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directories that never travel from a template into a project.
const SKIP_DIRS: &[&str] = &[
    "target", "dist", "out", "node_modules", ".git", ".idea", ".vscode",
];

pub const LANGUAGES: &[&str] = &["rust", "c", "shell", "powershell"];

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

/// The repository checkout that holds `templates/` and `sdk/`.
pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    pub fn discover(explicit: Option<&Path>) -> Result<Self, String> {
        if let Some(root) = explicit {
            return Ok(Repo {
                root: root.to_path_buf(),
            });
        }
        if let Some(root) = std::env::var_os("PLUGIN_TOOLS_REPO") {
            return Ok(Repo {
                root: PathBuf::from(root),
            });
        }
        let mut current = std::env::current_dir().map_err(|error| error.to_string())?;
        loop {
            if current.join("templates").is_dir() && current.join("sdk").is_dir() {
                return Ok(Repo { root: current });
            }
            if !current.pop() {
                return Err(
                    "cannot find the plugin repository (templates/ + sdk/); pass --repo".into(),
                );
            }
        }
    }

    pub fn template(&self, language: &str) -> PathBuf {
        self.root.join("templates").join(language)
    }

    pub fn template_dir(&self) -> PathBuf {
        self.root.join("templates")
    }
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
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {}", path.display(), error))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {}", path.display(), error))
}

/// The plugin's identity: manifest `id`, else the project directory name.
///
/// Same rule the host's bundler applies. The id ends up in directory names, log
/// file names, `<id>.<capability>` tool names and `/plugin-ui/<id>/`, so it must
/// stay ASCII; the display name (`name`) is what may be Chinese.
pub fn plugin_id(root: &Path, manifest: &Manifest) -> Result<String, String> {
    let id = match manifest
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
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
    value
        .replace("{name}", id)
        .replace("{version}", version)
        .replace("{platform}", platform)
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

    let artifact = expand(&spec.artifact, id, &manifest.version, platform);
    let artifact_path = root.join(&artifact);
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }

    let program = expand(&spec.command[0], id, &manifest.version, platform);
    let args: Vec<String> = spec.command[1..]
        .iter()
        .map(|part| expand(part, id, &manifest.version, platform))
        .collect();

    let mut command = Command::new(&program);
    command.args(&args).current_dir(root);
    for layer in [spec.env.get("*"), spec.env.get(platform)].into_iter().flatten() {
        for (key, value) in layer {
            command.env(key, expand(value, id, &manifest.version, platform));
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

/// Fail early when a package would ship nothing.
pub fn assert_payload(root: &Path) -> Result<(), String> {
    let payload = root.join("package").join("payload");
    if !payload.is_dir() || count_files(&payload)? == 0 {
        return Err("package/payload is empty: build first or add the files the plugin ships".into());
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

/// One package to build: identity, version and the target triple.
pub struct Package<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub platform: &'a str,
}

/// Where a package lands: the host names artifacts `<id>-<version>-<platform>.spk`
/// because the display name may be Chinese and must not reach a file name.
fn spk_path(root: &Path, package: &Package, out: Option<&Path>) -> PathBuf {
    out.map(Path::to_path_buf).unwrap_or_else(|| {
        root.join("dist").join(format!(
            "{}-{}-{}.spk",
            package.id, package.version, package.platform
        ))
    })
}

/// Pack `package/` with the host's `plugin bundle`.
pub fn pack(
    root: &Path,
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
        root.join("package").display().to_string(),
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

    let lib = package.join("payload").join("lib");
    match language {
        "shell" => {
            fs::create_dir_all(&lib).map_err(|error| format!("create {}: {}", lib.display(), error))?;
            fs::copy(
                repo.root.join("sdk").join("shell").join("svcd-plugin.sh"),
                lib.join("svcd-plugin.sh"),
            )
            .map_err(|error| format!("vendor shell sdk: {error}"))?;
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
            let web = package.join("payload").join("web");
            if web.is_dir() {
                let _ = fs::copy(
                    repo.root.join("sdk").join("web").join("plugin-ui.mjs"),
                    web.join("plugin-ui.mjs"),
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
                // Only refresh the SDK the plugin actually vendors; a shell
                // plugin must not ship the PowerShell module (the packer takes
                // everything under package/).
                if lib.join("svcd-plugin.sh").exists() {
                    let _ = fs::copy(
                        repo.root.join("sdk").join("shell").join("svcd-plugin.sh"),
                        lib.join("svcd-plugin.sh"),
                    );
                }
                if lib.join("SvcdPlugin.ps1").exists() {
                    let _ = fs::copy(
                        repo.root.join("sdk").join("powershell").join("SvcdPlugin.ps1"),
                        lib.join("SvcdPlugin.ps1"),
                    );
                }
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Project metadata written at scaffold time (stays in the repo, not the .spk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub language: String,
    pub platforms: Vec<String>,
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

    #[test]
    fn placeholders_expand() {
        assert_eq!(
            expand("target/release/{name}-{version}-{platform}", "hello", "1.0.0", "x86_64-pc-windows-msvc"),
            "target/release/hello-1.0.0-x86_64-pc-windows-msvc"
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
        assert_eq!(LANGUAGES, ["rust", "c", "shell", "powershell"]);
        assert_eq!(PLATFORMS.len(), 7);
        assert!(PLATFORMS.contains(&"x86_64-pc-windows-msvc"));
        assert!(PLATFORMS.contains(&"x86_64-unknown-linux-musl"));
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
}
