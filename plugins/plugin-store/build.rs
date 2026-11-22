use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rustc-env=SVCD_PLUGIN_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );

    // A stale-but-offline snapshot: the UI always has something to show even
    // when the network (or GitHub) is unavailable on first start. The runtime
    // cache wins over this as soon as a refresh succeeds.
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plugins.md");
    println!("cargo:rerun-if-changed={}", docs.display());
    let snapshot = std::fs::read_to_string(&docs).unwrap_or_else(|_| {
        "# 插件下载\n\n| 插件 | 版本 | 平台 | 大小 | 下载地址 |\n| --- | --- | --- | ---: | --- |\n".into()
    });
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("catalog_snapshot.md");
    std::fs::write(out, snapshot).expect("write catalog snapshot");
}
