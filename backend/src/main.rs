//! picweight backend entry point.
//!
//! Two modes:
//!
//! * `picweight-backend` — start the HTTP server (the default).
//! * `picweight-backend openapi [path]` — write the OpenAPI spec and exit. The
//!   Android build regenerates its Retrofit client from `android/openapi.json`,
//!   so this must work without any runtime configuration — no database, no
//!   OIDC, no API key.
//!
//! This file owns process concerns only — configuration, logging, the pool, the
//! OIDC handshake, the background workers and the listener. The route table
//! itself is [`picweight_backend::build_router`], which lives in the library so
//! the integration suite exercises the same wiring the binary serves (PRD §13).

use std::net::SocketAddr;
use std::sync::Arc;

use picweight_backend::{
    agent::AgentHandle,
    api::{self, events::EventBus},
    auth, config::Config, db, jobs, storage, AppState, VERSION,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `openapi` needs no configuration at all — it must work in a build stage
    // where no secrets exist.
    let mut args = std::env::args().skip(1);
    if let Some(command) = args.next() {
        match command.as_str() {
            "openapi" => return export_openapi(args.next()),
            "--version" | "-V" | "version" => {
                println!("picweight-backend {VERSION}");
                return Ok(());
            }
            "serve" => {}
            other => {
                anyhow::bail!("unknown command {other:?}; expected `serve` or `openapi [path]`");
            }
        }
    }

    run_server().await
}

/// Write the OpenAPI spec to a file, or to stdout when no path is given.
fn export_openapi(output: Option<String>) -> anyhow::Result<()> {
    let spec = api::ApiDoc::openapi().to_pretty_json()?;
    match output {
        Some(path) => {
            std::fs::write(&path, &spec)?;
            eprintln!("OpenAPI spec written to {path}");
        }
        None => println!("{spec}"),
    }
    Ok(())
}

async fn run_server() -> anyhow::Result<()> {
    // Configuration first: it decides the log filter, and a missing required
    // variable should crash-loop with a readable reason rather than start.
    let config = Config::from_env()?;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&config.rust_log))
        .init();

    info!("picweight {VERSION} starting");
    info!("data path: {}", config.data_path.display());
    info!("static dir: {}", config.static_dir.display());
    info!("model: {}", config.openai_model);
    if config.openai_base_url != picweight_backend::config::DEFAULT_OPENAI_BASE_URL {
        info!("OpenAI base URL overridden: {}", config.openai_base_url);
    }
    if config.web_search_enabled {
        info!("web_search tool is ENABLED");
    }

    storage::ensure_dirs(&config)?;

    let pool = db::establish_pool_from_url(&config.database_url)?;
    db::run_migrations(&pool)?;
    info!("database ready at {}", config.database_url);

    let config = Arc::new(config);

    let http = reqwest::Client::builder()
        .user_agent(format!("picweight/{VERSION}"))
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let auth_state = auth::init_oidc(&config.oidc, &config.jwt_secret, config.jwt_ttl_secs).await?;
    info!("OIDC ready (issuer: {})", config.oidc.issuer);

    let agent = Arc::new(AgentHandle::new(&config, pool.clone(), http.clone())?);
    info!("agent ready (prompt version {})", agent.prompt_version());

    // The queue is created before the state because the state carries its
    // sender; the receiver goes to the worker pool immediately after.
    let (queue, receiver) = jobs::JobQueue::new();
    let events = EventBus::new(api::events::EVENT_BUFFER);

    let state = AppState::new(
        pool,
        config.clone(),
        auth_state,
        agent,
        events,
        queue,
        http,
    );

    // Recover anything a previous process left in flight before serving, so a
    // restart mid-analysis cannot lose a logged meal (G5).
    match jobs::requeue_unfinished(&state).await {
        Ok(0) => {}
        Ok(n) => info!("re-queued {n} unfinished analysis job(s)"),
        Err(e) => tracing::error!("failed to re-queue unfinished jobs: {e}"),
    }

    let worker_handle = jobs::spawn_workers(state.clone(), receiver);
    let settler_handle = jobs::group_settler::spawn(state.clone());

    // The route table itself lives in the library so the integration suite
    // exercises exactly this wiring (PRD §13). Only the process-level layers —
    // tracing and CORS — are added here.
    let app = picweight_backend::build_router(state.clone())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("HTTP server stopped; stopping background workers");
    worker_handle.abort();
    settler_handle.abort();
    info!("shutdown complete");
    Ok(())
}

/// Wait for SIGTERM (Kubernetes) or Ctrl-C (local).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("failed to register SIGTERM handler: {e}");
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => info!("received Ctrl-C, shutting down"),
            _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
        info!("received Ctrl-C, shutting down");
    }
}
