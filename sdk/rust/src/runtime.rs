//! The `tools/call` worker pool both protocol loops share.
//!
//! Neither read loop runs a capability itself: it enqueues the call and goes
//! back to reading. That is what keeps `svcd.ping`, `svcd.status`, shutdown and
//! (on protocol 3) raw frames flowing while a slow capability is in flight.
//!
//! The pending counter is what makes a stop drain the queue instead of dropping
//! the tail of it: the loop stops accepting work, then waits for what it already
//! accepted, up to the host's stop budget.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::{reply_error, reply_result, status_text, Context, Plugin, QUEUE_DEPTH, WORKER_THREADS};

pub(crate) struct Job {
    pub id: Value,
    pub name: String,
    pub arguments: Value,
}

/// What happened to a submitted call. The caller owns the reply in every case.
pub(crate) enum Submit {
    Queued,
    /// The queue is at [`QUEUE_DEPTH`]: answer `-32603` instead of waiting.
    Full(Job),
    /// The workers are gone: stop the loop.
    Closed,
}

pub(crate) struct Pool {
    sender: SyncSender<Job>,
    /// Queued *and* running calls, so draining waits for both.
    pending: Arc<AtomicUsize>,
}

impl Pool {
    pub(crate) fn new<P: Plugin>(plugin: Arc<Mutex<P>>, id: &str) -> Pool {
        let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE_DEPTH);
        let receiver = Arc::new(Mutex::new(receiver));
        let pending = Arc::new(AtomicUsize::new(0));
        for index in 0..WORKER_THREADS {
            let plugin = Arc::clone(&plugin);
            let receiver = Arc::clone(&receiver);
            let pending = Arc::clone(&pending);
            let _ = thread::Builder::new()
                .name(format!("{id}-call-{index}"))
                .spawn(move || worker_loop(plugin, receiver, pending));
        }
        Pool { sender, pending }
    }

    pub(crate) fn submit(&self, id: Value, name: String, arguments: Value) -> Submit {
        self.pending.fetch_add(1, Ordering::SeqCst);
        match self.sender.try_send(Job {
            id,
            name,
            arguments,
        }) {
            Ok(()) => Submit::Queued,
            Err(TrySendError::Full(job)) => {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                Submit::Full(job)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                Submit::Closed
            }
        }
    }

    /// Give the accepted calls the stop budget to finish. The host kills the
    /// process group after 10 s regardless, so this never waits forever.
    pub(crate) fn drain(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while self.pending.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn worker_loop<P: Plugin>(
    plugin: Arc<Mutex<P>>,
    receiver: Arc<Mutex<Receiver<Job>>>,
    pending: Arc<AtomicUsize>,
) {
    loop {
        // The lock is held only while blocking in `recv`, so one worker waits
        // while the others run — concurrency without a second queue type.
        let job = {
            let guard = match receiver.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.recv()
        };
        let Ok(job) = job else { return };
        let outcome = {
            let mut guard = match plugin.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.call(&job.name, &job.arguments)
        };
        crate::reply_result(&job.id, outcome.into_result());
        pending.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a dispatch decided about the read loop.
pub(crate) enum Action {
    Continue,
    Stop,
}

/// Dispatch one stream-0 JSON-RPC message for either protocol.
///
/// The method set, the reply shapes and the error codes are the contract: a
/// plugin must hear the same answers whichever transport it speaks. The loops
/// differ only in how a frame arrives and in what an unknown notification
/// means, so that difference is the single hook.
pub(crate) fn dispatch<P: Plugin>(
    frame: &Value,
    context: &Context,
    pool: &Pool,
    plugin: &Arc<Mutex<P>>,
    notification: &mut dyn FnMut(&str, &Value) -> bool,
) -> Action {
    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
    let params = frame.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "svcd.ping" => reply_result(&id, json!({ "ok": true })),
        "svcd.status" => {
            let summary = status_text(plugin);
            reply_result(&id, json!({ "summary": summary, "details": {} }));
        }
        "svcd.shutdown" => {
            if !id.is_null() {
                reply_result(&id, Value::Null);
            }
            context.cancel().cancel();
            return Action::Stop;
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !P::CAPABILITIES.contains(&name.as_str()) {
                reply_error(&id, -32602, &format!("unknown tool: {name}"));
                return Action::Continue;
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match pool.submit(id, name, arguments) {
                Submit::Queued => {}
                Submit::Full(job) => reply_error(&job.id, -32603, "too many concurrent calls"),
                Submit::Closed => return Action::Stop,
            }
        }
        "" => {}
        other => {
            if !notification(other, &params) && !id.is_null() {
                reply_error(&id, -32601, &format!("method not found: {other}"));
            }
        }
    }
    Action::Continue
}
