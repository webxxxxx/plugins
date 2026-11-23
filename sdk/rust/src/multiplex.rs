//! Protocol 3: one stdin/stdout carries JSON-RPC **and** raw streams.
//!
//! A protocol-3 plugin exchanges frames with the host:
//!
//! ```text
//!   u32 payload_len (big endian)
//!   u32 stream_id   (big endian)
//!   payload
//! ```
//!
//! Stream 0 is the JSON-RPC channel — the same `svcd.ping`, `svcd.status`,
//! `svcd.shutdown`, `tools/call` and `notifications/*` the protocol-1 loop
//! serves, with the outer length as the framing. Streams >= 1 are opaque bytes:
//! the host pairs each one with a `/raw` WebSocket subscriber, so a plugin can
//! answer calls, push to the UI and stream binary data from one process (#265).
//!
//! What this module guarantees:
//!
//! * **The read loop never blocks on a raw write.** Raw frames go through a
//!   bounded queue drained by its own thread; [`Streams::send`] answers
//!   [`SendError::Busy`] instead of stalling the reader when the host is behind.
//!   JSON replies have their own bound and are drained first, so a raw flood
//!   cannot delay a ping; only a host that stopped reading stdout entirely can
//!   fill the JSON queue.
//! * **A frame never goes out half-written.** A payload above the limit is
//!   rejected before the header is written.
//! * **Raw never starves JSON.** The JSON queue keeps its own bound and is
//!   drained first, so a raw flood cannot delay a reply.
//! * **The read loop never waits for a capability.** Calls run on the shared
//!   worker pool and raw frames on their own thread. They still share the
//!   plugin object, so the next raw frame waits for a running call — the lock
//!   is the isolation boundary, not a second copy of the plugin.
//!
//! Inbound raw frames are dropped when the plugin's raw thread is
//! [`RAW_QUEUE_DEPTH`] frames behind, because blocking the read loop would
//! stall every stream-0 call. The host drops a slow subscriber's frames the
//! same way; nothing is corrupted, but the bytes are gone.
//!
//! Stream lifetime is the one place where this SDK is ahead of the host: the
//! landed protocol-3 contract has no control frame, so a closed subscriber is
//! invisible to the plugin and frames written to it are dropped by the host.
//! The runtime therefore reserves the notification pair
//! [`STREAM_CLOSED`] (host → plugin: [`RawPlugin::raw_closed`]) and
//! [`STREAM_CLOSE`] (plugin → host: [`Streams::close`]) and treats both as best
//! effort until the host implements them.

use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::runtime::{Action, Pool};
use crate::{install_panic_hook, install_sink, Sink};
use crate::{Context, Exit, Plugin, STOP_BUDGET};

/// Bytes in front of every payload: `u32 len | u32 stream_id`.
pub const HEADER: usize = 8;
/// Stream 0 is the JSON-RPC channel; everything at or above this is raw.
pub const JSON_STREAM: u32 = 0;
/// Largest stream-0 payload, matching the host's `CALL_FRAME_MAX`.
pub const JSON_FRAME_MAX: usize = 1024 * 1024;
/// Largest raw payload the host accepts by default (`ws_raw_max_bytes`).
pub const RAW_FRAME_MAX: usize = 1024 * 1024;
/// How many raw frames may wait for the writer before [`SendError::Busy`].
pub const RAW_QUEUE_DEPTH: usize = 64;
/// How many JSON frames may wait before the writer. Answered calls are only
/// this numerous, so the bound is a safety net, not a backpressure signal.
pub const JSON_QUEUE_DEPTH: usize = 256;

/// Host → plugin: the subscriber on `stream` went away.
pub const STREAM_CLOSED: &str = "svcd.stream.closed";
/// Plugin → host: stop delivering `stream`; nothing more will be produced.
pub const STREAM_CLOSE: &str = "svcd.stream.close";

