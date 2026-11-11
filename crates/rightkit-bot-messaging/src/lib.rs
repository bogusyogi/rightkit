//! `host_remote` — remote access adapters (Telegram bot + web auth).
//!
//! These adapters are pure clients of the local daemon HTTP API. They hold no engine
//! internals and no DB handles. This crate models the routing/auth/approval logic in a
//! transport-agnostic way so it can be unit-tested offline. The Telegram transport (the
//! long-poll loop and the HTTP daemon client) is [`telegram_poll`]; the web server lives in
//! the host binary and satisfies the traits defined here.

pub mod auth;
pub mod bridge;
pub mod channels;
pub mod daemon;
pub mod gateway;
pub mod openclaw;
pub mod telegram;
pub mod telegram_poll;
pub mod web;
pub mod webhook_ingress;
pub use webhook_ingress::{
    WebhookAdmission, WebhookIngress, WebhookIngressConfig, WebhookIngressError, WebhookSecret,
};

pub use auth::{AuthDecision, AuthGate};
pub use bridge::{BridgeError, BridgeTransport, RemoteBridge};
pub use daemon::{DaemonClient, DaemonError, StubDaemon};
pub use gateway::{
    append_remote_delivery, load_remote_deliveries, telegram_channel_descriptor,
    webhook_channel_descriptor, RemoteChannelAdapter, RemoteChannelDeliverySummary,
    RemoteChannelDescriptor, RemoteChannelProfile, RemoteChannelStatus, RemoteChannelsSnapshot,
    RemoteDeliveryPayload, RemoteDeliveryRecord, RemoteDeliveryTarget, RemoteGatewayProfile,
    TelegramChannelDescriptorInput, WebhookChannelAdapter,
};
pub use openclaw::{OpenClawMessage, OpenClawRequest, OpenClawResponse, OpenClawUsage};
pub use telegram::{
    load_telegram_session_bindings, save_telegram_session_bindings, BotCommand, InlineButton,
    TelegramBot, TelegramConfig, TelegramReply, TelegramSessionBindings, TelegramUpdate,
};
pub use web::{WebAuth, WebAuthDecision};
