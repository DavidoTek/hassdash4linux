//! Home Assistant client used by both hassdash4linux binaries.
//!
//! * [`tls`]: rustls configuration with system roots and optional certificate pinning
//! * [`http`]: minimal REST client (no redirects, size limits)
//! * [`auth`]: OAuth2 + PKCE login over a loopback redirect, refresh and revoke
//! * [`ws`]: the WebSocket session (requests, subscriptions, heartbeat)
//! * [`entities`]: decoder for `subscribe_entities` and the entity cache
//! * [`format`]: human-readable entity states
//! * [`mobile_app`]: companion device registration and encrypted webhooks
//! * [`client`]: ties credentials, TLS and connections together

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod entities;
pub mod error;
pub mod format;
pub mod http;
pub mod mobile_app;
pub mod tls;
pub mod ws;

pub use client::{Connector, Credentials};
pub use entities::{EntityCache, EntityState};
pub use error::HaError;
pub use ws::{Session, Subscription};

/// User agent for HTTP and WebSocket requests.
pub fn user_agent() -> String {
    format!("{}/{}", hd_core::APP_NAME, hd_core::VERSION)
}