/// Per-stream payload limits. The host derives both from its own configuration;
/// raise [`Limits::raw_frame_max`] only when the host's `ws_raw_max_bytes` was
/// raised too, or the host drops the frame after the SDK accepted it.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub json_frame_max: usize,
    pub raw_frame_max: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            json_frame_max: JSON_FRAME_MAX,
            raw_frame_max: RAW_FRAME_MAX,
        }
    }
}

/// Why a raw frame was not handed to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Stream 0 belongs to the runtime; push notifications with [`crate::emit`].
    StreamZero,
    /// The payload is above the limit for that stream. Nothing was written.
    TooLarge { len: usize, max: usize },
    /// The host is not draining fast enough. Retry later; do not block.
    Busy,
    /// The runtime stopped, or the host said this stream is closed.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::StreamZero => {
                write!(formatter, "stream 0 is the JSON-RPC channel, not raw")
            }
            SendError::TooLarge { len, max } => {
                write!(
                    formatter,
                    "frame is {len} bytes, above the {max} byte limit"
                )
            }
            SendError::Busy => write!(formatter, "raw stream is busy (host is behind)"),
            SendError::Closed => write!(formatter, "stream is closed"),
        }
    }
}

impl std::error::Error for SendError {}

/// A cloneable handle to the raw side of one session.
///
/// The runtime hands one to every [`RawPlugin::raw`] call, so a worker the
/// plugin spawns itself can keep pushing frames on the same stream.
#[derive(Clone)]
pub struct Streams {
    queue: Arc<OutQueue>,
    limits: Limits,
}

impl Streams {
    /// Queue one frame on a raw stream (`stream >= 1`).
    pub fn send(&self, stream: u32, payload: impl AsRef<[u8]>) -> Result<(), SendError> {
        if stream == JSON_STREAM {
            return Err(SendError::StreamZero);
        }
        let payload = payload.as_ref();
        if payload.len() > self.limits.raw_frame_max {
            return Err(SendError::TooLarge {
                len: payload.len(),
                max: self.limits.raw_frame_max,
            });
        }
        self.queue.push_raw(stream, encode(stream, payload))
    }

    /// Queue one stream-0 JSON-RPC message the plugin wants to send on its own
    /// (a notification, a progress event). Replies are written by the runtime;
    /// the frame header and the stream-0 id are added for you.
    pub fn send_json(&self, value: &Value) -> Result<(), SendError> {
        self.json(&value.to_string())
    }

    /// Tell the host this stream is done, and refuse further frames on it.
    ///
    /// The close is queued behind the frames already accepted for this stream,
    /// so the host sees the burst and only then the notification. The
    /// notification itself is best effort: the host does not implement
    /// [`STREAM_CLOSE`] yet, but the local stream is closed either way.
    pub fn close(&self, stream: u32) -> Result<(), SendError> {
        if stream == JSON_STREAM {
            return Err(SendError::StreamZero);
        }
        let notice = json!({
            "jsonrpc": "2.0",
            "method": STREAM_CLOSE,
            "params": { "stream": stream },
        })
        .to_string();
        // Queue first: a stream whose close could not be announced must not be
        // dead locally with nothing on the wire.
        self.queue
            .push_close(stream, encode(JSON_STREAM, notice.as_bytes()))?;
        self.queue.mark_closed(stream);
        Ok(())
    }

    /// Queue one stream-0 JSON frame. Used by every reply and by [`crate::emit`].
    pub(crate) fn json(&self, text: &str) -> Result<(), SendError> {
        if text.len() > self.limits.json_frame_max {
            return Err(SendError::TooLarge {
                len: text.len(),
                max: self.limits.json_frame_max,
            });
        }
        self.queue.push_json(encode(JSON_STREAM, text.as_bytes()))
    }

    /// The host told us the subscriber is gone: remember it so later writes are
    /// refused instead of silently dropped.
    fn host_closed(&self, stream: u32) {
        self.queue.mark_closed(stream);
    }
}

