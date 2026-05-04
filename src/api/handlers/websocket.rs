//! WebSocket Handler for Real-time Job Status Streaming
//!
//! Implements WebSocket endpoint for real-time job status updates:
//! - WS /api/v1/ws/jobs - Real-time job status streaming
//!
//! Uses actix-web-actors for proper WebSocket handshake and protocol handling.
//! The actual actor logic lives in crate::jobs::websocket::WsJobActor.

use actix_web::{web, Error, HttpRequest, HttpResponse};
use tracing::info;

use crate::jobs::manager::JobManager;
use crate::jobs::websocket::WsJobActor;

/// Handle WebSocket connection request
/// Performs the WebSocket handshake and spawns a WsJobActor
/// that streams job status events to the connected client.
pub async fn websocket_handler(
    req: HttpRequest,
    stream: web::Payload,
    job_manager: web::Data<JobManager>,
) -> Result<HttpResponse, Error> {
    info!("WebSocket connection request received");

    // Subscribe to job status events from the JobManager broadcast channel
    let event_rx = job_manager.subscribe();

    // Create the WebSocket actor with the broadcast receiver
    let actor = WsJobActor::new(event_rx);

    // Perform the WebSocket handshake and start the actor
    // This computes the proper Sec-WebSocket-Accept header and upgrades the connection
    actix_web_actors::ws::start(actor, &req, stream)
}

/// Configure WebSocket route
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/ws/jobs", web::get().to(websocket_handler));
}

#[cfg(test)]
mod tests {
    use crate::jobs::websocket::{WsClientMessage, WsServerMessage};

    #[test]
    fn test_ws_server_message_serialization() {
        let msg = WsServerMessage::job_status("test-uuid", "running", 50, "Processing...");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("job_status"));
        assert!(json.contains("running"));
        assert!(json.contains("50"));
    }

    #[test]
    fn test_ws_client_message_subscribe() {
        let json = r#"{"action": "subscribe", "job_id": "test-uuid"}"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Subscribe { job_id } => {
                assert_eq!(job_id, Some("test-uuid".to_string()));
            }
            _ => panic!("Expected Subscribe message"),
        }
    }

    #[test]
    fn test_ws_client_message_subscribe_all() {
        let json = r#"{"action": "subscribe"}"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Subscribe { job_id } => {
                assert!(job_id.is_none());
            }
            _ => panic!("Expected Subscribe message"),
        }
    }
}
