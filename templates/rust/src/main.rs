// __NAME__ - a svcd plugin.
//
// Only the capability bodies below are yours: the protocol shell handles the
// readiness probe, the shutdown request, framing, concurrency and the MCP
// result shape. Keep the capability list here identical to
// `package/manifest.json`; the test at the bottom fails the build if it drifts.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use svcd_plugin::{Context, Exit, Plugin, ToolOutcome};

struct __NAME_PASCAL__ {
    ticks: Arc<AtomicU64>,
}

impl Plugin for __NAME_PASCAL__ {
    const CAPABILITIES: &'static [&'static str] = &["echo", "stats"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "echo" => {
                let message = arguments
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)");
                ToolOutcome::text(message)
            }
            // JSON, so `stats.outputSchema` in the manifest has something to
            // describe: undeclared outputs stay text and are passed through.
            "stats" => ToolOutcome::text(
                json!({
                    "ticks": self.ticks.load(Ordering::SeqCst),
                    "pid": std::process::id(),
                })
                .to_string(),
            ),
            other => ToolOutcome::error(format!("unknown tool: {other}")),
        }
    }

    fn status(&self) -> Option<String> {
        Some(format!("ticks={}", self.ticks.load(Ordering::SeqCst)))
    }
}

fn main() -> Exit {
    let context = Context::from_env();
    let ticks = Arc::new(AtomicU64::new(0));

    // A background worker that pushes to the UI. It observes the same
    // cancellation token as the protocol loop, so `plugin stop` ends it within
    // the stop budget instead of leaking it until SIGKILL.
    let worker_ticks = Arc::clone(&ticks);
    let cancel = context.cancel();
    let _ = std::thread::Builder::new()
        .name(format!("{}-ticker", context.id()))
        .spawn(move || {
            let mut ticker = cancel.ticker(Duration::from_secs(5));
            while ticker.wait() {
                let count = worker_ticks.fetch_add(1, Ordering::SeqCst) + 1;
                svcd_plugin::emit("notifications/message", json!({ "type": "tick", "count": count }));
            }
        });

    svcd_plugin::serve(context, __NAME_PASCAL__ { ticks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_capabilities_match_the_manifest() {
        let mut declared =
            svcd_plugin::declared_capabilities(concat!(env!("CARGO_MANIFEST_DIR"), "/package/manifest.json"))
                .expect("read the manifest");
        declared.sort();

        let mut implemented: Vec<String> = __NAME_PASCAL__::CAPABILITIES
            .iter()
            .map(|name| name.to_string())
            .collect();
        implemented.sort();

        assert_eq!(declared, implemented, "manifest and code disagree on capabilities");
    }
}
