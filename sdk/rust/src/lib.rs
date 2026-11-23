//! The protocol shell for `.spk` plugins.
//!
//! Two transports ship here:
//!
//! * [`serve`] speaks protocol 1: newline-delimited JSON-RPC 2.0 on
//!   stdin/stdout, the shape every template and sample used before protocol 3.
//! * [`multiplex::serve`] speaks protocol 3: `u32 len | u32 stream_id | payload`
//!   frames, with stream 0 carrying the same JSON-RPC and streams >= 1 carrying
//!   raw bytes. A protocol-3 plugin serves calls, notifications and `/raw` from
//!   one pipe.
//!
//! Both share this page's contract: framing and the 1 MiB cap, the readiness
//! probe, the `tools/call` shape, out-of-order replies keyed by id, the
//! cancellation token every background worker must observe, and keeping stdout
//! exclusively for the protocol.
//!
//! A plugin author implements [`Plugin`] (plus [`multiplex::RawPlugin`] when it
//! speaks protocol 3) and nothing else.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub mod blob;
pub mod file;
pub mod multiplex;
pub mod transfer;

mod runtime;

pub use blob::{BlobArg, BlobWriter};
pub use file::{file_argument, FileArg, PartReader};
pub use transfer::FileStore;

/// One JSON-RPC frame may not exceed this many bytes, excluding the newline.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// How many `tools/call` frames run at once.
pub const WORKER_THREADS: usize = 4;
/// How many `tools/call` frames may wait for a worker before the host is told off.
pub const QUEUE_DEPTH: usize = 64;
/// The graceful-stop budget the host gives before SIGKILL.
pub const STOP_BUDGET: Duration = Duration::from_secs(9);

/// The return type of a plugin's `main`. A plugin that wants a non-zero exit
/// status calls [`std::process::exit`] itself; a normal stop is exit code 0.
pub type Exit = ();

/// Cooperative cancellation shared by every worker.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Sleep, returning `true` if the token fired during the wait.
    ///
    /// Polls in 50 ms steps so a cancelled worker stops promptly without the
    /// plugin having to own a timer implementation.
    pub fn sleep(&self, duration: Duration) -> bool {
        let step = Duration::from_millis(50);
        let started = Instant::now();
        while !self.is_cancelled() && started.elapsed() < duration {
            let left = duration.saturating_sub(started.elapsed());
            thread::sleep(step.min(left));
        }
        self.is_cancelled()
    }

    /// A periodic ticker that stops when the token fires.
    pub fn ticker(&self, period: Duration) -> Ticker {
        Ticker {
            cancel: self.clone(),
            period,
            next: Instant::now(),
        }
    }
}

/// Periodic wait bound to a [`Cancel`].
pub struct Ticker {
    cancel: Cancel,
    period: Duration,
    next: Instant,
}

impl Ticker {
    /// Wait for the next tick. Returns `false` once the token is cancelled.
    pub fn wait(&mut self) -> bool {
        self.next += self.period;
        let now = Instant::now();
        if self.next > now && self.cancel.sleep(self.next - now) {
            return false;
        }
        if self.next < now {
            // Fell behind (a long handler): resync instead of bursting to catch up.
            self.next = now + self.period;
        }
        !self.cancel.is_cancelled()
    }
}

/// What the host told us about itself at spawn time.
pub struct Context {
    id: String,
    version: String,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    temp_dir: Option<PathBuf>,
    cancel: Cancel,
}

