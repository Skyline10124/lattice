use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::warn;

use crate::events::{EventBus, PipelineEvent};

// ---------------------------------------------------------------------------
// WebSocket endpoint for live pipeline event streaming
// ---------------------------------------------------------------------------

/// The base path for the WebSocket endpoint. Typically mounted at
/// `/pipeline/ws` (or similar).
const WS_PATH: &str = "/ws";

const DEFAULT_ALLOWED_ORIGINS: &[&str] = &[
    "http://localhost:3000",
    "http://localhost:5173",
    "http://127.0.0.1:3000",
    "http://127.0.0.1:5173",
];

fn allowed_origins() -> Vec<String> {
    std::env::var("WS_ALLOWED_ORIGINS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .filter(|origins: &Vec<String>| !origins.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(|origin| origin.to_string())
                .collect()
        })
}

/// Build an axum [`Router`] that serves the pipeline event stream at `/ws`.
///
/// ```ignore
/// let bus = Arc::new(EventBus::new(256));
/// let app = axum::Router::new()
///     .nest("/pipeline", pipeline_ws_router(bus));
/// ```
///
/// # Authentication
///
/// When the `WS_SECRET` environment variable is set, clients must provide a
/// matching `token` query parameter. Additionally, the `Origin` header is
/// validated against a hardcoded allowlist of known development origins.
/// Authentication is opt-in: if `WS_SECRET` is not set, connections are
/// accepted without restriction.
pub fn pipeline_ws_router(bus: Arc<EventBus>) -> axum::Router {
    axum::Router::new().route(
        WS_PATH,
        get(move |ws, query, headers| ws_handler(ws, query, headers, bus.clone())),
    )
}

/// Query parameters for WebSocket authentication.
#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

/// Handle a WebSocket upgrade request — validate auth and subscribe the client
/// to the event bus.
async fn ws_handler(
    ws: WebSocketUpgrade,
    query: Query<WsQuery>,
    headers: HeaderMap,
    bus: Arc<EventBus>,
) -> impl IntoResponse {
    let secret = std::env::var("WS_SECRET").ok().filter(|s| !s.is_empty());
    if let Some(ref secret) = secret {
        let token = query.token.as_deref().unwrap_or("");
        if token != secret {
            warn!("WebSocket connection rejected: invalid token");
            return ws.on_upgrade(move |socket| async move {
                let mut socket = socket;
                let _ = socket
                    .send(Message::Text(
                        r#"{"error":"authentication failed: invalid token"}"#.into(),
                    ))
                    .await;
            });
        }
    }

    // Validate Origin header when WS_SECRET is configured
    if secret.is_some() {
        let origin_str = headers
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let allowed_origins = allowed_origins();

        let allowed = allowed_origins.iter().any(|a| a == origin_str) || origin_str.is_empty();

        if !allowed {
            warn!(
                "WebSocket connection rejected: origin '{}' not allowed",
                origin_str
            );
            return ws.on_upgrade(move |socket| async move {
                let mut socket = socket;
                let _ = socket
                    .send(Message::Text(r#"{"error":"origin not allowed"}"#.into()))
                    .await;
            });
        }
    }

    ws.on_upgrade(move |socket| handle_socket(socket, bus))
}

async fn handle_socket(socket: WebSocket, bus: Arc<EventBus>) {
    let (mut sender, mut receiver) = socket.split();
    let mut events: broadcast::Receiver<PipelineEvent> = bus.subscribe();

    // Spawn a task that forwards events to the WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        break; // client disconnected
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WebSocket client lagged by {n} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Drain incoming messages (ping/pong handled automatically by axum)
    while receiver.next().await.is_some() {}

    send_task.abort();
    let _ = send_task.await;
}
