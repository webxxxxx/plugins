// __NAME__ - a svcd plugin.
//
// Only the capability bodies below are yours: the protocol shell handles the
// readiness probe, the shutdown request, framing, concurrency and the MCP
// result shape. Keep the capability list here identical to
// `package/manifest.json`; the test at the bottom fails the build if it drifts.
//
// This template speaks protocol 3: stream 0 carries the JSON-RPC (calls,
// notifications, ping) and streams >= 1 carry raw bytes for `/raw` subscribers,
// all over the same stdin/stdout.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use svcd_plugin::multiplex::{self, RawPlugin, Streams};
use svcd_plugin::{log_info, log_warn, Context, Exit, Plugin, ToolOutcome};

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

/// Protocol 3 only: a raw frame arrived on `stream` (>= 1).
impl RawPlugin for __NAME_PASCAL__ {
    fn raw(&mut self, stream: u32, payload: &[u8], out: &Streams) {
        // One request, a short burst back, then close: the host turns the close
        // into a clean WebSocket close (1000) for this stream's /raw subscriber.
        let text = String::from_utf8_lossy(payload);
        for step in 1..=3 {
            let frame = format!("tick {step}/3 on stream {stream}: {text}");
            if let Err(error) = out.send(stream, frame.as_bytes()) {
                log_warn!("raw stream {stream}: {error}");
                break;
            }
        }
        if let Err(error) = out.close(stream) {
            log_warn!("raw stream {stream}: {error}");
        }
    }

    fn raw_closed(&mut self, stream: u32) {
        // The /raw subscriber left: stop producing for that stream.
        log_info!("host closed raw stream {stream}");
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

    multiplex::serve(context, __NAME_PASCAL__ { ticks })
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
