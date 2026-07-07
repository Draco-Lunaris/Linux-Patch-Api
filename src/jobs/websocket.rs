//! Job WebSocket Actor
//!
//! Implements real-time WebSocket streaming for job status updates using
//! actix-web-actors. Each connected client gets a WsJobActor that:
//! - Subscribes to JobManager broadcast channel for job status events
//! - Filters events based on client subscribe/unsubscribe messages
//! - Forwards matching events as JSON to the WebSocket client
//! - Handles ping/pong heartbeat for connection keep-alive
//! - Cleans up on disconnect

use actix::prelude::*;
use actix_web_actors::ws;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::manager::JobStatusEvent;

/// How often heartbeat pings are sent (seconds)
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a disconnect (seconds)
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Client-to-server WebSocket message
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "action")]
pub enum WsClientMessage {
    /// Subscribe to events for a specific job, or all jobs if job_id is None
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(default)]
        job_id: Option<String>,
    },
    /// Unsubscribe from events for a specific job
    #[serde(rename = "unsubscribe")]
    Unsubscribe { job_id: String },
}

/// Server-to-client WebSocket message
#[derive(Debug, Serialize, Clone)]
pub struct WsServerMessage {
    pub event: String,
    pub job_id: String,
    pub status: String,
    pub progress: u8,
    pub message: String,
    pub timestamp: String,
    /// Error message — only present on failure events. New field; old clients ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stable error code — only present on failure events. New field; old clients ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl WsServerMessage {
    /// Create a job status message from a JobStatusEvent
    pub fn from_job_status_event(event: &JobStatusEvent) -> Self {
        Self {
            event: event.event.clone(),
            job_id: event.job_id.to_string(),
            status: event.status.clone(),
            progress: event.progress,
            message: event.message.clone(),
            timestamp: event.timestamp.clone(),
            error: event.error.clone(),
            error_code: event.error_code.clone(),
        }
    }

    /// Create a connection established message
    pub fn connected(ws_id: &Uuid) -> Self {
        Self {
            event: "connected".to_string(),
            job_id: String::new(),
            status: "connected".to_string(),
            progress: 0,
            message: format!("WebSocket connected: {}", ws_id),
            timestamp: Utc::now().to_rfc3339(),
            error: None,
            error_code: None,
        }
    }

    /// Create a subscription confirmation message
    pub fn subscribed(job_id: &Option<String>) -> Self {
        match job_id {
            Some(id) => Self {
                event: "subscribed".to_string(),
                job_id: id.clone(),
                status: "subscribed".to_string(),
                progress: 0,
                message: format!("Subscribed to job: {}", id),
                timestamp: Utc::now().to_rfc3339(),
                error: None,
                error_code: None,
            },
            None => Self {
                event: "subscribed".to_string(),
                job_id: "all".to_string(),
                status: "subscribed".to_string(),
                progress: 0,
                message: "Subscribed to all job events".to_string(),
                timestamp: Utc::now().to_rfc3339(),
                error: None,
                error_code: None,
            },
        }
    }

    /// Create an unsubscription confirmation message
    pub fn unsubscribed(job_id: &str) -> Self {
        Self {
            event: "unsubscribed".to_string(),
            job_id: job_id.to_string(),
            status: "unsubscribed".to_string(),
            progress: 0,
            message: format!("Unsubscribed from job: {}", job_id),
            timestamp: Utc::now().to_rfc3339(),
            error: None,
            error_code: None,
        }
    }

    /// Create an error message
    pub fn error(code: &str, message: &str) -> Self {
        Self {
            event: "error".to_string(),
            job_id: String::new(),
            status: code.to_string(),
            progress: 0,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            error: None,
            error_code: None,
        }
    }

    /// Create a job status message (convenience constructor)
    pub fn job_status(job_id: &str, status: &str, progress: u8, message: &str) -> Self {
        Self {
            event: "job_status".to_string(),
            job_id: job_id.to_string(),
            status: status.to_string(),
            progress,
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            error: None,
            error_code: None,
        }
    }
}

