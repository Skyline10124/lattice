use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use crate::events::{EventBus, PipelineEvent};

// ---------------------------------------------------------------------------
// WebSocket endpoint for live pipeline event streaming
// ---------------------------------------------------------------------------

/// Build an axum [`Router`] that serves the pipeline event stream at `/ws`.
///
/// ```ignore
/// let bus = Arc::new(EventBus::new(256));
/// let app = axum::Router::new()
///     .nest("/pipeline", pipeline_ws_router(bus));
/// ```
pub fn pipeline_ws_router(bus: Arc<EventBus>) -> axum::Router {
    axum::Router::new().route("/ws", get(move |ws| ws_handler(ws, bus.clone())))
}

/// Handle a WebSocket upgrade request — subscribe the client to the event bus.
async fn ws_handler(ws: WebSocketUpgrade, bus: Arc<EventBus>) -> impl IntoResponse {
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
}
