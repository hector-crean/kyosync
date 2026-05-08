//! Runtime configuration loaded from the environment.
//!
//! Layered to match the bild_server convention: defaults → env overrides.
//! Production deployments add a TOML file source; we don't yet need that.

use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub log_filter: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7878".parse().expect("static addr parses"),
            log_filter: "kyoso_server=debug,tower_http=info".into(),
        }
    }
}

impl Config {
    /// Build a [`Config`] from environment variables, falling back to
    /// defaults. Recognises:
    ///
    /// - `KYOSO_BIND`        — `host:port` to listen on
    /// - `KYOSO_LOG_FILTER`  — `tracing_subscriber::EnvFilter` directive
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(bind) = std::env::var("KYOSO_BIND") {
            if let Ok(addr) = bind.parse() {
                cfg.bind = addr;
            } else {
                eprintln!("KYOSO_BIND={bind:?} is not a valid socket address; using default");
            }
        }
        if let Ok(filter) = std::env::var("KYOSO_LOG_FILTER") {
            cfg.log_filter = filter;
        }
        cfg
    }
}