/// Internal message for broadcasting a job status event to the actor
#[derive(Message)]
#[rtype(result = "()")]
pub struct BroadcastEvent(pub JobStatusEvent);

/// WebSocket actor for streaming job status updates
pub struct WsJobActor {
    /// Unique ID for this WebSocket connection
    ws_id: Uuid,
    /// Broadcast receiver for job status events from JobManager
    event_rx: Option<broadcast::Receiver<JobStatusEvent>>,
    /// Set of specific job IDs this client is subscribed to
    subscribed_jobs: HashSet<String>,
    /// Whether the client is subscribed to all job events
    subscribed_all: bool,
    /// Last time we heard from the client (ping/pong or message)
    last_heartbeat: Instant,
    /// The actor's own address for the broadcast listener
    addr: Option<Addr<WsJobActor>>,
}

impl WsJobActor {
    /// Create a new WebSocket actor with a broadcast receiver
    pub fn new(event_rx: broadcast::Receiver<JobStatusEvent>) -> Self {
        Self {
            ws_id: Uuid::new_v4(),
            event_rx: Some(event_rx),
            subscribed_jobs: HashSet::new(),
            subscribed_all: true, // Default: subscribe to all events
            last_heartbeat: Instant::now(),
            addr: None,
        }
    }

    /// Start the heartbeat check interval
    fn start_heartbeat(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            if Instant::now().duration_since(act.last_heartbeat) > CLIENT_TIMEOUT {
                // Heartbeat timed out, disconnect
                warn!(
                    ws_id = %act.ws_id,
                    "WebSocket heartbeat timeout, disconnecting"
                );
                ctx.stop();
                return;
            }
            // Send ping
            ctx.ping(b"");
        });
    }

    /// Start listening to the broadcast channel in a background task
    fn start_broadcast_listener(&mut self, ctx: &mut <Self as Actor>::Context) {
        let addr = ctx.address();
        self.addr = Some(addr.clone());

        // Take ownership of the receiver
        let mut rx = self.event_rx.take().expect("event_rx already taken");

        // Spawn a task that forwards broadcast events to this actor
        actix::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Send the event to the actor
                        if addr.try_send(BroadcastEvent(event)).is_err() {
                            // Actor is dead, stop listening
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // We fell behind, but can continue
                        debug!("WebSocket broadcast receiver lagged by {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Channel closed, stop listening
                        break;
                    }
                }
            }
        });
    }
}

impl Actor for WsJobActor {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!(ws_id = %self.ws_id, "WebSocket actor started");

        // Start heartbeat monitoring
        self.start_heartbeat(ctx);

        // Start listening to broadcast events
        self.start_broadcast_listener(ctx);

        // Send connection established message
        let msg = WsServerMessage::connected(&self.ws_id);
        if let Ok(json) = serde_json::to_string(&msg) {
            ctx.text(json);
        }
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        info!(ws_id = %self.ws_id, "WebSocket actor stopping");
        Running::Stop
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        info!(ws_id = %self.ws_id, "WebSocket actor stopped");
    }
}

/// Handle broadcast events from the JobManager channel
impl Handler<BroadcastEvent> for WsJobActor {
    type Result = ();

    fn handle(&mut self, msg: BroadcastEvent, ctx: &mut Self::Context) {
        let event = msg.0;

        // Check if this client should receive this event
        let should_forward =
            self.subscribed_all || self.subscribed_jobs.contains(&event.job_id.to_string());

        if should_forward {
            let server_msg = WsServerMessage::from_job_status_event(&event);
            match serde_json::to_string(&server_msg) {
                Ok(json) => ctx.text(json),
                Err(e) => {
                    error!(ws_id = %self.ws_id, error = %e, "Failed to serialize job status event");
                }
            }
        }
    }
}