/// What a protocol-3 plugin implements on top of [`Plugin`].
pub trait RawPlugin: Plugin {
    /// One raw frame arrived on `stream` (`>= 1`). Runs on the raw thread, never
    /// on the read loop, so a slow body delays the next raw frame but not ping,
    /// status, shutdown or `tools/call`.
    fn raw(&mut self, stream: u32, payload: &[u8], out: &Streams);

    /// The host said the subscriber on `stream` went away.
    ///
    /// Fires only when the host sends [`STREAM_CLOSED`]; the landed protocol-3
    /// contract has no close frame, so today this is a reserved hook. Do not
    /// make production depend on it — design producers so they stop on their own
    /// (for example: produce a burst per request instead of pushing forever).
    fn raw_closed(&mut self, _stream: u32) {}
}

/// Serve protocol 3 over stdin/stdout until the host stops or stdin closes.
pub fn serve<P: RawPlugin>(context: Context, plugin: P) -> Exit {
    serve_with(context, plugin, Limits::default())
}

/// [`serve`] with explicit limits (for a host that raised `ws_raw_max_bytes`).
pub fn serve_with<P: RawPlugin>(context: Context, plugin: P, limits: Limits) -> Exit {
    serve_on(io::stdin(), io::stdout(), context, plugin, limits)
}

/// Serve protocol 3 over a reader/writer pair. Public for tests and for
/// embeddings that do not own the process stdio.
pub fn serve_on<P, R, W>(reader: R, writer: W, context: Context, plugin: P, limits: Limits) -> Exit
where
    P: RawPlugin,
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    install_panic_hook();

    let queue = Arc::new(OutQueue::new());
    let streams = Streams {
        queue: Arc::clone(&queue),
        limits,
    };
    install_sink(Sink::Multiplex(streams.clone()));

    let writer_queue = Arc::clone(&queue);
    let writer = thread::Builder::new()
        .name(format!("{}-write", context.id()))
        .spawn(move || writer_loop(writer, writer_queue));
    let writer = match writer {
        Ok(handle) => handle,
        Err(error) => {
            crate::log_error!("cannot start the writer: {error}");
            return;
        }
    };

    let plugin = Arc::new(Mutex::new(plugin));
    let pool = Pool::new(Arc::clone(&plugin), context.id());
    let (raw_sender, raw_receiver) = mpsc::sync_channel::<RawJob>(RAW_QUEUE_DEPTH);
    let raw_thread = {
        let plugin = Arc::clone(&plugin);
        let streams = streams.clone();
        thread::Builder::new()
            .name(format!("{}-raw", context.id()))
            .spawn(move || raw_loop(plugin, raw_receiver, streams))
    };

    let session = Session {
        context: &context,
        pool: &pool,
        plugin: &plugin,
        streams: &streams,
        raw: &raw_sender,
        limits,
    };
    session.serve(reader);

    // Stop accepting work, give the accepted calls the stop budget, then let the
    // raw thread and the writer drain what they already hold.
    context.cancel().cancel();
    pool.drain(STOP_BUDGET);
    drop(raw_sender);
    if let Ok(handle) = raw_thread {
        let _ = handle.join();
    }
    queue.close();
    let _ = writer.join();
}

enum RawJob {
    Frame(u32, Vec<u8>),
    Closed(u32),
}

impl RawJob {
    fn stream(&self) -> u32 {
        match self {
            RawJob::Frame(stream, _) | RawJob::Closed(stream) => *stream,
        }
    }
}

/// One serving session: everything the read loop and the dispatcher need, so
/// neither has to carry six arguments around.
struct Session<'a, P: RawPlugin> {
    context: &'a Context,
    pool: &'a Pool,
    plugin: &'a Arc<Mutex<P>>,
    streams: &'a Streams,
    raw: &'a SyncSender<RawJob>,
    limits: Limits,
}