impl Context {
    /// Read the `SVCD_*` environment the host injects.
    ///
    /// Never fails: outside the host (a frame-level test, or running the binary
    /// by hand) the identity falls back and the paths stay absent.
    pub fn from_env() -> Self {
        let env = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        Context {
            id: env("SVCD_PLUGIN_ID")
                .or_else(|| env("SVCD_SERVICE_ID"))
                .unwrap_or_else(|| "plugin".to_string()),
            version: env("SVCD_PLUGIN_VERSION").unwrap_or_else(|| "0.0.0".to_string()),
            config: env("SVCD_CONFIG").map(PathBuf::from),
            data_dir: env("SVCD_DATA_DIR").map(PathBuf::from),
            log_dir: env("SVCD_LOG_DIR").map(PathBuf::from),
            temp_dir: env("SVCD_TEMP_DIR").map(PathBuf::from),
            cancel: Cancel::default(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The active (user-editable) config file, not the read-only template.
    pub fn config_path(&self) -> Option<&Path> {
        self.config.as_deref()
    }

    /// A writable directory that survives upgrades. Use it for state, not logs.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Where svcd writes this service's log. Do not write files here yourself.
    pub fn log_dir(&self) -> Option<&Path> {
        self.log_dir.as_deref()
    }

    /// The host's transient root (`SVCD_TEMP_DIR`). Blob results must be written
    /// under `incoming/` here; absent outside a host.
    pub fn temp_dir(&self) -> Option<&Path> {
        self.temp_dir.as_deref()
    }

    pub fn cancel(&self) -> Cancel {
        self.cancel.clone()
    }

    /// Parse the active config file. Missing file is not an error: it means the
    /// user has not edited the copy yet and the packaged template still applies.
    pub fn config_json(&self) -> Result<Option<Value>, String> {
        let Some(path) = self.config_path() else {
            return Ok(None);
        };
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|error| format!("{}: {}", path.display(), error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {}", path.display(), error)),
        }
    }
}

/// A capability's answer.
///
/// `Text` / `Error` become MCP content blocks; `Blob` becomes a `$blobPath`
/// result the host materializes into the content-addressed store.
pub enum ToolOutcome {
    Text(String),
    Error(String),
    /// A file the plugin wrote under `$SVCD_TEMP_DIR/incoming/`.
    Blob {
        path: PathBuf,
        name: String,
        mime_type: String,
    },
}

impl ToolOutcome {
    pub fn text(text: impl Into<String>) -> Self {
        ToolOutcome::Text(text.into())
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolOutcome::Error(text.into())
    }

    fn into_result(self) -> Value {
        match self {
            ToolOutcome::Text(text) => json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false
            }),
            ToolOutcome::Error(text) => json!({
                "content": [{ "type": "text", "text": text }],
                "isError": true
            }),
            ToolOutcome::Blob {
                path,
                name,
                mime_type,
            } => json!({
                "$blobPath": path.to_string_lossy(),
                "name": name,
                "mimeType": mime_type
            }),
        }
    }
}

/// What a plugin implements. This is the whole surface.
pub trait Plugin: Send + 'static {
    /// Capability names this plugin serves. Must equal `capabilities[].name` in
    /// `package/manifest.json`; the template ships a test that enforces it.
    const CAPABILITIES: &'static [&'static str];

    /// Run one capability. Called from a worker thread, never from the reader.
    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome;

    /// One line for `svc status`. Keep it cheap: it may run while a call is in
    /// flight, and then the shell reports `busy` instead.
    fn status(&self) -> Option<String> {
        None
    }
}

/// Serve protocol 1 until stdin closes or the host asks us to stop.
pub fn serve<P: Plugin>(context: Context, plugin: P) -> Exit {
    install_panic_hook();
    install_sink(Sink::Lines(Mutex::new(io::stdout())));

    let plugin = Arc::new(Mutex::new(plugin));
    let pool = runtime::Pool::new(Arc::clone(&plugin), context.id());

    read_loop(&context, &pool, &plugin);

    // Stop accepting work, tell the workers, and give them the stop budget to
    // fold their loops and finish what was already accepted. The host kills the
    // process group after 10 s regardless.
    context.cancel().cancel();
    pool.drain(STOP_BUDGET);
}

fn read_loop<P: Plugin>(context: &Context, pool: &runtime::Pool, plugin: &Arc<Mutex<P>>) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = line.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        if text.len() > MAX_FRAME_BYTES {
            reply_error(&Value::Null, -32700, "frame exceeds the 1 MiB limit");
            return;
        }
        let frame: Value = match serde_json::from_str(text) {
            Ok(frame) => frame,
            Err(_) => {
                reply_error(&Value::Null, -32700, "parse error");
                continue;
            }
        };
        if !frame.is_object() {
            reply_error(&Value::Null, -32700, "frame is not a JSON object");
            continue;
        }

        match runtime::dispatch(&frame, context, pool, plugin) {
            runtime::Action::Stop => return,
            runtime::Action::Continue => {}
        }
    }
}

