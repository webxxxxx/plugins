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
use std::collections::HashMap;
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
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub exec: Exec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Exec {
    pub path: String,
}

pub fn read_manifest(root: &Path) -> Result<Manifest, String> {
    let path = root.join("package").join("manifest.json");
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {}", path.display(), error))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {}", path.display(), error))
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
pub fn expand(value: &str, name: &str, version: &str, platform: &str) -> String {
    value
        .replace("{name}", name)
        .replace("{version}", version)
        .replace("{platform}", platform)
}

/// Run the recipe and copy the artifact to `payload/bin/`.
///
/// Returns the exec path the packaged manifest should use — on Windows the
/// artifact keeps its `.exe` suffix, and `CreateProcess` does not guess it.
pub fn build_from_recipe(
    root: &Path,
    manifest: &Manifest,
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

    let artifact = expand(&spec.artifact, &manifest.name, &manifest.version, platform);
    let artifact_path = root.join(&artifact);
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }

    let program = expand(&spec.command[0], &manifest.name, &manifest.version, platform);
    let args: Vec<String> = spec.command[1..]
        .iter()
        .map(|part| expand(part, &manifest.name, &manifest.version, platform))
        .collect();

    let mut command = Command::new(&program);
    command.args(&args).current_dir(root);
    for layer in [spec.env.get("*"), spec.env.get(platform)].into_iter().flatten() {
        for (key, value) in layer {
            command.env(key, expand(value, &manifest.name, &manifest.version, platform));
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

    let base = Path::new(&manifest.exec.path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "manifest.exec.path is missing".to_string())?
        .to_string();
    let built_name = built
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let keep_suffix = !base.to_lowercase().ends_with(".exe") && built_name.to_lowercase().ends_with(".exe");
    let final_name = if keep_suffix { format!("{base}.exe") } else { base };

    let target = root.join("package").join("payload").join("bin").join(&final_name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }
    fs::copy(&built, &target)
        .map_err(|error| format!("copy {}: {}", built.display(), error))?;
    Ok(Some(format!("payload/bin/{final_name}")))
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

/// Pack `package/` with the host's `plugin bundle`.
pub fn pack(
    root: &Path,
    manifest: &Manifest,
    platform: &str,
    key: Option<&Path>,
    out: Option<&Path>,
    exec: Option<&str>,
) -> Result<PathBuf, String> {
    let spk = out.map(Path::to_path_buf).unwrap_or_else(|| {
        root.join("dist")
            .join(format!("{}-{}-{}.spk", manifest.name, manifest.version, platform))
    });
    if let Some(parent) = spk.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {}", parent.display(), error))?;
    }

    let bin = std::env::var("SVCD_PLUGIN_BIN").unwrap_or_else(|_| "plugin".to_string());
    let mut args: Vec<String> = vec![
        "bundle".into(),
        root.join("package").display().to_string(),
        "--platform".into(),
        platform.into(),
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
    fn a_missing_recipe_means_no_build_step() {
        let root = std::env::temp_dir().join(format!("plugin-tools-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);
        assert!(read_recipe(&root).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}
