//! A fake host: real pipes, real frames, the real runtime.
//!
//! These tests drive [`multiplex::serve_on`] the way the daemon does — write a
//! frame, read a frame — so the wire format, the writer queue and the raw
//! thread are all exercised together. They are serialized because the runtime
//! installs the process-wide sink, exactly like a real plugin process.

use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use svcd_plugin::multiplex::{self, Limits, RawPlugin, SendError, Streams, CONTROL_STREAM};
use svcd_plugin::{Context, Plugin, ToolOutcome};

/// One plugin process owns the sink, so tests in this binary run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    match SERIAL.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct Demo;

impl Plugin for Demo {
    const CAPABILITIES: &'static [&'static str] = &["echo", "slow"];

    fn call(&mut self, name: &str, arguments: &Value) -> ToolOutcome {
        match name {
            "echo" => ToolOutcome::text(arguments["message"].as_str().unwrap_or("")),
            "slow" => {
                thread::sleep(Duration::from_millis(300));
                ToolOutcome::text("slow")
            }
            other => ToolOutcome::error(format!("unknown tool: {other}")),
        }
    }
}

impl RawPlugin for Demo {
    fn raw(&mut self, stream: u32, payload: &[u8], out: &Streams) {
        if payload == b"close-me" {
            svcd_plugin::emit(
                "notifications/message",
                json!({
                    "type": "closed-by-plugin",
                    "stream": stream,
                    "ok": out.close(stream).is_ok(),
                }),
            );
            return;
        }
        let mut reply = b"echo:".to_vec();
        reply.extend_from_slice(payload);
        let sent = out.send(stream, reply);
        svcd_plugin::emit(
            "notifications/message",
            json!({ "type": "raw", "stream": stream, "ok": sent.is_ok() }),
        );
    }

    fn raw_closed(&mut self, stream: u32) {
        svcd_plugin::emit(
            "notifications/message",
            json!({ "type": "closed", "stream": stream }),
        );
    }
}

struct Host {
    to_plugin: io::PipeWriter,
    frames: Receiver<(u32, Vec<u8>)>,
    serve: thread::JoinHandle<()>,
    reader: thread::JoinHandle<()>,
}

impl Host {
    fn start() -> Host {
        let (plugin_read, to_plugin) = io::pipe().expect("host to plugin pipe");
        let (mut host_read, from_plugin) = io::pipe().expect("plugin to host pipe");

        let (sender, frames) = mpsc::channel();
        let reader = thread::spawn(move || loop {
            let mut header = [0u8; 8];
            if host_read.read_exact(&mut header).is_err() {
                return;
            }
            let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
            let stream = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
            let mut payload = vec![0u8; len];
            if host_read.read_exact(&mut payload).is_err() {
                return;
            }
            if sender.send((stream, payload)).is_err() {
                return;
            }
        });

        let serve = thread::spawn(move || {
            multiplex::serve_on(
                plugin_read,
                from_plugin,
                Context::from_env(),
                Demo,
                Limits::default(),
            )
        });

        Host {
            to_plugin,
            frames,
            serve,
            reader,
        }
    }

    fn send(&mut self, stream: u32, payload: &[u8]) {
        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&stream.to_be_bytes());
        frame.extend_from_slice(payload);
        self.to_plugin.write_all(&frame).expect("write frame");
        self.to_plugin.flush().expect("flush");
    }

    fn send_json(&mut self, value: &Value) {
        self.send(0, value.to_string().as_bytes());
    }

    fn next(&self) -> (u32, Vec<u8>) {
        self.frames
            .recv_timeout(Duration::from_secs(5))
            .expect("a frame from the plugin")
    }

    fn next_json(&self) -> Value {
        let (stream, payload) = self.next();
        assert_eq!(stream, 0, "expected a stream-0 frame");
        serde_json::from_slice(&payload).expect("a JSON payload")
    }

    /// Close the host side and collect what the plugin wrote while draining.
    fn finish(self) -> Vec<(u32, Vec<u8>)> {
        drop(self.to_plugin);
        self.serve.join().expect("the runtime stopped");
        self.reader.join().expect("the reader stopped");
        self.frames.try_iter().collect()
    }
}