impl<P: RawPlugin> Session<'_, P> {
    fn serve<R: Read>(&self, reader: R) {
        let mut frames = Frames::new(reader);
        loop {
            match frames.next(&self.limits) {
                Ok(Next::Eof) => return,
                Ok(Next::Frame(stream, payload)) => {
                    if stream == JSON_STREAM {
                        if self.handle_json(&payload) {
                            return;
                        }
                    } else if let Err(error) = self.raw.try_send(RawJob::Frame(stream, payload)) {
                        // Dropping is the only option that keeps the read loop
                        // free; the host drops a slow subscriber's frames the
                        // same way.
                        note_dropped(&error);
                    }
                }
                Err(ReadError::Io(error)) => {
                    crate::log_error!("read: {error}");
                    return;
                }
                Err(ReadError::TooLarge {
                    stream: JSON_STREAM,
                    len,
                    max,
                }) => {
                    crate::log_error!("stream 0 frame is {len} bytes, above the {max} byte limit");
                    return;
                }
                Err(ReadError::TooLarge { stream, len, max }) => {
                    // The bytes were consumed, so the next frame is still aligned.
                    crate::log_error!(
                        "raw stream {stream} frame dropped: {len} bytes, above the {max} byte limit"
                    );
                }
            }
        }
    }

    /// Handle one stream-0 frame. Returns `true` when the loop must stop.
    fn handle_json(&self, payload: &[u8]) -> bool {
        let text = match std::str::from_utf8(payload) {
            Ok(text) => text,
            Err(_) => {
                crate::log_error!("stream 0 frame is not UTF-8");
                return true;
            }
        };
        let frame: Value = match serde_json::from_str(text) {
            Ok(frame) => frame,
            Err(error) => {
                crate::log_error!("stream 0 frame is not JSON: {error}");
                return true;
            }
        };
        if !frame.is_object() {
            crate::log_error!("stream 0 frame is not a JSON object");
            return true;
        }

        let mut on_notification = |method: &str, params: &Value| -> bool {
            if method != STREAM_CLOSED {
                return false;
            }
            if let Some(stream) = stream_param(params) {
                self.streams.host_closed(stream);
                if let Err(error) = self.raw.try_send(RawJob::Closed(stream)) {
                    crate::log_error!(
                        "stream {stream} closed by the host, but the raw thread is busy: {error}"
                    );
                }
            }
            true
        };

        match crate::runtime::dispatch(
            &frame,
            self.context,
            self.pool,
            self.plugin,
            &mut on_notification,
        ) {
            Action::Stop => true,
            Action::Continue => false,
        }
    }
}

fn stream_param(params: &Value) -> Option<u32> {
    let stream = params.get("stream").and_then(Value::as_u64)?;
    u32::try_from(stream)
        .ok()
        .filter(|stream| *stream != JSON_STREAM)
}

fn raw_loop<P: RawPlugin>(plugin: Arc<Mutex<P>>, receiver: Receiver<RawJob>, streams: Streams) {
    while let Ok(job) = receiver.recv() {
        let stream = job.stream();
        let mut plugin = match plugin.lock() {
            Ok(plugin) => plugin,
            Err(poisoned) => poisoned.into_inner(),
        };
        match job {
            RawJob::Frame(_, payload) => plugin.raw(stream, &payload, &streams),
            RawJob::Closed(_) => plugin.raw_closed(stream),
        }
    }
}

static DROPPED: AtomicU64 = AtomicU64::new(0);

fn note_dropped(error: &mpsc::TrySendError<RawJob>) {
    let total = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
    if total.is_power_of_two() {
        let stream = match error {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job.stream(),
        };
        crate::log_error!(
            "raw stream {stream} frames dropped so far: {total} (the plugin is behind)"
        );
    }
}

/// The bounded write side: JSON replies first, raw frames second, one writer
/// thread. Nothing here ever touches the plugin lock.
struct OutQueue {
    state: Mutex<OutState>,
    ready: Condvar,
}

/// One entry of the raw queue. A close travels in the same queue as the frames
/// it follows, so the host sees the burst and only then the notification — even
/// though JSON frames are otherwise drained first.
enum Out {
    Frame(Vec<u8>),
    Close(Vec<u8>),
}

