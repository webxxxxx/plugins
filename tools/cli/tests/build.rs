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

/// The same plugin with a capability answered by a one-shot hook (svcd #342).
/// The hook path is a payload file like any other, so the stage has to hold it:
/// `capabilities[]` is an array, and an array arm is easy to forget in the walk.
const MANIFEST_WITH_HOOK: &str = r#"{
  "schema": 1,
  "id": "demo",
  "name": "示例插件",
  "version": "1.0.0",
  "desc": "fixture",
  "exec": { "path": "payload/bin/demo", "ready": "exec" },
  "capabilities": [
    {
      "name": "echo",
      "desc": "Echo a message",
      "inputSchema": { "type": "object", "properties": { "message": { "type": "string" } } },
      "hook": { "path": "payload/bin/echo.sh", "args": ["echo"], "timeout": 5 }
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

/// A project whose manifest declares a capability hook and that ships a family
/// overlay — the overlay is what makes the build stage a copy, and the stage is
/// where a named payload path is checked.
fn hooked_project(tag: &str) -> PathBuf {
    let root = project(tag, MANIFEST_WITH_HOOK);
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/unix.json"),
        "{\n  \"exec\": { \"path\": \"payload/bin/demo\", \"ready\": \"exec\" }\n}",
    )
    .unwrap();
    root
}

#[test]
fn a_missing_capability_hook_fails_before_the_host_is_called() {
    let root = hooked_project("capability-hook-missing");
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("payload/bin/echo.sh"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("capabilities[0].hook.path"),
        "the error should name where the path came from: {}",
        stderr(&output)
    );
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_capability_hook_in_the_package_lets_the_build_through() {
    let root = hooked_project("capability-hook-ok");
    std::fs::write(root.join("package/payload/bin/echo.sh"), "#!/bin/sh\n").unwrap();
    let (output, _calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_declared_icon_lets_the_build_through() {    let root = project(
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
fn the_recipe_compiler_comes_from_cc() {
    // `{cc}` is how one build.json covers both a native build and a musl cross
    // build: the command array cannot vary by platform, `CC` can.
    let root = project("recipe-cc", MANIFEST);
    std::fs::write(
        root.join("build.json"),
        r#"{"build":{"command":["{cc}","--version"],"artifact":"out/demo"}}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_plugin-tools"))
        .args(["build", "--platform", "x86_64-unknown-linux-musl"])
        .arg("--dir")
        .arg(&root)
        .env("SVCD_PLUGIN_BIN", fake_host())
        .env("CC", "/opt/cross/bin/aarch64-linux-musl-gcc")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("/opt/cross/bin/aarch64-linux-musl-gcc --version"),
        "{stdout}"
    );
    // That compiler does not exist here, so the build stops before bundling.
    assert!(!output.status.success(), "{stdout}");
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

/// A platform whose script language differs gets only its few keys merged in,
/// into a staged copy: the authored manifest is the base for every other
/// platform, and `platforms/` must never travel as payload.
#[test]
fn a_platform_overlay_is_merged_into_a_staged_copy() {
    let root = project("overlay", MANIFEST);
    let stage = root.join("dist/stage");
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/x86_64-pc-windows-msvc.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" },
  "lifecycle": { "start": { "path": "payload/bin/demo.ps1" } }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("package/payload/bin/demo.ps1"), "# demo\n").unwrap();

    let (output, calls) = run(
        &root,
        &[
            "build",
            "--platform",
            "x86_64-pc-windows-msvc",
            "--stage",
            stage.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let bundle = calls
        .iter()
        .find(|argv| argv[0] == "bundle")
        .expect("bundle ran");
    assert_eq!(Path::new(&bundle[1]), stage.join("package"), "{bundle:?}");

    let bytes = std::fs::read(stage.join("package/manifest.json")).unwrap();
    let staged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(staged["interpreter"]["name"], "powershell");
    assert_eq!(staged["exec"]["path"], "payload/bin/demo.ps1");
    assert_eq!(staged["lifecycle"]["start"]["path"], "payload/bin/demo.ps1");
    // Whatever the overlay did not name still comes from the base.
    assert_eq!(staged["version"], "1.0.0");
    assert!(staged["capabilities"].is_array(), "{staged}");
    // The payload came along, and the overlay did not.
    assert!(stage.join("package/payload/bin/demo").is_file());
    assert!(stage.join("package/payload/bin/demo.ps1").is_file());
    assert!(!stage.join("package/platforms").exists());
    // The authored manifest is untouched, so the next platform still starts
    // from `sh`.
    assert_eq!(
        plugin_tools::read_manifest(&root).unwrap().exec.path,
        "payload/bin/demo"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// `id` and `version` name the artifact and the release, so they cannot differ
/// per platform; the build must stop before the host is called.
#[test]
fn an_overlay_cannot_change_the_package_identity() {
    let root = project("overlay-identity", MANIFEST);
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/x86_64-pc-windows-msvc.json"),
        r#"{"version":"2.0.0"}"#,
    )
    .unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-pc-windows-msvc"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("must not change"), "{}", stderr(&output));
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// `--stage` is where the release pipeline tars the package from, so it cannot
/// hold several platforms at one path.
#[test]
fn a_stage_holds_one_package() {
    let root = project("stage-many", MANIFEST);
    let (output, calls) = run(
        &root,
        &["build", "--stage", root.join("dist/stage").to_str().unwrap()],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--stage"), "{}", stderr(&output));
    assert!(calls.is_empty(), "{calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// `MANIFEST` without the entry: what a plugin with a per-platform entry keeps
/// in `package/manifest.json`.
fn neutral_manifest() -> String {
    MANIFEST.replace(
        "  \"exec\": { \"path\": \"payload/bin/demo\", \"ready\": \"notify\" },\n",
        "",
    )
}

/// A neutral manifest plus one overlay per platform: each package gets its own
/// entry, and the authored manifest stays free of both.
#[test]
fn each_platform_gets_its_own_manifest() {
    let root = project("per-platform", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/x86_64-unknown-linux-gnu.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/x86_64-pc-windows-msvc.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("package/payload/bin/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("package/payload/bin/demo.ps1"), "# demo\n").unwrap();

    let staged_of = |tag: &str, platform: &str| -> serde_json::Value {
        let stage = root.join(format!("dist/{tag}"));
        let (output, _calls) = run(
            &root,
            &[
                "build",
                "--platform",
                platform,
                "--stage",
                stage.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let bytes = std::fs::read(stage.join("package/manifest.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    };

    let linux = staged_of("linux", "x86_64-unknown-linux-gnu");
    assert_eq!(linux["exec"]["path"], "payload/bin/demo.sh");
    assert_eq!(linux["interpreter"]["name"], "sh");
    // The neutral half still comes from `package/manifest.json`.
    assert_eq!(linux["id"], "demo");
    assert_eq!(linux["version"], "1.0.0");

    let windows = staged_of("windows", "x86_64-pc-windows-msvc");
    assert_eq!(windows["exec"]["path"], "payload/bin/demo.ps1");
    assert_eq!(windows["interpreter"]["name"], "powershell");

    let bytes = std::fs::read(root.join("package/manifest.json")).unwrap();
    let base: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        base.get("exec").is_none(),
        "the authored manifest must stay platform-neutral: {base}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The scripts under `platforms/` travel with their own family only: the other
/// one never enters the package, so nothing has to be filtered out later.
#[test]
fn platform_scripts_only_travel_with_their_family() {
    let root = project("platform-scripts", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/x86_64-unknown-linux-gnu.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/x86_64-pc-windows-msvc.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    // Neither script is in `package/`: the build has to bring them in.
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("platforms/demo.ps1"), "# demo\n").unwrap();

    for (tag, platform, mine, theirs) in [
        ("linux", "x86_64-unknown-linux-gnu", "demo.sh", "demo.ps1"),
        ("windows", "x86_64-pc-windows-msvc", "demo.ps1", "demo.sh"),
    ] {
        let stage = root.join(format!("dist/{tag}"));
        let (output, _calls) = run(
            &root,
            &[
                "build",
                "--platform",
                platform,
                "--stage",
                stage.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let bin = stage.join("package/payload/bin");
        assert!(bin.join(mine).is_file(), "{platform} needs {mine}");
        assert!(!bin.join(theirs).exists(), "{platform} must not ship {theirs}");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The same rule for `platforms/lib/`: a script project keeps both SDKs, and each
/// package gets only the one its script loads.
#[test]
fn platform_libs_only_travel_with_their_family() {
    let root = project("platform-libs", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms/lib")).unwrap();
    std::fs::write(
        root.join("platforms/unix.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/windows.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("platforms/demo.ps1"), "# demo\n").unwrap();
    std::fs::write(root.join("platforms/lib/svcd-plugin.sh"), "# unix sdk\n").unwrap();
    std::fs::write(root.join("platforms/lib/SvcdPlugin.ps1"), "# windows sdk\n").unwrap();

    for (tag, platform, mine, theirs) in [
        (
            "linux",
            "x86_64-unknown-linux-gnu",
            "svcd-plugin.sh",
            "SvcdPlugin.ps1",
        ),
        (
            "windows",
            "x86_64-pc-windows-msvc",
            "SvcdPlugin.ps1",
            "svcd-plugin.sh",
        ),
    ] {
        let stage = root.join(format!("dist/{tag}"));
        let (output, _calls) = run(
            &root,
            &[
                "build",
                "--platform",
                platform,
                "--stage",
                stage.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let lib = stage.join("package/payload/lib");
        assert!(lib.join(mine).is_file(), "{platform} needs {mine}");
        assert!(!lib.join(theirs).exists(), "{platform} must not ship {theirs}");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A library whose extension names no family belongs to no package, so it is
/// refused instead of shipped to everyone or silently dropped.
#[test]
fn a_library_without_a_family_is_refused() {
    let root = project("lib-no-family", &MANIFEST);
    std::fs::create_dir_all(root.join("platforms/lib")).unwrap();
    std::fs::write(root.join("platforms/lib/README.md"), "notes\n").unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("platforms/lib/README.md"), "{text}");
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A `script` project keeps everything platform-specific outside `package/`, so
/// its payload only exists once the stage has picked this platform's half. The
/// "would ship nothing" guard has to judge the stage, not the source tree.
#[test]
fn a_payload_that_only_exists_after_staging_is_allowed() {
    let root = project("late-payload", &neutral_manifest());
    // Nothing in `package/` but the manifest: headerless UI, no entry yet.
    std::fs::remove_dir_all(root.join("package/payload")).unwrap();
    std::fs::create_dir_all(root.join("platforms/lib")).unwrap();
    std::fs::write(
        root.join("platforms/unix.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/windows.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("platforms/demo.ps1"), "# demo\n").unwrap();
    std::fs::write(root.join("platforms/lib/svcd-plugin.sh"), "# unix sdk\n").unwrap();
    std::fs::write(root.join("platforms/lib/SvcdPlugin.ps1"), "# windows sdk\n").unwrap();

    let stage = root.join("dist/linux");
    let (output, _calls) = run(
        &root,
        &[
            "build",
            "--platform",
            "x86_64-unknown-linux-gnu",
            "--stage",
            stage.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let package = stage.join("package");
    assert!(package.join("payload/bin/demo.sh").is_file());
    assert!(package.join("payload/lib/svcd-plugin.sh").is_file());
    assert!(!package.join("payload/lib/SvcdPlugin.ps1").exists());
    let _ = std::fs::remove_dir_all(root);
}

/// The guard still fires when there is nothing to ship even after staging.
#[test]
fn a_package_with_no_payload_at_all_is_still_refused() {
    let root = project("empty-payload", &MANIFEST);
    std::fs::remove_dir_all(root.join("package/payload")).unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("is empty"), "{}", stderr(&output));
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A stage whose manifest names a payload file nothing supplied must stop the
/// build with the platform and the path named, not install and fail later.
#[test]
fn a_stage_missing_its_entry_names_the_platform() {
    let root = project("missing-entry", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/x86_64-pc-windows-msvc.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-pc-windows-msvc"]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("payload/bin/demo.ps1"), "{text}");
    assert!(text.contains("x86_64-pc-windows-msvc"), "{text}");
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// The same check applies when no overlay was merged: a plugin can name the POSIX
/// script in `package/manifest.json` and still be built for Windows, where that
/// script never travels.
#[test]
fn a_staged_copy_without_an_overlay_still_checks_its_entry() {
    let root = project(
        "base-missing-entry",
        &MANIFEST.replace("payload/bin/demo", "payload/bin/demo.sh"),
    );
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    // Only the unix half exists, so a Windows stage brings no script at all.
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    let stage = root.join("dist/windows");
    let (output, calls) = run(
        &root,
        &[
            "build",
            "--platform",
            "x86_64-pc-windows-msvc",
            "--stage",
            stage.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("payload/bin/demo.sh"), "{text}");
    assert!(text.contains("x86_64-pc-windows-msvc"), "{text}");
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A family overlay serves every triple in it: three platforms that differ in
/// nothing do not need three files.
#[test]
fn one_family_overlay_serves_every_platform_in_it() {
    let root = project("family-overlay", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/unix.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/windows.json"),
        r#"{
  "interpreter": { "name": "powershell", "args": ["-NoProfile", "-File"] },
  "exec": { "path": "payload/bin/demo.ps1", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("platforms/demo.ps1"), "# demo\n").unwrap();

    for (tag, platform, entry, interpreter) in [
        ("gnu", "x86_64-unknown-linux-gnu", "payload/bin/demo.sh", "sh"),
        ("mac", "aarch64-apple-darwin", "payload/bin/demo.sh", "sh"),
        (
            "windows",
            "x86_64-pc-windows-msvc",
            "payload/bin/demo.ps1",
            "powershell",
        ),
    ] {
        let stage = root.join(format!("dist/{tag}"));
        let (output, _calls) = run(
            &root,
            &[
                "build",
                "--platform",
                platform,
                "--stage",
                stage.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let bytes = std::fs::read(stage.join("package/manifest.json")).unwrap();
        let staged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(staged["exec"]["path"], entry, "{platform}");
        assert_eq!(staged["interpreter"]["name"], interpreter, "{platform}");
        // The identity half still comes from the shared manifest.
        assert_eq!(staged["id"], "demo");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A triple overlay is the exception layer: it lands on top of its family.
#[test]
fn a_triple_overlay_wins_over_its_family() {
    let root = project("triple-over-family", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(
        root.join("platforms/unix.json"),
        r#"{
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/bin/demo.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("platforms/x86_64-unknown-linux-gnu.json"),
        r#"{
  "exec": { "path": "payload/bin/demo-gnu.sh", "ready": "notify" }
}"#,
    )
    .unwrap();
    std::fs::write(root.join("platforms/demo.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(root.join("platforms/demo-gnu.sh"), "#!/bin/sh\n").unwrap();

    let stage = root.join("dist/gnu");
    let (output, _calls) = run(
        &root,
        &[
            "build",
            "--platform",
            "x86_64-unknown-linux-gnu",
            "--stage",
            stage.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let bytes = std::fs::read(stage.join("package/manifest.json")).unwrap();
    let staged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(staged["exec"]["path"], "payload/bin/demo-gnu.sh");
    // The family layer is still under it.
    assert_eq!(staged["interpreter"]["name"], "sh");
    let _ = std::fs::remove_dir_all(root);
}

/// An overlay named after neither a triple nor a family would never be read, so
/// it is a typo and stops the build.
#[test]
fn an_overlay_named_after_nothing_is_refused() {
    let root = project("bogus-overlay", &neutral_manifest());
    std::fs::create_dir_all(root.join("platforms")).unwrap();
    std::fs::write(root.join("platforms/freebsd.json"), "{}").unwrap();
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("platforms/freebsd.json"), "{text}");
    // The message has to say what a legal name looks like.
    assert!(text.contains("unix") && text.contains("x86_64-pc-windows-msvc"), "{text}");
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A neutral manifest with no overlay supplies no entry at all, and the error
/// has to say where the entry belongs.
#[test]
fn a_manifest_without_an_entry_says_where_the_entry_goes() {
    let root = project("no-entry", &neutral_manifest());
    let (output, calls) = run(&root, &["build", "--platform", "x86_64-unknown-linux-gnu"]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("platforms/<triple>.json"), "{text}");
    assert!(calls.is_empty(), "the host must not run: {calls:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// `--platform=x` is the spelling people type. Dropping the value turned the
/// build into "every platform in `.plugin.json`", one bundle call per triple.
#[test]
fn the_platform_flag_may_be_written_with_equals() {
    let root = project("equals-flag", &MANIFEST);
    let (output, calls) = run(&root, &["build", "--platform=x86_64-unknown-linux-gnu"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // The fake host is called twice: once to bundle, once to verify.
    let bundles: Vec<_> = calls.iter().filter(|call| call[0] == "bundle").collect();
    assert_eq!(bundles.len(), 1, "one platform means one package: {calls:?}");
    assert!(
        bundles[0].iter().any(|arg| arg == "x86_64-unknown-linux-gnu"),
        "{:?}",
        bundles[0]
    );
    let _ = std::fs::remove_dir_all(root);
}

/// An option nobody reads is said out loud instead of being dropped.
#[test]
fn an_unknown_option_is_reported() {
    let root = project("unknown-flag", &MANIFEST);
    let (output, _calls) = run(
        &root,
        &["build", "--platform", "x86_64-unknown-linux-gnu", "--bogus"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("ignoring unknown option --bogus"),
        "{}",
        stderr(&output)
    );
    let _ = std::fs::remove_dir_all(root);
}
