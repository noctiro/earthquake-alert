mod config;
mod db;
mod models;
mod routes;
mod services;
mod utils;

use anyhow::Result;
use axum::{
    Router,
    routing::{delete, get, post},
};
use config::Config;
use db::Database;
use routes::{
    AppState, health_handler, index_handler, stats_handler, subscribe_handler, unsubscribe_handler,
};
use services::{BarkNotifier, BarkPushConfig, EarthquakeMonitor};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "earthquake_alert_backend=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env();
    tracing::info!(
        event = "config.loaded",
        server_host = %config.server_host,
        server_port = config.server_port,
        db_path = %config.db_path,
        websocket_url = %config.eew_websocket_url,
        max_concurrent_notifications = config.max_concurrent_notifications,
        http_pool_size = config.http_pool_size,
        "config.loaded"
    );

    let db = Database::open(&config.db_path)?;
    tracing::info!(event = "database.opened", db_path = %config.db_path, "database.opened");

    let push_config = BarkPushConfig {
        sound: config.bark_sound.clone(),
        volume: config.bark_volume,
        group: config.bark_group.clone(),
        call: config.bark_call,
    };
    let bark_notifier = BarkNotifier::new(
        config.bark_api_url.clone(),
        config.http_pool_size,
        db.subscriptions(),
        push_config,
    )?;

    let state = AppState {
        db: db.clone(),
        bark_notifier: bark_notifier.clone(),
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/index.html", get(index_handler))
        .route("/health", get(health_handler))
        .route("/api/subscribe", post(subscribe_handler))
        .route("/api/unsubscribe", delete(unsubscribe_handler))
        .route("/api/stats", get(stats_handler))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;

    tracing::info!(event = "server.starting", listen_addr = %addr, "server.starting");

    let monitor = EarthquakeMonitor::new(db, config.clone(), bark_notifier)?;
    tokio::spawn(async move {
        if let Err(e) = monitor.start().await {
            tracing::error!(event = "monitor.task_failed", error = ?e, "monitor.task_failed");
        }
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