#[test]
fn ping_and_a_capability_share_stream_zero() {
    let _guard = serial();
    let mut host = Host::start();

    host.send_json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "svcd.ping" }));
    let pong = host.next_json();
    assert_eq!(pong["id"], json!(1));
    assert_eq!(pong["result"]["ok"], json!(true));

    host.send_json(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "echo", "arguments": { "message": "hi" } }
    }));
    let reply = host.next_json();
    assert_eq!(reply["id"], json!(2));
    assert_eq!(reply["result"]["content"][0]["text"], json!("hi"));

    host.finish();
}

#[test]
fn an_unknown_tool_is_answered_on_stream_zero() {
    let _guard = serial();
    let mut host = Host::start();

    host.send_json(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "nope" }
    }));
    let reply = host.next_json();
    assert_eq!(reply["error"]["code"], json!(-32602));

    host.send_json(&json!({ "jsonrpc": "2.0", "id": 4, "method": "no.such.method" }));
    assert_eq!(host.next_json()["error"]["code"], json!(-32601));

    host.finish();
}

#[test]
fn a_raw_frame_reaches_the_plugin_and_its_reply_keeps_the_stream() {
    let _guard = serial();
    let mut host = Host::start();

    host.send(4, b"hello");

    // The plugin replies on the same stream and emits a notification on stream 0.
    // The two channels are independent, so collect both instead of assuming an
    // order between them.
    let mut raw = None;
    let mut notice = None;
    while raw.is_none() || notice.is_none() {
        let (stream, payload) = host.next();
        if stream == 4 {
            raw = Some(payload);
        } else {
            let value: Value = serde_json::from_slice(&payload).expect("json");
            if value["params"]["type"] == json!("raw") {
                notice = Some(value);
            }
        }
    }
    assert_eq!(raw.unwrap(), b"echo:hello");
    assert_eq!(notice.unwrap()["params"]["stream"], json!(4));

    host.finish();
}

#[test]
fn a_slow_capability_does_not_hold_up_ping() {
    let _guard = serial();
    let mut host = Host::start();

    host.send_json(&json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": { "name": "slow" }
    }));
    host.send_json(&json!({ "jsonrpc": "2.0", "id": 11, "method": "svcd.ping" }));

    // The ping is answered while the capability is still sleeping.
    let first = host.next_json();
    assert_eq!(
        first["id"],
        json!(11),
        "ping must not queue behind the call"
    );

    let second = host.next_json();
    assert_eq!(second["id"], json!(10));
    assert_eq!(second["result"]["content"][0]["text"], json!("slow"));

    host.finish();
}

#[test]
fn a_control_close_from_the_host_reaches_the_plugin() {
    let _guard = serial();
    let mut host = Host::start();

    host.send(CONTROL_STREAM, br#"{"op":"close","stream":7}"#);

    let notice = host.next_json();
    assert_eq!(notice["params"]["type"], json!("closed"));
    assert_eq!(notice["params"]["stream"], json!(7));

    host.finish();
}

#[test]
fn the_plugin_can_close_a_stream_on_the_control_stream() {
    let _guard = serial();
    let mut host = Host::start();

    host.send(4, b"close-me");

    let mut notice = None;
    let mut control = None;
    while notice.is_none() || control.is_none() {
        let (stream, payload) = host.next();
        if stream == CONTROL_STREAM {
            control = Some(payload);
        } else {
            let value: Value = serde_json::from_slice(&payload).expect("json");
            if value["params"]["type"] == json!("closed-by-plugin") {
                notice = Some(value);
            }
        }
    }
    assert_eq!(notice.unwrap()["params"]["ok"], json!(true));
    let control: Value = serde_json::from_slice(&control.unwrap()).expect("json");
    assert_eq!(control, json!({ "op": "close", "stream": 4 }));

    host.finish();
}

#[test]
fn stream_zero_that_is_not_json_stops_the_loop() {
    let _guard = serial();
    let mut host = Host::start();

    host.send(0, b"not json");
    // The runtime stops instead of answering garbage with more garbage.
    host.finish();
}

fn wire(stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn parse_frames(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let stream =
            u32::from_be_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
        let end = at + 8 + len as usize;
        if end > bytes.len() {
            break;
        }
        frames.push((stream, bytes[at + 8..end].to_vec()));
        at = end;
    }
    frames
}

static FLOOD_SENT: AtomicUsize = AtomicUsize::new(0);
static FLOOD_BUSY: AtomicUsize = AtomicUsize::new(0);

/// Pushes far more raw bytes than the queue can hold, and counts what the
/// runtime accepted and refused.
struct Flood;

impl Plugin for Flood {
    const CAPABILITIES: &'static [&'static str] = &["echo"];

    fn call(&mut self, _name: &str, _arguments: &Value) -> ToolOutcome {
        ToolOutcome::text("ok")
    }
}