pub(crate) fn status_text<P: Plugin>(plugin: &Arc<Mutex<P>>) -> String {
    match plugin.try_lock() {
        Ok(guard) => guard.status().unwrap_or_else(|| "running".to_string()),
        // A capability holds the lock; never block the probe behind it.
        Err(_) => "busy".to_string(),
    }
}

/// Push a JSON-RPC notification to the host. The host mirrors it to UI
/// subscribers over SSE and WebSocket, so this is how the browser hears things.
///
/// Never blocks on a slow UI: the host owns the queue (512 messages) and drops
/// the oldest when it overflows.
pub fn emit(method: &str, params: Value) {
    write_json(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

/// Read the `capabilities[].name` list out of a manifest, for the drift test the
/// template ships: the declared set must equal [`Plugin::CAPABILITIES`].
pub fn declared_capabilities(path: impl AsRef<Path>) -> Result<Vec<String>, String> {
    let path = path.as_ref();
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {}", path.display(), error))?;
    let manifest: Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {}", path.display(), error))?;
    let mut names = Vec::new();
    if let Some(list) = manifest.get("capabilities").and_then(Value::as_array) {
        for entry in list {
            if let Some(name) = entry.get("name").and_then(Value::as_str) {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

/// Where `emit` and every reply go. One sink per process: a plugin speaks one
/// protocol, and the runtime that starts it installs the sink before reading.
pub(crate) enum Sink {
    /// Protocol 1: one JSON value per line on stdout.
    Lines(Mutex<io::Stdout>),
    /// Protocol 3: stream-0 frames through the multiplex writer.
    Multiplex(multiplex::Streams),
}

static SINK: Mutex<Option<Arc<Sink>>> = Mutex::new(None);

pub(crate) fn install_sink(sink: Sink) {
    let mut slot = match SINK.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(Arc::new(sink));
}

fn sink() -> Option<Arc<Sink>> {
    let slot = match SINK.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.clone()
}

fn write_json(value: &Value) {
    let text = value.to_string();
    if text.contains('\n') || text.len() > MAX_FRAME_BYTES {
        log_error!("refusing to write an invalid frame");
        return;
    }
    let Some(sink) = sink() else {
        return;
    };
    match &*sink {
        Sink::Lines(writer) => {
            let mut out = match writer.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let _ = out.write_all(text.as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
        Sink::Multiplex(streams) => {
            let _ = streams.json(&text);
        }
    }
}

pub(crate) fn reply_result(id: &Value, result: Value) {
    if id.is_null() {
        return;
    }
    write_json(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn reply_error(id: &Value, code: i64, message: &str) {
    write_json(&json!({
        "jsonrpc": "2.0",
        "id": if id.is_null() { Value::Null } else { id.clone() },
        "error": { "code": code, "message": message }
    }));
}

pub(crate) fn install_panic_hook() {
    // A panicking handler must not spew a backtrace onto stdout: that would
    // corrupt the channel and the host would see a protocol violation.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[error] plugin panicked: {info}");
        previous(info);
    }));
}

/// Structured-ish logging. Everything goes to stderr, which svcd captures and
/// rotates into `<logs>/<id>.log`; a plugin must never write log files itself.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { eprintln!("[info] {}", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { eprintln!("[warn] {}", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { eprintln!("[error] {}", format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_maps_to_the_mcp_shape() {
        let ok = ToolOutcome::text("hi").into_result();
        assert_eq!(ok["isError"], json!(false));
        assert_eq!(ok["content"][0]["text"], json!("hi"));

        let bad = ToolOutcome::error("nope").into_result();
        assert_eq!(bad["isError"], json!(true));
    }

    #[test]
    fn ticker_stops_when_cancelled() {
        let cancel = Cancel::default();
        let mut ticker = cancel.ticker(Duration::from_secs(60));
        let waiter = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            waiter.cancel();
        });
        assert!(!ticker.wait(), "ticker must wake on cancel, not after 60 s");
    }

    #[test]
    fn cancel_sleep_reports_interruption() {
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(cancel.sleep(Duration::from_secs(30)));
    }
}
