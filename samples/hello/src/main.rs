//! hello — the reference plugin.
//!
//! This is what "everything the SDK offers" looks like in one place:
//!
//! * a config read with a sane fallback (`SVCD_CONFIG` may not exist yet);
//! * structured logging to stderr, which svcd captures and rotates;
//! * four capabilities: two plain ones, one taking a **blob input** and one
//!   producing a **blob output** (see `plugin-manager/docs/plugin-blob.md`);
//! * a background worker that observes the cancellation token, so `plugin stop`
//!   ends it inside the stop budget instead of leaking it until SIGKILL;
//! * notifications pushed to the UI over SSE / WebSocket.
//!
//! Copy it, delete the parts you do not need, keep the shape.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use svcd_plugin::blob::{blob_argument, BlobWriter};
use svcd_plugin::file::file_argument;
use svcd_plugin::{log_info, log_warn, Context, Exit, Plugin, ToolOutcome};

/// Refuse to summarize a blob larger than this; the point is to stream, not to
/// pull a 512 MiB file into a summary.
const MAX_SUMMARY_BYTES: u64 = 64 * 1024 * 1024;

struct Hello {
    started: Instant,
    calls: Arc<AtomicU64>,
    ticks: Arc<AtomicU64>,
    greeting: String,
    /// `$SVCD_TEMP_DIR`, captured once: blob results must land in `incoming/`.
    temp: Option<PathBuf>,
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
            started: Instant::now(),
            calls: Arc::new(AtomicU64::new(0)),
            ticks: Arc::new(AtomicU64::new(0)),
            greeting,
            temp: context.temp_dir().map(PathBuf::from),
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

    /// Stream a blob and count lines and words without loading it into memory.
    fn summarize(&self, arguments: &Value) -> ToolOutcome {
        let blob = match blob_argument(arguments, "input") {
            Ok(Some(blob)) => blob,
            Ok(None) => {
                return ToolOutcome::error("summarize: \"input\" must be a blob argument ({$blob: …})")
            }
            Err(error) => return ToolOutcome::error(error),
        };
        if blob.size > MAX_SUMMARY_BYTES {
            return ToolOutcome::error(format!(
                "summarize: blob is {} bytes, above the {} byte summary limit",
                blob.size, MAX_SUMMARY_BYTES
            ));
        }

        let file = match blob.open() {
            Ok(file) => file,
            Err(error) => return ToolOutcome::error(error),
        };
        let mut lines = 0u64;
        let mut words = 0u64;
        let mut bytes = 0u64;
        let mut buffer = String::new();
        let mut reader = BufReader::new(file);
        loop {
            buffer.clear();
            match reader.read_line(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    lines += 1;
                    words += buffer.split_whitespace().count() as u64;
                    bytes += read as u64;
                }
                Err(error) => return ToolOutcome::error(format!("read blob: {error}")),
            }
        }

        ToolOutcome::text(format!(
            "{} bytes, {} lines, {} words ({})",
            bytes, lines, words, blob.id
        ))
    }

    /// Stream a file package (parts + manifest) and check it matches the size
    /// the host declared. This is the `$file` tier: no 512 MiB ceiling.
    fn inspect_file(&self, arguments: &Value) -> ToolOutcome {
        let file = match file_argument(arguments, "input") {
            Ok(Some(file)) => file,
            Ok(None) => {
                return ToolOutcome::error("inspect_file: \"input\" must be a file argument ({$file: …})")
            }
            Err(error) => return ToolOutcome::error(error),
        };
        let parts = match file.part_paths() {
            Ok(parts) => parts.len(),
            Err(error) => return ToolOutcome::error(error),
        };
        let mut reader = match file.open() {
            Ok(reader) => reader,
            Err(error) => return ToolOutcome::error(error),
        };

        let mut buffer = [0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => bytes += read as u64,
                Err(error) => return ToolOutcome::error(format!("read package: {error}")),
            }
        }
        if bytes != file.size {
            return ToolOutcome::error(format!(
                "read {bytes} bytes but the manifest says {}",
                file.size
            ));
        }
        ToolOutcome::text(format!(
            "{}: {} bytes in {} part(s) ({})",
            file.name, bytes, parts, file.id
        ))
    }

    /// Produce a small report as a blob the host stores and links.
    fn report(&self) -> ToolOutcome {
        let Some(temp) = self.temp.as_deref() else {
            return ToolOutcome::error("report: SVCD_TEMP_DIR is not set");
        };
        let mut writer = match BlobWriter::create_in(temp, "report.txt", "text/plain; charset=utf-8") {
            Ok(writer) => writer,
            Err(error) => return ToolOutcome::error(error),
        };
        let body = format!(
            "hello report\ngreeting: {}\ncalls: {}\nticks: {}\nuptime_secs: {}\n",
            self.greeting,
            self.calls.load(Ordering::SeqCst),
            self.ticks.load(Ordering::SeqCst),
            self.started.elapsed().as_secs()
        );
        if let Err(error) = writer.write_all(body.as_bytes()) {
            return ToolOutcome::error(error);
        }
        writer.finish()
    }
}

impl Plugin for Hello {
    const CAPABILITIES: &'static [&'static str] =
        &["echo", "stats", "summarize", "inspect_file", "report"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match name {
            "echo" => match arguments.get("message").and_then(Value::as_str) {
                Some(message) => ToolOutcome::text(format!("{} {}", self.greeting, message)),
                None => ToolOutcome::error("echo: \"message\" is required"),
            },
            "stats" => ToolOutcome::text(self.stats().to_string()),
            "summarize" => self.summarize(arguments),
            "inspect_file" => self.inspect_file(arguments),
            "report" => self.report(),
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
        "{} {} starting (temp: {})",
        context.id(),
        context.version(),
        context
            .temp_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string())
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
            _ => panic!("echo without a message should fail"),
        }
    }

    #[test]
    fn a_plain_argument_is_not_a_blob() {
        let context = Context::from_env();
        let mut plugin = Hello::new(&context);
        match plugin.call("summarize", &json!({ "input": "not a blob" })) {
            ToolOutcome::Error(error) => assert!(error.contains("blob argument"), "{error}"),
            _ => panic!("a plain string must be rejected"),
        }
    }
}
