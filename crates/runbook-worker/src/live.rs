//! Live updates (D58, CF4), wasm32 only.
//!
//! **In the ExecutionDO.** A subscriber is a hibernatable WebSocket
//! (`acceptWebSocket`, tag `live`) whose attachment is the last sequence it
//! was sent. Nothing about it is held in memory: after every commit the
//! object reads its sockets back from the runtime (`getWebSockets`) and
//! their cursors from their attachments, so a commit handled by a fresh
//! instance (after eviction, with the sockets hibernated) still reaches
//! them. Each message is one SSE frame (`runbook_api::sse::frame`); after
//! the run's last event the object closes the socket.
//!
//! **In the api Worker.** `GET /v1/executions/{exe}/events` opens one
//! socket to the object and streams its frames as `text/event-stream`,
//! with the native headers, `: keep-alive` while idle and an end after the
//! stream lifetime. The native server serves the same bytes.
use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::{FutureExt, Stream, StreamExt};
use runbook_core::{ExecutionStore, StoreError};
use runbook_store_do::{DoSql, DoStore};
use serde::{Deserialize, Serialize};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use wasm_bindgen::{JsCast, closure::Closure};
use worker::{Delay, State, WebSocket, WebSocketPair};

/// The tag every live subscriber is accepted with.
pub const TAG: &str = "live";

/// A subscriber's attachment: the last sequence it was sent.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Cursor {
    pub sent: u64,
}

/// Accept a subscriber resuming after `after`, send it what it missed,
/// and return the client end. Refused cursors are `(status, code)`.
pub async fn subscribe<B: DoSql>(
    state: &State,
    store: &DoStore<B>,
    after: u64,
) -> Result<WebSocket, (u16, &'static str)> {
    let run = store
        .load(store.name())
        .await
        .map_err(|error| match error {
            StoreError::NotFound => (404, "resource.not_found"),
            _ => (503, "storage.unavailable"),
        })?;
    runbook_api::sse::check_history(&run.events, run.revision, after)?;
    let pair = WebSocketPair::new().map_err(|_| (503, "stream.unavailable"))?;
    state.accept_websocket_with_tags(&pair.server, &[TAG]);
    pair.server
        .serialize_attachment(Cursor { sent: after })
        .map_err(|_| (503, "stream.unavailable"))?;
    send_new(&pair.server, &run, after);
    Ok(pair.client)
}

/// Send every event after `sent`; record the new cursor; close after the
/// last event. A socket that cannot be written is closed.
fn send_new(socket: &WebSocket, run: &runbook_core::StoredExecution, sent: u64) {
    let mut cursor = sent;
    for event in run.events.iter().filter(|e| e.seq > sent) {
        let Some(frame) = runbook_api::sse::frame(event) else {
            let _ = socket.close(Some(1011), Some("encode"));
            return;
        };
        if socket.send_with_str(frame).is_err() {
            let _ = socket.close(Some(1011), Some("send"));
            return;
        }
        cursor = event.seq;
        if runbook_api::sse::is_last(event) {
            let _ = socket.serialize_attachment(Cursor { sent: cursor });
            let _ = socket.close(Some(1000), Some("completed"));
            return;
        }
    }
    let _ = socket.serialize_attachment(Cursor { sent: cursor });
}

/// After a commit: bring every subscriber of this object up to the head.
/// Returns how many sockets were served.
pub async fn fan_out<B: DoSql>(state: &State, store: &DoStore<B>) -> usize {
    let sockets = state.get_websockets_with_tag(TAG);
    if sockets.is_empty() {
        return 0;
    }
    let Ok(run) = store.load(store.name()).await else {
        return 0;
    };
    for socket in &sockets {
        let sent = socket
            .deserialize_attachment::<Cursor>()
            .ok()
            .flatten()
            .unwrap_or_default()
            .sent;
        send_new(socket, &run, sent);
    }
    sockets.len()
}

/// The socket's event listeners, kept alive as long as the bridge.
type Listeners = (
    Closure<dyn FnMut(web_sys::MessageEvent)>,
    Closure<dyn FnMut(web_sys::CloseEvent)>,
);

