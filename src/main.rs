mod config;
mod db;
mod i18n;
mod models;
mod parser;
mod routes;
mod session;
mod translator;

use std::sync::Arc;

use anyhow::Result;
use axum::{middleware, Router};

use crate::config::Config;
use crate::db::create_pool;
use crate::routes::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let config = Config::from_env()?;

    if let Some(log_file) = &config.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)?;
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let pool = create_pool(&config.database_url).await?;
    db::run_migrations(&pool).await?;

    let state = Arc::new(AppState {
        pool,
        default_api_key: config.default_api_key.clone(),
        session_secret: config.session_secret.clone(),
        cookie_secure: config.cookie_secure,
    });

    if state.default_api_key.is_empty() {
        tracing::warn!("No DEFAULT_API_KEY set — users must configure their own API key");
    }

    // 前端产物在 static/ 下（frontend/ 构建而来），后端只提供 /api 与静态回退
    let app: Router = routes::router()
        .layer(middleware::from_fn(routes::origin_guard))
        .with_state(state);

    let addr: std::net::SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("Server running on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
