//! Home Assistant WebSocket session.
//!
//! [`Session::connect`] opens the socket, authenticates and spawns an actor
//! task that owns the socket. The actor assigns message IDs, routes results
//! and subscription events, and sends a `ping` every 30 s; a missing `pong`
//! within 10 s closes the session. When the session ends, pending requests
//! fail with [`HaError::Closed`] and every [`Subscription`] ends.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use url::Url;

use crate::HaError;
use crate::tls::Tls;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for one message (large instances send big `get_states` results).
const MAX_MESSAGE: usize = 32 * 1024 * 1024;
/// Buffered events per subscription before events are dropped.
const SUB_BUFFER: usize = 512;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Reply = oneshot::Sender<Result<Value, HaError>>;

enum Outgoing {
    Request {
        msg: Value,
        reply: Option<Reply>,
    },
    Subscribe {
        msg: Value,
        reply: Reply,
        events: mpsc::Sender<Value>,
        id_tx: oneshot::Sender<u64>,
    },
    Unsubscribe(u64),
}

enum Pending {
    /// A request and when it was sent.
    Request(Option<Reply>, tokio::time::Instant),
    Subscribe(Reply, mpsc::Sender<Value>),
}

impl Pending {
    /// Whether nobody waits for the answer any more: the caller gave up
    /// (`REQUEST_TIMEOUT`) or a fire-and-forget request was never answered.
    /// Subscriptions are not pruned here: a `Subscription` dropped after a
    /// timeout sends `Unsubscribe`, which needs the entry to find the ID.
    fn abandoned(&self, now: tokio::time::Instant) -> bool {
        match self {
            Pending::Request(Some(r), _) => r.is_closed(),
            Pending::Request(None, sent) => now.duration_since(*sent) > 2 * REQUEST_TIMEOUT,
            Pending::Subscribe(..) => false,
        }
    }
}

struct Inner {
    tx: mpsc::UnboundedSender<Outgoing>,
    closed: watch::Receiver<bool>,
    ha_version: String,
}

/// An authenticated WebSocket session. Cheap to clone.
#[derive(Clone)]
pub struct Session(Arc<Inner>);

/// Events of one subscription. Dropping it unsubscribes.
pub struct Subscription {
    id: u64,
    rx: mpsc::Receiver<Value>,
    tx: mpsc::UnboundedSender<Outgoing>,
    unsubscribed: AtomicBool,
}

impl Subscription {
    /// The next event, or `None` once the session closed.
    pub async fn next(&mut self) -> Option<Value> {
        self.rx.recv().await
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if !self.unsubscribed.swap(true, Ordering::Relaxed) {
            let _ = self.tx.send(Outgoing::Unsubscribe(self.id));
        }
    }
}

/// The WebSocket URL for a server base URL.
pub fn websocket_url(base: &Url) -> Url {
    let mut u = hd_core::server::join(base, "/api/websocket");
    let scheme = if base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    // Switching between special schemes is allowed by the URL standard.
    let _ = u.set_scheme(scheme);
    u
}

