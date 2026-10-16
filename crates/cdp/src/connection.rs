use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::error::{CdpError, Result};
use crate::session::Session;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Events are fanned out to every subscriber; a subscriber that falls this far behind
/// starts losing events (and is told so via `RecvError::Lagged`).
const EVENT_BUFFER: usize = 8192;

/// A protocol event, e.g. `Page.loadEventFired`, tagged with the flat-mode session it
/// belongs to (`None` for browser-level events).
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub session_id: Option<String>,
    pub params: Value,
}

/// One connection to a browser, multiplexing flat-mode sessions for every attached target.
///
/// Framing follows `--remote-debugging-pipe`: each message is a JSON document terminated
/// by a NUL byte. Using the pipe rather than a TCP port means no other local process (and
/// no page doing localhost port scans) can discover or attach to the browser.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

struct Inner {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    events: broadcast::Sender<Event>,
    closed: AtomicBool,
}

#[derive(Deserialize)]
struct Incoming {
    id: Option<u64>,
    method: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<ProtocolError>,
}

#[derive(Deserialize)]
struct ProtocolError {
    code: i64,
    message: String,
}

impl Connection {
    /// Starts the reader and writer tasks over an already-established pipe pair.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (inner, rx) = Inner::start();
        tokio::spawn(write_loop(writer, rx));
        tokio::spawn(read_loop(reader, Arc::clone(&inner)));
        Self { inner }
    }

    /// Connects to a browser's DevTools WebSocket: `ws://host:port/devtools/browser/<id>`
    /// from a Chrome started with `--remote-debugging-port`, or the `wss://` address a
    /// hosted browser hands out. Each message is one text frame, where the pipe ends
    /// each with a NUL byte; otherwise the protocol is the same.
    pub async fn connect(url: &str) -> Result<Self> {
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (stream, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), true)
            .await
            .map_err(|err| CdpError::Connect {
                url: url.to_owned(),
                reason: err.to_string(),
            })?;
        let (mut sink, mut source) = futures_util::StreamExt::split(stream);
        let (inner, mut rx) = Inner::start();
        tokio::spawn(async move {
            use futures_util::SinkExt;
            use tokio_tungstenite::tungstenite::Message;
            while let Some(mut bytes) = rx.recv().await {
                if bytes.last() == Some(&0) {
                    bytes.pop();
                }
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };
                if let Err(err) = sink.send(Message::text(text)).await {
                    tracing::warn!(%err, "CDP websocket write failed");
                    return;
                }
            }
            let _ = sink.close().await;
        });
        let reading = Arc::clone(&inner);
        tokio::spawn(async move {
            use futures_util::StreamExt;
            use tokio_tungstenite::tungstenite::Message;
            while let Some(message) = source.next().await {
                match message {
                    Ok(Message::Text(text)) => dispatch(&reading, text.as_bytes()),
                    Ok(Message::Binary(bytes)) => dispatch(&reading, &bytes),
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(%err, "CDP websocket read failed");
                        break;
                    }
                }
            }
            reading.close();
        });
        Ok(Self { inner })
    }

    /// The browser-level session (no target attached).
    pub fn browser(&self) -> Session {
        Session::new(self.clone(), None)
    }

    /// A handle for an already-attached flat-mode session.
    pub fn session(&self, session_id: impl Into<String>) -> Session {
        Session::new(self.clone(), Some(session_id.into()))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub(crate) async fn call(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = serde_json::json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            message["sessionId"] = Value::from(session_id);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|source| CdpError::Serde {
            method: method.to_owned(),
            source,
        })?;
        bytes.push(0);

        let (tx, rx) = oneshot::channel();
        self.inner.pending().insert(id, tx);
        if self.inner.outgoing.send(bytes).is_err() {
            self.inner.pending().remove(&id);
            return Err(CdpError::Closed);
        }

        let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result.map_err(|err| match err {
                CdpError::Protocol { code, message, .. } => CdpError::Protocol {
                    method: method.to_owned(),
                    code,
                    message,
                },
                other => other,
            }),
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                self.inner.pending().remove(&id);
                Err(CdpError::Timeout {
                    method: method.to_owned(),
                    timeout,
                })
            }
        }
    }
}

impl Inner {
    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value>>>> {
        // A poisoned lock only means another task panicked mid-insert; the map is still valid.
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        for (_, tx) in self.pending().drain() {
            let _ = tx.send(Err(CdpError::Closed));
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(bytes) = rx.recv().await {
        if let Err(err) = writer.write_all(&bytes).await {
            tracing::warn!(%err, "CDP pipe write failed");
            return;
        }
        if let Err(err) = writer.flush().await {
            tracing::warn!(%err, "CDP pipe flush failed");
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(reader: R, inner: Arc<Inner>) {
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    let mut buf = Vec::with_capacity(1 << 16);
    loop {
        buf.clear();
        match reader.read_until(0, &mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(%err, "CDP pipe read failed");
                break;
            }
        }
        if buf.last() == Some(&0) {
            buf.pop();
        }
        dispatch(&inner, &buf);
    }
    inner.close();
}

impl Inner {
    /// A connection's shared state, and the queue its writer sends from.
    fn start() -> (Arc<Self>, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (outgoing, rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let inner = Arc::new(Inner {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            outgoing,
            events,
            closed: AtomicBool::new(false),
        });
        (inner, rx)
    }
}

/// The largest message a remote browser may send: screenshots and DOM snapshots of
/// long pages run to tens of megabytes (the pipe has no limit).
const MAX_MESSAGE: usize = 256 << 20;

fn dispatch(inner: &Inner, bytes: &[u8]) {
    let message: Incoming = match serde_json::from_slice(bytes) {
        Ok(message) => message,
        Err(err) => {
            tracing::warn!(%err, "dropping malformed CDP message");
            return;
        }
    };
    if let Some(id) = message.id {
        let Some(tx) = inner.pending().remove(&id) else {
            // The caller already gave up (timeout); the late answer is irrelevant.
            return;
        };
        let result = match message.error {
            Some(error) => Err(CdpError::Protocol {
                method: String::new(),
                code: error.code,
                message: error.message,
            }),
            None => Ok(message.result.unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    } else if let Some(method) = message.method {
        // No subscribers is a normal state; the event is simply dropped.
        let _ = inner.events.send(Event {
            method,
            session_id: message.session_id,
            params: message.params.unwrap_or(Value::Null),
        });
    }
}