/// What the bridge reads from the object's socket.
enum Item {
    Frame(String),
    Closed,
}

/// The SSE body: the object's frames, keep-alives while idle, and an end
/// at the lifetime or when the object closes. Dropping it (the client
/// went away) closes the socket to the object.
/// D32: re-authorize the caller; `false` ends the stream.
pub type Recheck = Box<dyn FnMut() -> Pin<Box<dyn std::future::Future<Output = bool>>>>;

pub struct Bridge {
    socket: WebSocket,
    recheck: Option<Recheck>,
    checking: Option<Pin<Box<dyn std::future::Future<Output = bool>>>>,
    /// D32: when the next re-authorization is due, on its own clock: frames
    /// never push it back (review fix 5).
    check_due: Pin<Box<Delay>>,
    frames: UnboundedReceiver<Item>,
    keep_alive: Duration,
    idle: Pin<Box<Delay>>,
    ends_at_ms: f64,
    done: bool,
    _listeners: Listeners,
}

impl Bridge {
    /// `recheck` runs at every keep-alive, before the keep-alive is sent
    /// (D32, as the native stream's periodic recheck): a caller whose
    /// credential was revoked or lost access stops receiving at once.
    pub fn new(
        socket: WebSocket,
        keep_alive: Duration,
        lifetime: Duration,
        recheck: Option<Recheck>,
    ) -> worker::Result<Self> {
        socket.accept()?;
        let (tx, frames) = unbounded();
        let on_message = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                if let Some(text) = event.data().as_string() {
                    let _ = tx.unbounded_send(Item::Frame(text));
                }
            })
        };
        let on_close = Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |_| {
            let _ = tx.unbounded_send(Item::Closed);
        });
        let raw: &web_sys::WebSocket = socket.as_ref();
        raw.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        raw.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        #[allow(clippy::cast_precision_loss)]
        let ends_at_ms = worker::Date::now().as_millis() as f64 + lifetime.as_millis() as f64;
        Ok(Self {
            socket,
            recheck,
            checking: None,
            check_due: Box::pin(Delay::from(keep_alive)),
            frames,
            keep_alive,
            idle: Box::pin(Delay::from(keep_alive)),
            ends_at_ms,
            done: false,
            _listeners: (on_message, on_close),
        })
    }
}

impl Stream for Bridge {
    type Item = Result<Vec<u8>, worker::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        #[allow(clippy::cast_precision_loss)]
        if worker::Date::now().as_millis() as f64 >= self.ends_at_ms {
            self.done = true;
            return Poll::Ready(None);
        }
        // A re-authorization in flight holds every frame until it answers.
        if let Some(check) = self.checking.as_mut() {
            match check.as_mut().poll(cx) {
                Poll::Ready(true) => {
                    self.checking = None;
                    let every = self.keep_alive;
                    self.check_due = Box::pin(Delay::from(every));
                }
                Poll::Ready(false) => {
                    self.done = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        // D32 on a fixed cadence: due checks run before the next frame, so
        // a busy run cannot keep a revoked caller streaming.
        if self.recheck.is_some() && self.check_due.poll_unpin(cx).is_ready() {
            if let Some(recheck) = self.recheck.as_mut() {
                self.checking = Some(recheck());
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        match self.frames.poll_next_unpin(cx) {
            Poll::Ready(Some(Item::Frame(frame))) => {
                let keep_alive = self.keep_alive;
                self.idle = Box::pin(Delay::from(keep_alive));
                return Poll::Ready(Some(Ok(frame.into_bytes())));
            }
            Poll::Ready(Some(Item::Closed) | None) => {
                self.done = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }
        match self.idle.poll_unpin(cx) {
            Poll::Ready(()) => {
                let keep_alive = self.keep_alive;
                self.idle = Box::pin(Delay::from(keep_alive));
                Poll::Ready(Some(Ok(runbook_api::sse::KEEP_ALIVE.as_bytes().to_vec())))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let raw: &web_sys::WebSocket = self.socket.as_ref();
        raw.set_onmessage(None);
        raw.set_onclose(None);
        let _ = self.socket.close(Some(1000), Some("subscriber gone"));
    }
}