impl Session {
    /// Connect and authenticate with an access token.
    pub async fn connect(base: &Url, tls: &Tls, access_token: &str) -> Result<Session, HaError> {
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            Self::connect_inner(base, tls, access_token),
        )
        .await
        .map_err(|_| HaError::Timeout)?
    }

    async fn connect_inner(base: &Url, tls: &Tls, access_token: &str) -> Result<Session, HaError> {
        let host = base
            .host_str()
            .ok_or_else(|| HaError::Local("URL without host".into()))?
            .to_owned();
        let port = base.port_or_known_default().unwrap_or(8123);
        let tcp = tokio::net::TcpStream::connect((
            host.trim_start_matches('[').trim_end_matches(']'),
            port,
        ))
        .await
        .map_err(|e| HaError::Network(e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        let stream: Box<dyn Io> = if base.scheme() == "https" {
            let name = rustls::pki_types::ServerName::try_from(
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned(),
            )
            .map_err(|e| HaError::Local(e.to_string()))?;
            tls.capture.take();
            let connector = tokio_rustls::TlsConnector::from(tls.config.clone());
            match connector.connect(name, tcp).await {
                Ok(s) => Box::new(s),
                Err(e) => {
                    return Err(match tls.capture.take() {
                        Some(fp) => HaError::Tls {
                            fingerprint: Some(fp),
                            message: e.to_string(),
                        },
                        None => HaError::Network(e.to_string()),
                    });
                }
            }
        } else {
            Box::new(tcp)
        };

        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let url = websocket_url(base);
        let mut request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                url.as_str(),
            )
            .map_err(|e| HaError::Local(e.to_string()))?;
        if let Ok(ua) = crate::user_agent().parse() {
            request.headers_mut().insert("User-Agent", ua);
        }
        let (mut ws, _) =
            tokio_tungstenite::client_async_with_config(request, stream, Some(config))
                .await
                .map_err(|e| HaError::Network(crate::http::root_cause(&e)))?;

        // Authentication phase.
        let first = read_json(&mut ws).await?;
        if first.get("type").and_then(Value::as_str) != Some("auth_required") {
            return Err(HaError::Protocol("expected auth_required".into()));
        }
        let auth = json!({"type": "auth", "access_token": access_token}).to_string();
        ws.send(Message::text(auth))
            .await
            .map_err(|e| HaError::Network(e.to_string()))?;
        let answer = read_json(&mut ws).await?;
        let ha_version = match answer.get("type").and_then(Value::as_str) {
            Some("auth_ok") => answer
                .get("ha_version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            Some("auth_invalid") => {
                let m = answer
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid token");
                return Err(HaError::Auth(hd_core::truncate_chars(m, 200)));
            }
            _ => return Err(HaError::Protocol("unexpected authentication answer".into())),
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let (closed_tx, closed_rx) = watch::channel(false);
        tokio::spawn(actor(ws, rx, closed_tx));
        Ok(Session(Arc::new(Inner {
            tx,
            closed: closed_rx,
            ha_version,
        })))
    }

    /// The Home Assistant version reported during authentication.
    pub fn ha_version(&self) -> &str {
        &self.0.ha_version
    }

    pub fn is_closed(&self) -> bool {
        *self.0.closed.borrow()
    }

    /// Resolves when the session has ended.
    pub async fn closed(&self) {
        let mut rx = self.0.closed.clone();
        let _ = rx.wait_for(|c| *c).await;
    }

    /// Send a command and wait for its result.
    pub async fn call(&self, msg: Value) -> Result<Value, HaError> {
        let (reply, rx) = oneshot::channel();
        self.0
            .tx
            .send(Outgoing::Request {
                msg,
                reply: Some(reply),
            })
            .map_err(|_| HaError::Closed)?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Err(_) => Err(HaError::Timeout),
            Ok(Err(_)) => Err(HaError::Closed),
            Ok(Ok(r)) => r,
        }
    }

    /// Send a command without waiting for the result.
    pub fn send(&self, msg: Value) {
        let _ = self.0.tx.send(Outgoing::Request { msg, reply: None });
    }

    /// Start a subscription.
    pub async fn subscribe(&self, msg: Value) -> Result<Subscription, HaError> {
        let (reply, rx) = oneshot::channel();
        let (events, events_rx) = mpsc::channel(SUB_BUFFER);
        let (id_tx, id_rx) = oneshot::channel();
        self.0
            .tx
            .send(Outgoing::Subscribe {
                msg,
                reply,
                events,
                id_tx,
            })
            .map_err(|_| HaError::Closed)?;
        let id = id_rx.await.map_err(|_| HaError::Closed)?;
        let sub = Subscription {
            id,
            rx: events_rx,
            tx: self.0.tx.clone(),
            unsubscribed: AtomicBool::new(false),
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Err(_) => Err(HaError::Timeout),
            Ok(Err(_)) => Err(HaError::Closed),
            Ok(Ok(Err(e))) => {
                sub.unsubscribed.store(true, Ordering::Relaxed);
                Err(e)
            }
            Ok(Ok(Ok(_))) => Ok(sub),
        }
    }

    // ---- convenience commands ------------------------------------------

    pub async fn get_config(&self) -> Result<Value, HaError> {
        self.call(json!({"type": "get_config"})).await
    }

    pub async fn get_states(&self) -> Result<Vec<crate::EntityState>, HaError> {
        let v = self.call(json!({"type": "get_states"})).await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(crate::EntityState::from_full).collect())
            .unwrap_or_default())
    }

    /// `subscribe_entities` limited to the given entity IDs.
    pub async fn subscribe_entities(&self, entity_ids: &[String]) -> Result<Subscription, HaError> {
        self.subscribe(json!({"type": "subscribe_entities", "entity_ids": entity_ids}))
            .await
    }

    /// Call a service on one entity.
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        entity_id: &str,
        data: serde_json::Map<String, Value>,
    ) -> Result<Value, HaError> {
        self.call(json!({
            "type": "call_service",
            "domain": domain,
            "service": service,
            "service_data": data,
            "target": {"entity_id": entity_id},
        }))
        .await
    }

    /// Entity registry data for display (names, areas, precision). Works for non-admin users.
    pub async fn entity_registry_display(&self) -> Result<Value, HaError> {
        self.call(json!({"type": "config/entity_registry/list_for_display"}))
            .await
    }

    pub async fn area_registry(&self) -> Result<Value, HaError> {
        self.call(json!({"type": "config/area_registry/list"}))
            .await
    }

    pub async fn device_registry(&self) -> Result<Value, HaError> {
        self.call(json!({"type": "config/device_registry/list"}))
            .await
    }
}

async fn read_json<S: Io>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Result<Value, HaError> {
    loop {
        match ws.next().await {
            None => return Err(HaError::Closed),
            Some(Err(e)) => return Err(HaError::Network(e.to_string())),
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(t.as_str())
                    .map_err(|e| HaError::Protocol(e.to_string()));
            }
            Some(Ok(Message::Close(_))) => return Err(HaError::Closed),
            Some(Ok(_)) => continue,
        }
    }
}

