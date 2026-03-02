use std::sync::OnceLock;

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Global WebChannel instance shared between gateway and channel system.
/// Initialized once when web channel is enabled in config.
static WEB_CHANNEL_INSTANCE: OnceLock<Arc<WebChannel>> = OnceLock::new();

/// Channel message sender, set by `start_channels` when the web channel is active.
static WEB_CHANNEL_TX: OnceLock<tokio::sync::mpsc::Sender<super::traits::ChannelMessage>> = OnceLock::new();

/// Get or create the global WebChannel instance.
pub fn get_or_init_web_channel() -> Arc<WebChannel> {
    Arc::clone(WEB_CHANNEL_INSTANCE.get_or_init(|| Arc::new(WebChannel::new())))
}

/// Get the global WebChannel instance if it exists.
pub fn get_web_channel() -> Option<Arc<WebChannel>> {
    WEB_CHANNEL_INSTANCE.get().map(Arc::clone)
}

/// Set the channel message sender (called from start_channels).
pub fn set_web_channel_tx(tx: tokio::sync::mpsc::Sender<super::traits::ChannelMessage>) {
    let _ = WEB_CHANNEL_TX.set(tx);
}

/// Get the channel message sender.
pub fn get_web_channel_tx() -> Option<tokio::sync::mpsc::Sender<super::traits::ChannelMessage>> {
    WEB_CHANNEL_TX.get().cloned()
}
// ─────────────────────────────────────────────────────────────────────────────
// JSON message types for WebSocket protocol
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "message")]
    Message {
        content: String,
        #[serde(default)]
        image_url: Option<String>,
    },
    #[serde(rename = "command")]
    Command { content: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "typing")]
    Typing { active: bool },
    #[serde(rename = "draft")]
    Draft { content: String, message_id: String },
    #[serde(rename = "draft_update")]
    DraftUpdate { content: String, message_id: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        name: String,
        result: serde_json::Value,
    },
    #[serde(rename = "message")]
    Message { content: String, message_id: String },
    #[serde(rename = "reaction")]
    Reaction { emoji: String, action: String },
    #[serde(rename = "error")]
    Error { content: String },
    #[serde(rename = "pong")]
    Pong,
}

// ─────────────────────────────────────────────────────────────────────────────
// WebChannel struct + Channel trait implementation
// ─────────────────────────────────────────────────────────────────────────────

pub struct WebChannel {
    broadcast_tx: broadcast::Sender<String>,
    connection_count: Arc<AtomicUsize>,
}

impl WebChannel {
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(256);
        Self {
            broadcast_tx,
            connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn broadcast_tx(&self) -> &broadcast::Sender<String> {
        &self.broadcast_tx
    }

    pub fn connection_count(&self) -> usize {
        self.connection_count.load(Ordering::Relaxed)
    }

    fn broadcast(&self, msg: &ServerMessage) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = self.broadcast_tx.send(json);
        }
    }
}

#[async_trait]
impl Channel for WebChannel {
    fn name(&self) -> &str {
        "Web"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Message {
            content: message.content.clone(),
            message_id: uuid::Uuid::new_v4().to_string(),
        });
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Web channel receives messages via WS handler, not by actively listening.
        // Block until shutdown.
        tokio::signal::ctrl_c().await?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Typing { active: true });
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Typing { active: false });
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.broadcast(&ServerMessage::Draft {
            content: message.content.clone(),
            message_id: message_id.clone(),
        });
        Ok(Some(message_id))
    }

    async fn update_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<Option<String>> {
        self.broadcast(&ServerMessage::DraftUpdate {
            content: text.to_string(),
            message_id: message_id.to_string(),
        });
        Ok(None)
    }

    async fn finalize_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Message {
            content: text.to_string(),
            message_id: message_id.to_string(),
        });
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Reaction {
            emoji: emoji.to_string(),
            action: "add".to_string(),
        });
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.broadcast(&ServerMessage::Reaction {
            emoji: emoji.to_string(),
            action: "remove".to_string(),
        });
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WebSocket connection handler
// ─────────────────────────────────────────────────────────────────────────────

pub async fn handle_ws_connection(
    socket: WebSocket,
    channel: Arc<WebChannel>,
    message_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut broadcast_rx = channel.broadcast_tx.subscribe();
    channel.connection_count.fetch_add(1, Ordering::Relaxed);

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Message { content, .. }) => {
                                if content.trim().is_empty() {
                                    continue;
                                }
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let _ = message_tx.send(ChannelMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    sender: format!("web:{}", uuid::Uuid::new_v4()),
                                    reply_target: "web".to_string(),
                                    content,
                                    channel: "Web".to_string(),
                                    timestamp: ts,
                                    thread_ts: None,
                                }).await;
                            }
                            Ok(ClientMessage::Command { content }) => {
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let _ = message_tx.send(ChannelMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    sender: format!("web:{}", uuid::Uuid::new_v4()),
                                    reply_target: "web".to_string(),
                                    content,
                                    channel: "Web".to_string(),
                                    timestamp: ts,
                                    thread_ts: None,
                                }).await;
                            }
                            Ok(ClientMessage::Ping) => {
                                let pong = serde_json::to_string(&ServerMessage::Pong)
                                    .unwrap_or_default();
                                let _ = ws_sender.send(Message::Text(pong.into())).await;
                            }
                            Err(_) => {
                                let err = serde_json::to_string(&ServerMessage::Error {
                                    content: "Invalid JSON".to_string(),
                                }).unwrap_or_default();
                                let _ = ws_sender.send(Message::Text(err.into())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                }
            }
            msg = broadcast_rx.recv() => {
                if let Ok(json_str) = msg {
                    if ws_sender.send(Message::Text(json_str.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    channel.connection_count.fetch_sub(1, Ordering::Relaxed);
}
