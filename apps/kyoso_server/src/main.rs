//! Binary entry point. Loads config, installs tracing, opens the
//! persistent store (Postgres if `DATABASE_URL` is set, otherwise
//! in-memory), spawns snapshot + GC schedulers, and serves until
//! Ctrl+C / SIGTERM.

use kyoso_server::{AppState, Config, OpStore, SchedulerConfig, app, scheduler, shutdown};
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() {
    let config = Config::from_env();

    let filter = EnvFilter::try_new(&config.log_filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).compact().init();

    let store = match std::env::var("DATABASE_URL") {
        Ok(url) => {
            tracing::info!("using postgres store");
            OpStore::postgres(&url).await.expect("postgres connect")
        }
        Err(_) => {
            tracing::warn!("DATABASE_URL not set; using in-memory store (no persistence)");
            OpStore::in_memory()
        }
    };
    let state = AppState::from_store(store);

    let _scheduler_handles = scheduler::spawn(state.rooms.clone(), SchedulerConfig::default());

    let router = app(state);
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .expect("bind listener");
    tracing::info!(addr = %config.bind, "kyoso_server listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown::signal())
        .await
        .expect("server error");
}