async fn actor<S: Io + 'static>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    mut rx: mpsc::UnboundedReceiver<Outgoing>,
    closed: watch::Sender<bool>,
) {
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut subs: HashMap<u64, mpsc::Sender<Value>> = HashMap::new();
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pong_deadline: Option<(u64, tokio::time::Instant)> = None;

    let reason: &str = loop {
        let deadline = pong_deadline.map(|(_, d)| d);
        tokio::select! {
            out = rx.recv() => {
                let Some(out) = out else { break "all handles dropped" };
                // Drop requests nobody waits for any more, so a server that
                // stops answering cannot grow `pending` without bound.
                let now = tokio::time::Instant::now();
                pending.retain(|_, p| !p.abandoned(now));
                let (mut msg, id) = match out {
                    Outgoing::Request { msg, reply } => {
                        let id = next_id;
                        pending.insert(id, Pending::Request(reply, now));
                        (msg, id)
                    }
                    Outgoing::Subscribe { msg, reply, events, id_tx } => {
                        let id = next_id;
                        let _ = id_tx.send(id);
                        pending.insert(id, Pending::Subscribe(reply, events));
                        (msg, id)
                    }
                    Outgoing::Unsubscribe(sub_id) => {
                        if subs.remove(&sub_id).is_none() && !pending.contains_key(&sub_id) {
                            continue;
                        }
                        pending.remove(&sub_id);
                        let id = next_id;
                        pending.insert(id, Pending::Request(None, now));
                        (json!({"type": "unsubscribe_events", "subscription": sub_id}), id)
                    }
                };
                next_id += 1;
                msg["id"] = json!(id);
                if ws.send(Message::text(msg.to_string())).await.is_err() {
                    break "send failed";
                }
            }
            msg = ws.next() => {
                match msg {
                    None => break "closed by server",
                    Some(Err(e)) => {
                        tracing::debug!("websocket error: {e}");
                        break "read error";
                    }
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<Value>(t.as_str()) {
                            Ok(Value::Array(msgs)) => {
                                for m in msgs {
                                    handle(m, &mut pending, &mut subs, &mut pong_deadline);
                                }
                            }
                            Ok(m) => handle(m, &mut pending, &mut subs, &mut pong_deadline),
                            Err(e) => tracing::warn!("ignoring malformed message: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) => break "closed by server",
                    Some(Ok(_)) => {}
                }
            }
            _ = ping.tick() => {
                if pong_deadline.is_none() {
                    let id = next_id;
                    next_id += 1;
                    pong_deadline = Some((id, tokio::time::Instant::now() + PONG_TIMEOUT));
                    if ws.send(Message::text(json!({"id": id, "type": "ping"}).to_string())).await.is_err() {
                        break "send failed";
                    }
                }
            }
            _ = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                break "pong timeout";
            }
        }
    };
    tracing::info!("websocket session ended: {reason}");
    let _ = ws.close(None).await;
    for (_, p) in pending.drain() {
        match p {
            Pending::Request(Some(r), _) | Pending::Subscribe(r, _) => {
                let _ = r.send(Err(HaError::Closed));
            }
            Pending::Request(None, _) => {}
        }
    }
    subs.clear();
    let _ = closed.send(true);
}

fn handle(
    msg: Value,
    pending: &mut HashMap<u64, Pending>,
    subs: &mut HashMap<u64, mpsc::Sender<Value>>,
    pong_deadline: &mut Option<(u64, tokio::time::Instant)>,
) {
    let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
    match msg.get("type").and_then(Value::as_str) {
        Some("result") => {
            let ok = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
            let result = if ok {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            } else {
                let err = msg.get("error");
                let s = |k: &str| {
                    err.and_then(|e| e.get(k))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                Err(HaError::Ha {
                    code: s("code"),
                    message: hd_core::truncate_chars(&s("message"), 300),
                })
            };
            match pending.remove(&id) {
                Some(Pending::Request(Some(r), _)) => {
                    let _ = r.send(result);
                }
                Some(Pending::Subscribe(r, events)) => {
                    if result.is_ok() {
                        subs.insert(id, events);
                    }
                    let _ = r.send(result);
                }
                _ => {}
            }
        }
        Some("event") => {
            if let Some(tx) = subs.get(&id) {
                let event = msg.get("event").cloned().unwrap_or(Value::Null);
                match tx.try_send(event) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!("subscription {id} is lagging; dropped an event")
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        subs.remove(&id);
                    }
                }
            }
        }
        Some("pong") if pong_deadline.is_some_and(|(pid, _)| pid == id) => {
            *pong_deadline = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url() {
        let b = Url::parse("https://ha.example.org").unwrap();
        assert_eq!(
            websocket_url(&b).as_str(),
            "wss://ha.example.org/api/websocket"
        );
        let b = Url::parse("http://192.168.1.2:8123").unwrap();
        assert_eq!(
            websocket_url(&b).as_str(),
            "ws://192.168.1.2:8123/api/websocket"
        );
    }
}
