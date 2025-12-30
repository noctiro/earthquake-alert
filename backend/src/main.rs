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
    AppState, health_handler, stats_handler, subscribe_handler, unsubscribe_by_path_handler,
};
use services::EarthquakeMonitor;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "earthquake_alert_backend=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 加载配置
    let config = Config::from_env();
    tracing::info!("配置加载完成: {:?}", config);

    // 打开数据库
    let db = Database::open(&config.db_path)?;
    tracing::info!("数据库已打开: {}", config.db_path);

    // 创建应用状态
    let state = AppState { db: db.clone() };

    // 创建路由
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/api/subscribe", post(subscribe_handler))
        .route(
            "/api/unsubscribe/{bark_id}",
            delete(unsubscribe_by_path_handler),
        )
        .route("/api/stats", get(stats_handler))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    // 启动服务器
    let addr: SocketAddr = format!("{}:{}", config.server_host, config.server_port).parse()?;

    tracing::info!("服务器启动中: http://{}", addr);

    // 在后台任务中启动地震监控（支持百万级并发）
    let monitor = EarthquakeMonitor::new(
        db,
        config.bark_api_url.clone(),
        config.http_pool_size,
        config.max_concurrent_notifications,
        config.batch_size,
    );
    tokio::spawn(async move {
        if let Err(e) = monitor.start().await {
            tracing::error!("地震监控服务错误: {:?}", e);
        }
    });

    // 启动 HTTP 服务器
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
