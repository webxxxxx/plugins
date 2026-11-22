//! Test fixture: a stand-in for the host's `plugin` CLI. `tests/build.rs`
//! compiles it with `rustc` on first use (std only — no crates to link).
//!
//! `plugin-tools` shells out to the real host for `bundle`/`verify`/`install`/
//! `start`, and the host lives in another repository. The integration tests
//! point `SVCD_PLUGIN_BIN` here and read back the argv this fixture was called
//! with.
//!
//! Every invocation appends one JSON line, `{"argv": [...]}`, to `$FAKE_HOST_LOG`;
//! `bundle` also writes a placeholder package where `-o` points so the toolchain
//! can finish and report the path.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log(&args);
    match args.first().map(String::as_str) {
        Some("bundle") => {
            let out = value_of(&args, "-o").unwrap_or_else(|| fail("bundle needs -o"));
            std::fs::write(out, b"fake package\n").unwrap_or_else(|error| fail(&error.to_string()));
        }
        Some("verify") | Some("install") | Some("start") => {}
        other => fail(&format!("unexpected verb {other:?}")),
    }
}

fn log(args: &[String]) {
    let Ok(path) = std::env::var("FAKE_HOST_LOG") else {
        return;
    };
    let quoted: Vec<String> = args.iter().map(|arg| quote(arg)).collect();
    let line = format!("{{\"argv\":[{}]}}\n", quoted.join(","));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| fail(&error.to_string()));
    file.write_all(line.as_bytes())
        .unwrap_or_else(|error| fail(&error.to_string()));
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).map(String::as_str)
}

fn fail(message: &str) -> ! {
    eprintln!("fake-host: {message}");
    std::process::exit(2);
}