impl RawPlugin for Flood {
    fn raw(&mut self, stream: u32, payload: &[u8], out: &Streams) {
        if payload != b"flood" {
            return;
        }
        let frame = vec![b'x'; 4096];
        for _ in 0..200 {
            match out.send(stream, &frame) {
                Ok(()) => {
                    FLOOD_SENT.fetch_add(1, Ordering::SeqCst);
                }
                Err(SendError::Busy) => {
                    FLOOD_BUSY.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {}
            }
        }
    }
}

/// A writer that accepts nothing until the test opens the gate, so the writer
/// thread is wedged and the outbound queues fill up.
struct Gate {
    written: Arc<Mutex<Vec<u8>>>,
    open: Arc<(Mutex<bool>, Condvar)>,
}

impl Write for Gate {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let (lock, condvar) = &*self.open;
        let mut open = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*open {
            open = condvar
                .wait(open)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        self.written
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_full_raw_queue_still_answers_stream_zero() {
    let _guard = serial();
    let (plugin_read, mut to_plugin) = io::pipe().expect("host to plugin");
    let written = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer = Gate {
        written: Arc::clone(&written),
        open: Arc::clone(&gate),
    };

    let serve = thread::spawn(move || {
        multiplex::serve_on(
            plugin_read,
            writer,
            Context::from_env(),
            Flood,
            Limits::default(),
        )
    });

    // The raw queue can hold RAW_QUEUE_DEPTH frames; the writer is stuck, so
    // everything after that must come back as Busy instead of blocking.
    to_plugin.write_all(&wire(2, b"flood")).expect("raw frame");
    to_plugin.flush().expect("flush");

    let deadline = Instant::now() + Duration::from_secs(5);
    while FLOOD_SENT.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        FLOOD_BUSY.load(Ordering::SeqCst) > 0,
        "the raw queue must have refused frames"
    );

    // Stream 0 is answered while the raw queue is full and the writer is stuck.
    to_plugin
        .write_all(&wire(
            0,
            br#"{"jsonrpc":"2.0","id":21,"method":"svcd.ping"}"#,
        ))
        .expect("ping");
    to_plugin.flush().expect("flush");
    thread::sleep(Duration::from_millis(100));

    *gate
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    gate.1.notify_all();

    drop(to_plugin);
    serve.join().expect("serve");

    let frames = parse_frames(
        &written
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    let answered = frames.iter().any(|(stream, payload)| {
        *stream == 0 && String::from_utf8_lossy(payload).contains("\"id\":21")
    });
    assert!(answered, "stream 0 must be answered while raw is full");

    let sent = FLOOD_SENT.load(Ordering::SeqCst);
    assert_eq!(
        frames.iter().filter(|(stream, _)| *stream == 2).count(),
        sent,
        "every accepted raw frame is written"
    );
    assert!(sent < 200, "the rest were refused, not buffered: {sent}");
}
