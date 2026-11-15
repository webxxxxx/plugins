//! hello — the reference plugin.
//!
//! This is what "everything the SDK offers" looks like in one place:
//!
//! * a config read with a sane fallback (`SVCD_CONFIG` may not exist yet);
//! * structured logging to stderr, which svcd captures and rotates;
//! * five capabilities, including the chunked file protocol the browser half
//!   (`sdk/web`) speaks;
//! * a background worker that observes the cancellation token, so `plugin stop`
//!   ends it inside the stop budget instead of leaking it until SIGKILL;
//! * notifications pushed to the UI over SSE / WebSocket.
//!
//! Copy it, delete the parts you do not need, keep the shape.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use svcd_plugin::{log_info, log_warn, Context, Exit, FileStore, Plugin, ToolOutcome};

struct Hello {
    files: FileStore,
    started: Instant,
    calls: Arc<AtomicU64>,
    ticks: Arc<AtomicU64>,
    greeting: String,
}

impl Hello {
    fn new(context: &Context) -> Self {
        // The config file is the user-editable copy; missing means "template
        // defaults still apply", which is not an error.
        let greeting = match context.config_json() {
            Ok(Some(config)) => config
                .get("greeting")
                .and_then(Value::as_str)
                .unwrap_or("hello")
                .to_string(),
            Ok(None) => "hello".to_string(),
            Err(error) => {
                log_warn!("config: {error}");
                "hello".to_string()
            }
        };

        Hello {
            files: FileStore::from_context(context),
            started: Instant::now(),
            calls: Arc::new(AtomicU64::new(0)),
            ticks: Arc::new(AtomicU64::new(0)),
            greeting,
        }
    }

    fn stats(&self) -> Value {
        json!({
            "greeting": self.greeting,
            "calls": self.calls.load(Ordering::SeqCst),
            "ticks": self.ticks.load(Ordering::SeqCst),
            "uptime_secs": self.started.elapsed().as_secs(),
        })
    }
}

impl Plugin for Hello {
    const CAPABILITIES: &'static [&'static str] =
        &["echo", "stats", "file_put", "file_get", "file_list"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match name {
            "echo" => match arguments.get("message").and_then(Value::as_str) {
                Some(message) => ToolOutcome::text(format!("{} {}", self.greeting, message)),
                None => ToolOutcome::error("echo: \"message\" is required"),
            },
            "stats" => ToolOutcome::text(self.stats().to_string()),
            "file_put" => self.files.put(arguments),
            "file_get" => self.files.get(arguments),
            "file_list" => self.files.list(),
            other => ToolOutcome::error(format!("unknown tool: {other}")),
        }
    }

    fn status(&self) -> Option<String> {
        Some(format!(
            "calls={} ticks={} uptime={}s",
            self.calls.load(Ordering::SeqCst),
            self.ticks.load(Ordering::SeqCst),
            self.started.elapsed().as_secs()
        ))
    }
}

fn main() -> Exit {
    let context = Context::from_env();
    let plugin = Hello::new(&context);
    log_info!(
        "{} {} starting (store: {})",
        context.id(),
        context.version(),
        plugin.files.root().display()
    );

    // A background worker. It holds the same cancellation token the protocol
    // loop does, so a shutdown request ends both within the stop budget.
    let ticks = Arc::clone(&plugin.ticks);
    let cancel = context.cancel();
    let _ = std::thread::Builder::new()
        .name(format!("{}-ticker", context.id()))
        .spawn(move || {
            let mut ticker = cancel.ticker(Duration::from_secs(5));
            while ticker.wait() {
                let count = ticks.fetch_add(1, Ordering::SeqCst) + 1;
                svcd_plugin::emit(
                    "notifications/message",
                    json!({ "type": "tick", "count": count }),
                );
            }
            log_info!("ticker stopped");
        });

    svcd_plugin::serve(context, plugin)
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

        let mut implemented: Vec<String> = Hello::CAPABILITIES
            .iter()
            .map(|name| name.to_string())
            .collect();
        implemented.sort();

        assert_eq!(declared, implemented, "manifest and code disagree on capabilities");
    }

    #[test]
    fn a_missing_message_is_an_error_not_a_panic() {
        let context = Context::from_env();
        let mut plugin = Hello::new(&context);
        match plugin.call("echo", &json!({})) {
            ToolOutcome::Error(error) => assert!(error.contains("message"), "{error}"),
            ToolOutcome::Text(_) => panic!("echo without a message should fail"),
        }
    }
}