struct OutState {
    json: VecDeque<Vec<u8>>,
    raw: VecDeque<Out>,
    closed_streams: BTreeSet<u32>,
    closed: bool,
}

impl OutQueue {
    fn new() -> OutQueue {
        OutQueue {
            state: Mutex::new(OutState {
                json: VecDeque::new(),
                raw: VecDeque::new(),
                closed_streams: BTreeSet::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, OutState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn push_json(&self, frame: Vec<u8>) -> Result<(), SendError> {
        let mut state = self.lock();
        while state.json.len() >= JSON_QUEUE_DEPTH && !state.closed {
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        if state.closed {
            return Err(SendError::Closed);
        }
        state.json.push_back(frame);
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    fn push_raw(&self, stream: u32, frame: Vec<u8>) -> Result<(), SendError> {
        let mut state = self.lock();
        if state.closed || state.closed_streams.contains(&stream) {
            return Err(SendError::Closed);
        }
        if state.raw.len() >= RAW_QUEUE_DEPTH {
            return Err(SendError::Busy);
        }
        state.raw.push_back(Out::Frame(frame));
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    fn push_close(&self, stream: u32, frame: Vec<u8>) -> Result<(), SendError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError::Closed);
        }
        if state.closed_streams.contains(&stream) {
            return Ok(());
        }
        if state.raw.len() >= RAW_QUEUE_DEPTH {
            return Err(SendError::Busy);
        }
        state.raw.push_back(Out::Close(frame));
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    fn mark_closed(&self, stream: u32) {
        self.lock().closed_streams.insert(stream);
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        drop(state);
        self.ready.notify_all();
    }

    /// Blocks until a frame is available or the queue is closed and empty.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.lock();
        loop {
            if let Some(frame) = state.json.pop_front() {
                return Some(frame);
            }
            if let Some(frame) = state.raw.pop_front() {
                return Some(match frame {
                    Out::Frame(frame) | Out::Close(frame) => frame,
                });
            }
            if state.closed {
                return None;
            }
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
}

fn writer_loop<W: Write>(mut out: W, queue: Arc<OutQueue>) {
    while let Some(frame) = queue.pop() {
        if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
            // stdout is gone: nothing can be delivered any more.
            queue.close();
            return;
        }
    }
}

fn encode(stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// One decoded frame.
enum Next {
    Frame(u32, Vec<u8>),
    /// The host closed stdin (or the header was cut) — a clean stop.
    Eof,
}

enum ReadError {
    Io(io::Error),
    /// The declared length is above the limit for that stream. For a raw stream
    /// the payload is skipped, so the reader stays aligned; for stream 0 the
    /// caller stops.
    TooLarge {
        stream: u32,
        len: usize,
        max: usize,
    },
}

/// Decodes `u32 len | u32 stream | payload` without ever allocating more than
/// the limit allows.
struct Frames<R> {
    reader: R,
}

impl<R: Read> Frames<R> {
    fn new(reader: R) -> Frames<R> {
        Frames { reader }
    }

    fn next(&mut self, limits: &Limits) -> Result<Next, ReadError> {
        let header = match read_header(&mut self.reader) {
            Ok(Some(header)) => header,
            Ok(None) => return Ok(Next::Eof),
            Err(error) => return Err(ReadError::Io(error)),
        };
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let stream = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        let max = if stream == JSON_STREAM {
            limits.json_frame_max
        } else {
            limits.raw_frame_max
        };
        if len > max {
            if stream != JSON_STREAM {
                skip(&mut self.reader, len).map_err(ReadError::Io)?;
            }
            return Err(ReadError::TooLarge { stream, len, max });
        }
        let mut payload = vec![0u8; len];
        self.reader
            .read_exact(&mut payload)
            .map_err(ReadError::Io)?;
        Ok(Next::Frame(stream, payload))
    }
}

/// Fill the 8-byte header. `Ok(None)` is a clean close *before* any byte; a
/// torn header is a protocol error, not a close.
fn read_header(reader: &mut impl Read) -> io::Result<Option<[u8; HEADER]>> {
    let mut header = [0u8; HEADER];
    let mut filled = 0;
    while filled < HEADER {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated frame header",
                ))
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(header))
}

/// Consume `len` bytes so the next header is still where the host put it.
fn skip(reader: &mut impl Read, len: usize) -> io::Result<()> {
    let mut buffer = [0u8; 8 * 1024];
    let mut left = len;
    while left > 0 {
        let take = left.min(buffer.len());
        reader.read_exact(&mut buffer[..take])?;
        left -= take;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streams() -> Streams {
        Streams {
            queue: Arc::new(OutQueue::new()),
            limits: Limits::default(),
        }
    }

    fn frame(stream: u32, payload: &[u8]) -> Vec<u8> {
        encode(stream, payload)
    }

    #[test]
    fn a_frame_is_length_then_stream_then_payload() {
        let bytes = frame(7, b"hi");
        assert_eq!(&bytes[..4], &2u32.to_be_bytes(), "payload length");
        assert_eq!(&bytes[4..8], &7u32.to_be_bytes(), "stream id");
        assert_eq!(&bytes[8..], b"hi");
    }

    #[test]
    fn reading_reports_the_stream_and_the_payload() {
        let mut bytes = frame(0, b"{}");
        bytes.extend_from_slice(&frame(3, b"raw"));
        let mut frames = Frames::new(&bytes[..]);

        match frames.next(&Limits::default()) {
            Ok(Next::Frame(0, payload)) => assert_eq!(payload, b"{}"),
            Ok(_) => panic!("expected a stream-0 frame"),
            Err(_) => panic!("expected a frame, not an error"),
        }
        match frames.next(&Limits::default()) {
            Ok(Next::Frame(3, payload)) => assert_eq!(payload, b"raw"),
            Ok(_) => panic!("expected a raw frame"),
            Err(_) => panic!("expected a frame, not an error"),
        }
        assert!(matches!(frames.next(&Limits::default()), Ok(Next::Eof)));
    }

    #[test]
    fn a_cut_payload_is_an_io_error_not_a_half_frame() {
        let bytes = frame(1, b"abcdef");
        let mut frames = Frames::new(&bytes[..bytes.len() - 2]);
        assert!(matches!(
            frames.next(&Limits::default()),
            Err(ReadError::Io(_))
        ));
    }

    #[test]
    fn a_stream_zero_frame_above_the_limit_is_refused() {
        let mut header = (JSON_FRAME_MAX as u32 + 1).to_be_bytes().to_vec();
        header.extend_from_slice(&0u32.to_be_bytes());
        let mut frames = Frames::new(&header[..]);
        match frames.next(&Limits::default()) {
            Err(ReadError::TooLarge {
                stream: 0,
                len,
                max,
            }) => {
                assert_eq!(len, JSON_FRAME_MAX + 1);
                assert_eq!(max, JSON_FRAME_MAX);
            }
            _ => panic!("an oversized stream-0 frame must be refused"),
        }
    }

    #[test]
    fn an_oversized_raw_frame_is_skipped_and_the_next_one_still_reads() {
        let mut bytes = Vec::new();
        let payload = vec![0u8; RAW_QUEUE_DEPTH + 16];
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&frame(2, b"next"));

        let limits = Limits {
            raw_frame_max: 8,
            ..Limits::default()
        };
        let mut frames = Frames::new(&bytes[..]);
        assert!(matches!(
            frames.next(&limits),
            Err(ReadError::TooLarge { stream: 2, .. })
        ));
        match frames.next(&limits) {
            Ok(Next::Frame(2, payload)) => assert_eq!(payload, b"next"),
            _ => panic!("the reader must stay aligned after a skipped frame"),
        }
    }

    #[test]
    fn a_clean_close_is_not_an_error() {
        let mut frames = Frames::new(&b""[..]);
        assert!(matches!(frames.next(&Limits::default()), Ok(Next::Eof)));
    }

    #[test]
    fn a_torn_header_is_a_protocol_error_not_a_close() {
        let mut frames = Frames::new(&b"\x00\x00\x00"[..]);
        assert!(matches!(
            frames.next(&Limits::default()),
            Err(ReadError::Io(_))
        ));
    }

    #[test]
    fn send_json_puts_the_frame_on_stream_zero() {
        let streams = streams();
        streams
            .send_json(&json!({ "jsonrpc": "2.0", "method": "notifications/message" }))
            .expect("a json frame fits");
        let frame = streams.queue.pop().expect("the frame");
        assert_eq!(&frame[4..8], &0u32.to_be_bytes(), "stream 0");
        let text = String::from_utf8(frame[HEADER..].to_vec()).expect("utf-8");
        assert!(text.contains("notifications/message"), "{text}");
    }

    #[test]
    fn closing_twice_keeps_one_notification() {
        let streams = streams();
        streams.close(9).expect("close");
        streams.close(9).expect("close is idempotent");
        let frame = streams.queue.pop().expect("one notification");
        assert_eq!(&frame[4..8], &0u32.to_be_bytes());
        assert!(
            streams.queue.lock().raw.is_empty(),
            "no second notification"
        );
    }

    #[test]
    fn stream_zero_is_not_a_raw_target() {
        assert_eq!(streams().send(0, b"x"), Err(SendError::StreamZero));
        assert_eq!(streams().close(0), Err(SendError::StreamZero));
    }

    #[test]
    fn a_payload_above_the_limit_is_refused_before_anything_is_written() {
        let streams = Streams {
            queue: Arc::new(OutQueue::new()),
            limits: Limits {
                raw_frame_max: 4,
                ..Limits::default()
            },
        };
        assert_eq!(
            streams.send(1, b"12345"),
            Err(SendError::TooLarge { len: 5, max: 4 })
        );
        assert!(streams.queue.lock().raw.is_empty(), "nothing may be queued");
    }

    #[test]
    fn a_full_raw_queue_reports_busy_without_blocking() {
        let streams = streams();
        for _ in 0..RAW_QUEUE_DEPTH {
            streams.send(1, b"x").expect("the queue has room");
        }
        assert_eq!(streams.send(1, b"x"), Err(SendError::Busy));

        // JSON keeps its own bound, so the replies a raw flood is blocking can
        // still be queued (and are drained first).
        streams.json("{}").expect("json is not the raw queue");
    }

    #[test]
    fn closing_a_stream_sends_the_notification_and_refuses_more_frames() {
        let streams = streams();
        streams.send(3, b"one").expect("before the close");

        streams.close(3).expect("close is queued");
        assert_eq!(streams.send(3, b"two"), Err(SendError::Closed));

        let raw = streams.queue.pop().expect("the raw frame");
        assert_eq!(
            &raw[4..8],
            &3u32.to_be_bytes(),
            "the raw frame kept its stream"
        );
        let notice = streams.queue.pop().expect("the close notification");
        assert_eq!(
            &notice[4..8],
            &0u32.to_be_bytes(),
            "the notification is stream 0"
        );
        let text = String::from_utf8(notice[HEADER..].to_vec()).expect("utf-8");
        assert!(text.contains(STREAM_CLOSE), "{text}");
        assert!(text.contains("\"stream\":3"), "{text}");
    }

    #[test]
    fn a_host_close_marks_the_stream_so_later_writes_are_refused() {
        let streams = streams();
        streams.host_closed(5);
        assert_eq!(streams.send(5, b"x"), Err(SendError::Closed));
    }

    #[test]
    fn the_stream_parameter_is_a_raw_stream_only() {
        assert_eq!(stream_param(&json!({ "stream": 4 })), Some(4));
        assert_eq!(stream_param(&json!({ "stream": 0 })), None);
        assert_eq!(stream_param(&json!({ "stream": "4" })), None);
        assert_eq!(stream_param(&json!({})), None);
    }
}
