//! WebSocket Handler for Real-time Job Status Streaming
//!
//! Implements WebSocket endpoint for real-time job status updates:
//! - WS /api/v1/ws/jobs - Real-time job status streaming
//!
//! Note: Full WebSocket implementation requires actix-web-actors compatibility.
//! This stub provides the endpoint structure for future enhancement.

use actix_web::{http::StatusCode, web, Error, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::jobs::manager::JobManager;

/// WebSocket message from client
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "action")]
pub enum WsClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(default)]
        job_id: Option<String>,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { job_id: String },
}

/// WebSocket message to client
#[derive(Debug, Serialize, Clone)]
pub struct WsServerMessage {
    pub event: String,
    pub job_id: String,
    pub status: String,
    pub progress: u8,
    pub message: String,
    pub timestamp: String,
}

impl WsServerMessage {
    pub fn job_status(job_id: &str, status: &str, progress: u8, message: &str) -> Self {
        Self {
            event: "job_status".to_string(),
            job_id: job_id.to_string(),
            status: status.to_string(),
            progress,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    pub fn job_complete(job_id: &str, status: &str, message: &str) -> Self {
        Self {
            event: "job_complete".to_string(),
            job_id: job_id.to_string(),
            status: status.to_string(),
            progress: 100,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

/// Handle WebSocket connection request
/// Returns upgrade response for WebSocket handshake
pub async fn websocket_handler(
    req: HttpRequest,
    _job_manager: web::Data<JobManager>,
) -> Result<HttpResponse, Error> {
    let ws_id = Uuid::new_v4();
    info!(ws_id = %ws_id, "WebSocket connection request");

    // Check if this is a WebSocket upgrade request
    if req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
    {
        // WebSocket upgrade requested
        // In full implementation, this would use actix-web-actors::ws::start()
        // For now, return a response indicating WebSocket support

        let response_msg = serde_json::json!({
            "event": "connected",
            "ws_id": ws_id.to_string(),
            "timestamp": Utc::now().to_rfc3339(),
            "message": "WebSocket endpoint ready. Full implementation requires actix-web-actors compatibility.",
            "polling_alternative": "Use GET /api/v1/jobs/{id} for job status polling"
        });

        // Return HTTP 101 Switching Protocols for WebSocket upgrade
        // In production, this would be handled by actix-web-actors
        Ok(HttpResponse::build(StatusCode::SWITCHING_PROTOCOLS)
            .insert_header(("upgrade", "websocket"))
            .insert_header(("connection", "upgrade"))
            .json(response_msg))
    } else {
        // Not a WebSocket request - return info about the endpoint
        let info_msg = serde_json::json!({
            "endpoint": "/api/v1/ws/jobs",
            "method": "GET",
            "upgrade_required": "websocket",
            "headers": {
                "upgrade": "websocket",
                "connection": "Upgrade",
                "sec-websocket-key": "<base64-key>",
                "sec-websocket-version": "13"
            },
            "alternative": "Use GET /api/v1/jobs/{id} for job status polling"
        });

        Ok(HttpResponse::Ok().json(info_msg))
    }
}

/// Broadcast job status update to subscribed WebSocket clients
pub async fn broadcast_job_update(
    job_id: &Uuid,
    status: &crate::jobs::manager::JobStatus,
    progress: u8,
    _message: &str,
) {
    info!(job_id = %job_id, status = ?status, progress = progress, "Job status update available for broadcast");
    // In production, would use a broadcast channel to notify all subscribed WebSocket clients
}

/// Configure WebSocket route
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/ws/jobs", web::get().to(websocket_handler));
}

#[cfg(test)]
mod tests {
    use super::*;

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
