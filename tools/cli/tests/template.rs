//! What `plugin-tools template` scaffolds, and the order it asks for things.
//!
//! The wizard is interactive when stdin and stdout are terminals, and takes
//! every parameter at once otherwise — that second path is what agents and CI
//! use, so `stdin` is deliberately `/dev/null` here: a single missing flag must
//! never turn into a wait for input.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const LINUX: &str = "x86_64-unknown-linux-musl";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("plugin-tools-template-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Run `template <name> <args>` with stdout/stderr captured and no stdin.
fn template(dir: &Path, name: &str, args: &[&str]) -> (Output, PathBuf) {
    let output = Command::new(env!("CARGO_BIN_EXE_plugin-tools"))
        .arg("template")
        .arg(name)
        .args(args)
        .arg("--dir")
        .arg(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (output, dir.join(name))
}

/// Like [`template`], but without the implicit `--dir`: the tests that check
/// where a project lands have to say it themselves.
fn template_raw(name: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_plugin-tools"))
        .arg("template")
        .arg(name)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json(path: &Path) -> serde_json::Value {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

/// A mixed platform set with `--lang script`: one POSIX script, one PowerShell
/// script, and one overlay per family pointing at its own half.
#[test]
fn a_script_project_gets_one_script_per_family() {
    let dir = scratch("script");
    let platforms = format!("{LINUX},{WINDOWS}");
    let (output, project) = template(
        &dir,
        "demo",
        &[
            "--platforms",
            &platforms,
            "--lang",
            "script",
            "--desc",
            "mixed scaffold",
            "--ui",
            "web",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    // Both halves are committed source; neither is in package/.
    assert!(project.join("platforms/demo.sh").is_file());
    assert!(project.join("platforms/demo.ps1").is_file());
    assert!(!project.join("package/payload/bin/demo.sh").exists());
    assert!(!project.join("package/payload/bin/demo.ps1").exists());
    // Both SDKs are vendored next to the scripts they serve: one per family, so
    // the build can send each package only the one it runs.
    assert!(project.join("platforms/lib/svcd-plugin.sh").is_file());
    assert!(project.join("platforms/lib/SvcdPlugin.ps1").is_file());
    assert!(
        !project.join("package/payload/lib").exists(),
        "an SDK inside package/ ships to every platform"
    );

    // The shared manifest says what the plugin is, not how to run it.
    let base = json(&project.join("package/manifest.json"));
    assert!(base.get("exec").is_none(), "{base}");
    assert!(base.get("interpreter").is_none(), "{base}");
    assert_eq!(base["id"], "demo");
    assert_eq!(base["desc"], "mixed scaffold");
    assert_eq!(base["ui"]["entry"], "index.html");

    let unix = json(&project.join("platforms/unix.json"));
    assert_eq!(unix["interpreter"]["name"], "sh");
    assert_eq!(unix["exec"]["path"], "payload/bin/demo.sh");

    let windows = json(&project.join("platforms/windows.json"));
    assert_eq!(windows["interpreter"]["name"], "powershell");
    assert!(windows["interpreter"]["args"]
        .as_array()
        .unwrap()
        .contains(&serde_json::Value::String("-File".into())));
    assert_eq!(windows["exec"]["path"], "payload/bin/demo.ps1");
    // Two platforms in one family share one overlay: the count follows families,
    // not platforms.
    let overlays = std::fs::read_dir(project.join("platforms"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().to_string_lossy().ends_with(".json"))
        .count();
    assert_eq!(overlays, 2, "one overlay per family, not per platform");
    let _ = std::fs::remove_dir_all(dir);
}

/// A single family keeps the entry in the shared manifest: there is nothing to
/// differ, and `pack` stays a plain copy of `package/`.
#[test]
fn a_single_family_keeps_one_manifest() {
    let dir = scratch("one-family");
    let (output, project) = template(&dir, "demo", &["--platforms", LINUX, "--lang", "shell"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let base = json(&project.join("package/manifest.json"));
    assert_eq!(base["exec"]["path"], "payload/bin/demo.sh");
    assert_eq!(base["interpreter"]["name"], "sh");
    assert!(!project.join("platforms").exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// The language list is filtered by the platforms, so a pair that cannot work
/// is refused with the allowed set in the message.
#[test]
fn a_language_that_cannot_target_the_platform_set_is_refused() {
    let dir = scratch("refused");

    let (output, _project) = template(&dir, "shellwin", &["--platforms", WINDOWS, "--lang", "shell"]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("shell"), "{text}");
    assert!(text.contains("powershell"), "{text}");

    let (output, _project) = template(
        &dir,
        "scriptunix",
        &["--platforms", LINUX, "--lang", "script"],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("script"), "{}", stderr(&output));

    // One manifest cannot name `powershell` and `pwsh` at once, so a mixed set
    // offers the static languages and `script` instead.
    let platforms = format!("{LINUX},{WINDOWS}");
    let (output, _project) = template(
        &dir,
        "pwshmixed",
        &["--platforms", &platforms, "--lang", "powershell"],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("script"), "{}", stderr(&output));

    let _ = std::fs::remove_dir_all(dir);
}

/// A fake template repository: the real `shell` template plus the SDK files
/// `vendor_sdk` copies, and a marker that proves where the project came from.
fn fake_template_repo(tag: &str) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = std::env::temp_dir().join(format!("plugin-tools-repo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (from, to) in [
        ("templates/shell", "templates/shell"),
        ("sdk/web", "sdk/web"),
        ("sdk/shell", "sdk/shell"),
    ] {
        copy_dir(&repo.join(from), &root.join(to));
    }
    std::fs::write(root.join("templates/shell/MARKER.txt"), "from the fake repo\n").unwrap();
    root
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `--conf` supplies every parameter, and a flag still wins over the file.
#[test]
fn a_config_file_supplies_the_parameters() {
    let dir = scratch("conf");
    let out = dir.join("generated");
    let conf = dir.join("plugin-tools.json");
    std::fs::write(
        &conf,
        format!(
            r#"{{ "dir": "{}", "lang": "shell", "platforms": ["{LINUX}"], "desc": "from conf", "ui": "gui" }}"#,
            out.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();

    // `dir` comes from the file: no `--dir` on the command line.
    let output = template_raw("demo", &["--conf", conf.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let project = out.join("demo");
    let base = json(&project.join("package/manifest.json"));
    assert_eq!(base["desc"], "from conf");
    // `ui` is the manifest's own `ui.type`, so `"gui"` lands in the manifest.
    assert_eq!(
        base["ui"],
        serde_json::json!({ "type": "gui", "entry": "index.html" })
    );
    assert!(project.join("package/payload/web/index.html").is_file());

    // The command line overrides the file, not the other way round.
    let elsewhere = dir.join("elsewhere");
    let output = template_raw(
        "demo",
        &[
            "--conf",
            conf.to_str().unwrap(),
            "--desc",
            "from flag",
            "--dir",
            elsewhere.to_str().unwrap(),
            "--force",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let moved = elsewhere.join("demo");
    assert!(moved.is_dir(), "--dir must win over the file");
    assert_eq!(json(&moved.join("package/manifest.json"))["desc"], "from flag");
    let _ = std::fs::remove_dir_all(dir);
}

/// `--template-repo` points the scaffold at another checkout instead of the
/// one found from the current directory.
#[test]
fn a_local_template_repo_is_used() {
    let dir = scratch("local-repo");
    let repo = fake_template_repo("local");
    let (output, project) = template(
        &dir,
        "demo",
        &[
            "--template-repo",
            repo.to_str().unwrap(),
            "--platforms",
            LINUX,
            "--lang",
            "shell",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(project.join("MARKER.txt").is_file(), "project came from the real repo");
    assert!(project.join("package/payload/lib/svcd-plugin.sh").is_file());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(repo);
}

/// A git URL is cloned into a cache directory and used from there.
#[test]
fn a_git_template_repo_is_cloned() {
    if !run_git(Path::new("."), &["--version"]) {
        eprintln!("skipping: git is not on PATH");
        return;
    }
    let dir = scratch("git-repo");
    let repo = fake_template_repo("git");
    assert!(run_git(&repo, &["init", "-q"]), "git init");
    assert!(run_git(&repo, &["add", "-A"]), "git add");
    assert!(
        run_git(
            &repo,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "-q",
                "-m",
                "template repo",
            ]
        ),
        "git commit"
    );
    let url = format!("file://{}", repo.display().to_string().replace('\\', "/"));

    let (output, project) = template(
        &dir,
        "demo",
        &[
            "--template-repo",
            &url,
            "--platforms",
            LINUX,
            "--lang",
            "shell",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(project.join("MARKER.txt").is_file());
    // The second run reuses the clone instead of cloning again.
    let (output, _) = template(
        &dir,
        "demo2",
        &[
            "--template-repo",
            &url,
            "--platforms",
            LINUX,
            "--lang",
            "shell",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(repo);
}

fn run_git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// A directory without `templates/` and `sdk/` is refused, not half-used.
#[test]
fn a_directory_that_is_not_a_template_repo_is_refused() {
    let dir = scratch("bad-repo");
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (output, _) = template(
        &dir,
        "demo",
        &[
            "--template-repo",
            empty.to_str().unwrap(),
            "--platforms",
            LINUX,
            "--lang",
            "shell",
        ],
    );
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("not a template repository"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A config file may hold only some keys: the rest still come from the command
/// line, and a flag always beats the file — including turning a boolean off.
#[test]
fn a_partial_config_leaves_the_rest_to_the_command_line() {
    let dir = scratch("partial-conf");
    let out = dir.join("generated");
    let conf = dir.join("plugin-tools.json");
    std::fs::write(
        &conf,
        format!(
            r#"{{ "dir": "{}", "ui": "web" }}"#,
            out.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();

    let output = template_raw(
        "demo",
        &[
            "--conf",
            conf.to_str().unwrap(),
            "--platforms",
            LINUX,
            "--lang",
            "shell",
            "--desc",
            "from flag",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let project = out.join("demo");
    let base = json(&project.join("package/manifest.json"));
    // `dir` and `ui` came from the file, the rest from the command line.
    assert_eq!(base["desc"], "from flag");
    assert_eq!(base["ui"]["entry"], "index.html");
    assert_eq!(base["interpreter"]["name"], "sh");

    // `--ui headerless` overrides the file's `ui`, so no page is shipped.
    let output = template_raw(
        "demo",
        &[
            "--conf",
            conf.to_str().unwrap(),
            "--platforms",
            LINUX,
            "--lang",
            "shell",
            "--ui",
            "headerless",
            "--force",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let base = json(&project.join("package/manifest.json"));
    assert_eq!(base["ui"], serde_json::json!({ "type": "headerless" }));
    assert!(!project.join("package/payload/web").exists());
    let _ = std::fs::remove_dir_all(dir);
}


/// `--ui headerless|web|gui` writes manage's `ui.type`; only the two page
/// kinds carry an `entry`, and only they ship `payload/web/`.
#[test]
fn the_ui_kind_is_the_manifests_ui_type() {
    let dir = scratch("ui");

    // `headerless`: no page, and the manifest says so without an entry.
    let (output, project) = template(
        &dir,
        "bare",
        &["--platforms", LINUX, "--lang", "shell", "--ui", "headerless"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!project.join("package/payload/web").exists());
    let base = json(&project.join("package/manifest.json"));
    assert_eq!(base["ui"], serde_json::json!({ "type": "headerless" }));
    assert!(base["ui"].get("entry").is_none(), "headerless has no entry");

    // `web`: the template's page plus the toolkit.
    let (output, project) = template(
        &dir,
        "browser",
        &["--platforms", LINUX, "--lang", "shell", "--ui", "web"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(project.join("package/payload/web/index.html").is_file());
    assert!(project.join("package/payload/web/plugin-ui.mjs").is_file());
    assert_eq!(
        json(&project.join("package/manifest.json"))["ui"],
        serde_json::json!({ "type": "web", "entry": "index.html" })
    );

    // `gui`: the same files, declared for the desktop WebView.
    let (output, project) = template(
        &dir,
        "desktop",
        &["--platforms", LINUX, "--lang", "shell", "--ui", "gui"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(project.join("package/payload/web/plugin-ui.mjs").is_file());
    assert_eq!(
        json(&project.join("package/manifest.json"))["ui"],
        serde_json::json!({ "type": "gui", "entry": "index.html" })
    );

    // Anything else is refused with the three values it takes.
    let (output, _) = template(
        &dir,
        "bogus",
        &["--platforms", LINUX, "--lang", "shell", "--ui", "none"],
    );
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("headerless, web or gui"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The three values are the only ones, and a bad one fails before anything is
/// written — an agent that mistypes must not be left with half a project.
#[test]
fn a_bad_ui_value_creates_nothing() {
    let dir = scratch("ui-bad");
    for value in ["none", "headless", "Web"] {
        let (output, project) = template(
            &dir,
            "demo",
            &["--platforms", LINUX, "--lang", "shell", "--ui", value],
        );
        assert!(!output.status.success(), "{value} must be refused");
        assert!(
            stderr(&output).contains("headerless, web or gui"),
            "{}",
            stderr(&output)
        );
        assert!(!project.exists(), "{value} must not create a project");
    }

    // The removed boolean flags point at their replacement instead of being
    // silently ignored (which would quietly ship a page-less plugin).
    for flag in ["--webui", "--no-ui"] {
        let (output, project) = template(
            &dir,
            "demo",
            &["--platforms", LINUX, "--lang", "shell", flag],
        );
        assert!(!output.status.success(), "{flag} must be refused");
        assert!(stderr(&output).contains("--ui web"), "{}", stderr(&output));
        assert!(!project.exists(), "{flag} must not create a project");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// `--dir` is relative to where the caller stands, not to the template repo. A
/// git repo is a clone in a temp cache, so a project created inside it would be
/// deleted by the next `--refresh`.
#[test]
fn a_relative_dir_resolves_against_the_current_directory() {
    let dir = scratch("relative-dir");
    let repo = fake_template_repo("relative-dir");
    let output = Command::new(env!("CARGO_BIN_EXE_plugin-tools"))
        .arg("template")
        .arg("demo")
        .args([
            "--template-repo",
            repo.to_str().unwrap(),
            "--platforms",
            LINUX,
            "--lang",
            "shell",
            "--dir",
            "made",
        ])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.join("made/demo/package/manifest.json").is_file());
    assert!(
        !repo.join("made").exists(),
        "the project must not land inside the template repo"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(repo);
}

/// Without a terminal there is nobody to ask, and guessing the host triple is
/// how an agent-built plugin ships for the wrong platform.
#[test]
fn a_missing_platforms_flag_never_guesses() {
    let dir = scratch("no-platforms");
    let output = template_raw(
        "demo",
        &["--dir", dir.to_str().unwrap(), "--lang", "shell"],
    );
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("--platforms"), "{text}");
    assert!(!dir.join("demo").exists(), "nothing is created");
    let _ = std::fs::remove_dir_all(dir);
}

