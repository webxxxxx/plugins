//! What `plugin-tools build` / `dev` hand to the host's `plugin` CLI.
//!
//! The host is a separate binary in a separate repository, so these tests point
//! `SVCD_PLUGIN_BIN` at `tests/support/fake_host.rs` and read back its argv. The
//! fixtures are real manifests: a Chinese display name, an ASCII id, and icon
//! files whose presence is what the toolchain checks.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The host stand-in, compiled once per test binary with `rustc`.
///
/// It is not a `[[bin]]` target: `plugin-tools` ships as a single binary, and a
/// fixture that only tests need must not be built by `cargo build --release`.
fn fake_host() -> &'static str {
    static HOST: OnceLock<PathBuf> = OnceLock::new();
    HOST.get_or_init(|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_host.rs");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(if cfg!(windows) {
            "fake-host.exe"
        } else {
            "fake-host"
        });
        let status = Command::new("rustc")
            .args(["--edition", "2021", "-O", "--crate-name", "fake_host"])
            .arg(&source)
            .arg("-o")
            .arg(&out)
            .status()
            .expect("rustc must be on PATH to build the host fixture");
        assert!(status.success(), "failed to compile {}", source.display());
        out
    })
    .to_str()
    .expect("utf-8 path")
}

const MANIFEST: &str = r#"{
  "schema": 1,
  "id": "demo",
  "name": "示例插件",
  "version": "1.0.0",
  "desc": "fixture",
  "exec": { "path": "payload/bin/demo", "ready": "notify" },
  "capabilities": [
    {
      "name": "echo",
      "desc": "Echo a message",
      "inputSchema": { "type": "object", "properties": { "message": { "type": "string" } } }
    }
  ]
}"#;

/// A plugin directory: manifest, one payload file, no build step.
fn project(tag: &str, manifest: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("plugin-tools-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("package/payload/bin")).unwrap();
    std::fs::write(root.join("package/manifest.json"), manifest).unwrap();
    std::fs::write(root.join("package/payload/bin/demo"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("build.json"), r#"{"build": null}"#).unwrap();
    root
}

fn run(root: &Path, args: &[&str]) -> (std::process::Output, Vec<Vec<String>>) {
    let log = root.join("host.log");
    let _ = std::fs::remove_file(&log);
    let output = Command::new(env!("CARGO_BIN_EXE_plugin-tools"))
        .args(args)
        .arg("--dir")
        .arg(root)
        .env("SVCD_PLUGIN_BIN", fake_host())
        .env("FAKE_HOST_LOG", &log)
        .output()
        .unwrap();
    (output, invocations(&log))
}

/// The argv of every fake-host call, in order.
fn invocations(log: &Path) -> Vec<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(log) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let argv = value.get("argv")?.as_array()?;
            Some(
                argv.iter()
                    .filter_map(|arg| arg.as_str().map(str::to_string))
                    .collect(),
            )
        })
        .collect()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn the_package_is_named_after_the_id_and_then_verified() {
    let root = project("named", MANIFEST);
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let spk = root.join("dist/demo-1.0.0-x86_64-unknown-linux-gnu.spk");
    assert!(spk.is_file(), "expected {}", spk.display());

    let bundle = calls
        .iter()
        .find(|argv| argv[0] == "bundle")
        .expect("bundle ran");
    assert!(bundle.contains(&"x86_64-unknown-linux-gnu".to_string()));
    let id = bundle.iter().position(|arg| arg == "--id").expect("--id");
    assert_eq!(bundle[id + 1], "demo");
    let out = bundle.iter().position(|arg| arg == "-o").expect("-o");
    assert_eq!(Path::new(&bundle[out + 1]), spk);
    assert!(!bundle.contains(&"示例插件".to_string()), "{bundle:?}");

    let verify = calls
        .iter()
        .find(|argv| argv[0] == "verify")
        .expect("verify ran");
    assert_eq!(Path::new(&verify[1]), spk);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_manifest_without_an_id_falls_back_to_the_directory_name() {
    let root = project("fallback", &MANIFEST.replace("\"id\": \"demo\",", ""));
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let dir = root.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        root.join(format!("dist/{dir}-1.0.0-x86_64-unknown-linux-gnu.spk"))
            .is_file(),
        "calls: {calls:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_icon_fails_before_the_host_is_called() {
    let root = project(
        "icons-missing",
        &MANIFEST.replace(
            "\"capabilities\"",
            "\"icons\": { \"png\": \"payload/icons/missing.png\" },\n  \"capabilities\"",
        ),
    );
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("payload/icons/missing.png"),
        "{}",
        stderr(&output)
    );
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_declared_icon_lets_the_build_through() {
    let root = project(
        "icons-ok",
        &MANIFEST.replace(
            "\"capabilities\"",
            "\"icons\": { \"png\": \"payload/icons/icon.png\" },\n  \"capabilities\"",
        ),
    );
    std::fs::create_dir_all(root.join("package/payload/icons")).unwrap();
    std::fs::write(root.join("package/payload/icons/icon.png"), b"\x89PNG\r\n").unwrap();
    let (output, _calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dev_starts_the_id_not_the_display_name() {
    let root = project("dev", MANIFEST);
    let triple = plugin_tools::host_triple().unwrap();
    let (output, calls) = run(&root, &["dev", "--platform", triple]);
    assert!(output.status.success(), "{}", stderr(&output));

    let install = calls
        .iter()
        .find(|argv| argv[0] == "install")
        .expect("install ran");
    assert!(
        install[1].ends_with(&format!("demo-1.0.0-{triple}.spk")),
        "{install:?}"
    );
    let start = calls
        .iter()
        .find(|argv| argv[0] == "start")
        .expect("start ran");
    assert_eq!(start[1], "demo");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_recipe_expands_the_id_into_file_names() {
    let root = project("recipe", MANIFEST);
    // The build step is the fake host, so no compiler is needed: it writes
    // `out/{name}`, and `{name}` must be the ASCII id.
    std::fs::write(
        root.join("build.json"),
        format!(
            r#"{{"build":{{"command":["{}","bundle","-o","out/{{name}}"],"artifact":"out/{{name}}"}}}}"#,
            fake_host().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let recipe = calls
        .iter()
        .find(|argv| argv[0] == "bundle" && argv.contains(&"out/demo".to_string()))
        .unwrap_or_else(|| panic!("recipe did not expand {{name}} to the id: {calls:?}"));
    assert_eq!(recipe, &["bundle", "-o", "out/demo"]);
    assert!(root.join("package/payload/bin/demo").is_file());
    let _ = std::fs::remove_dir_all(root);
}