/// Handle WebSocket protocol messages (ping/pong, text, close, etc.)
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsJobActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                error!(ws_id = %self.ws_id, error = %e, "WebSocket protocol error");
                ctx.stop();
                return;
            }
        };

        match msg {
            ws::Message::Ping(msg) => {
                self.last_heartbeat = Instant::now();
                ctx.pong(&msg);
            }
            ws::Message::Pong(_) => {
                self.last_heartbeat = Instant::now();
            }
            ws::Message::Text(text) => {
                let text = text.to_string();
                debug!(ws_id = %self.ws_id, text = %text, "Received WebSocket text message");

                // Parse as client message
                match serde_json::from_str::<WsClientMessage>(&text) {
                    Ok(client_msg) => match client_msg {
                        WsClientMessage::Subscribe { job_id } => match job_id {
                            Some(id) => {
                                self.subscribed_jobs.insert(id.clone());
                                let msg = WsServerMessage::subscribed(&Some(id));
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    ctx.text(json);
                                }
                            }
                            None => {
                                self.subscribed_all = true;
                                let msg = WsServerMessage::subscribed(&None);
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    ctx.text(json);
                                }
                            }
                        },
                        WsClientMessage::Unsubscribe { job_id } => {
                            self.subscribed_jobs.remove(&job_id);
                            let msg = WsServerMessage::unsubscribed(&job_id);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                ctx.text(json);
                            }
                        }
                    },
                    Err(e) => {
                        warn!(
                            ws_id = %self.ws_id,
                            error = %e,
                            text = %text,
                            "Invalid WebSocket client message"
                        );
                        let msg = WsServerMessage::error(
                            "invalid_message",
                            &format!("Invalid message: {}", e),
                        );
                        if let Ok(json) = serde_json::to_string(&msg) {
                            ctx.text(json);
                        }
                    }
                }
            }
            ws::Message::Binary(_) => {
                // We don't handle binary messages
                warn!(ws_id = %self.ws_id, "Received binary message, ignoring");
            }
            ws::Message::Close(reason) => {
                info!(ws_id = %self.ws_id, reason = ?reason, "WebSocket close received");
                ctx.close(reason);
                ctx.stop();
            }
            ws::Message::Continuation(_) => {
                // Continuation frames not expected for our use case
                ctx.stop();
            }
            ws::Message::Nop => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_server_message_from_event() {
        let event = JobStatusEvent {
            event: "job_status".to_string(),
            job_id: Uuid::new_v4(),
            status: "running".to_string(),
            progress: 50,
            message: "Processing...".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            error: None,
            error_code: None,
        };
        let msg = WsServerMessage::from_job_status_event(&event);
        assert_eq!(msg.event, "job_status");
        assert_eq!(msg.status, "running");
        assert_eq!(msg.progress, 50);
        assert!(msg.error.is_none());
        assert!(msg.error_code.is_none());
    }

    #[test]
    fn test_ws_server_message_from_failed_event() {
        let event = JobStatusEvent {
            event: "job_status".to_string(),
            job_id: Uuid::new_v4(),
            status: "failed".to_string(),
            progress: 0,
            message: "Install failed".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            error: Some("apt-get failed (exit 100): E: Unable to locate package foo".to_string()),
            error_code: Some("PKG_NOT_FOUND".to_string()),
        };
        let msg = WsServerMessage::from_job_status_event(&event);
        assert_eq!(msg.status, "failed");
        assert!(msg.error.is_some());
        assert_eq!(msg.error_code.as_deref(), Some("PKG_NOT_FOUND"));
        // Verify the error/code survive JSON round-trip.
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("PKG_NOT_FOUND"));
        assert!(json.contains("Unable to locate package"));
    }

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

    #[test]
    fn test_ws_client_message_unsubscribe() {
        let json = r#"{"action": "unsubscribe", "job_id": "test-uuid"}"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Unsubscribe { job_id } => {
                assert_eq!(job_id, "test-uuid");
            }
            _ => panic!("Expected Unsubscribe message"),
        }
    }
}
