//! The shared HTTP client setup. Every outbound request (Polymarket,
//! Supabase Storage) goes through an agent built here so TLS and timeouts are consistent.

use std::time::Duration;

use ureq::Agent;
use ureq::tls::{TlsConfig, TlsProvider};

pub const USER_AGENT: &str = concat!("oddsmith/", env!("CARGO_PKG_VERSION"));

/// An agent using the platform TLS stack. Non-2xx statuses are returned as responses, not
/// errors, so callers can read error bodies and decide whether to retry.
pub fn agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into()
}
