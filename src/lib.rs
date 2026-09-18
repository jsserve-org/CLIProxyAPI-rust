pub mod auth;
pub mod chat;
pub mod claude;
pub mod config;
pub mod error;
pub mod management;
pub mod proxy;
pub mod routing;
pub mod security;
pub mod usage;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{any, get, patch, post},
};
use reqwest::redirect::Policy;
use tower::limit::ConcurrencyLimitLayer;

use auth::AuthStore;
use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub auth: Arc<AuthStore>,
    pub client: reqwest::Client,
    pub routing: Arc<routing::RoutingState>,
    pub usage: Arc<usage::UsageQueue>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self> {
        let auth = Arc::new(AuthStore::new(config.auth_dir.clone()).await?);
        let mut builder = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(config.timeout())
            .pool_max_idle_per_host(config.max_concurrency.min(16))
            .tcp_keepalive(std::time::Duration::from_secs(60));
        if !config.proxy_url.trim().is_empty() {
            builder =
                builder.proxy(reqwest::Proxy::all(&config.proxy_url).context("invalid proxy-url")?);
        }
        let client = builder.build()?;
        let usage = Arc::new(usage::UsageQueue::new(
            config.usage_statistics_enabled,
            config.redis_usage_queue_retention_seconds as i64,
        ));
        Ok(Self {
            routing: Arc::new(routing::RoutingState::new(&config)?),
            config: Arc::new(config),
            auth,
            client,
            usage,
        })
    }
}

pub fn api_router(state: AppState) -> Router {
    api_routes(&state).with_state(state)
}

fn api_routes(state: &AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/v1/models", get(proxy::models))
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route("/v1/completions", post(chat::completions))
        .route("/v1/responses", post(proxy::responses))
        .route("/v1/responses/compact", post(proxy::responses_compact))
        .route("/v1/messages", post(claude::messages))
        .route("/v1/messages/count_tokens", post(claude::count_tokens))
        .route("/backend-api/codex/{*path}", any(proxy::backend))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security::require_api_key,
        ));
    Router::new()
        .route("/", get(proxy::root))
        .route("/healthz", get(proxy::health).head(proxy::health))
        .merge(protected)
        .fallback(proxy::not_found)
        .method_not_allowed_fallback(proxy::not_found)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(ConcurrencyLimitLayer::new(state.config.max_concurrency))
}

pub fn admin_router(state: AppState) -> Router {
    let management = Router::new()
        .route("/config", get(management::config))
        .route(
            "/auth-files",
            get(management::auth_files)
                .post(management::upload)
                .delete(management::delete),
        )
        .route("/auth-files/download", get(management::download))
        .route("/auth-files/status", patch(management::patch_status))
        .route("/auth-files/refresh", post(management::refresh))
        .route("/api-call", post(management::api_call))
        .route("/usage-queue", get(management::usage_queue))
        .route(
            "/usage-statistics-enabled",
            get(management::get_usage_statistics_enabled)
                .put(management::put_usage_statistics_enabled)
                .patch(management::put_usage_statistics_enabled),
        )
        .route("/api-key-usage", get(management::api_key_usage))
        .fallback(management::not_implemented)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security::require_management_key,
        ));
    api_routes(&state)
        .nest("/v0/management", management)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(ConcurrencyLimitLayer::new(state.config.max_concurrency))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::{AppState, admin_router, api_router, config::Config};

    async fn state() -> AppState {
        let directory = tempdir().unwrap();
        let config = Config {
            auth_dir: directory.path().to_path_buf(),
            api_keys: vec!["api-test".into()],
            remote_management: crate::config::RemoteManagement {
                secret_key: "admin-test".into(),
            },
            ..Config::default()
        };
        AppState::new(config).await.unwrap()
    }

    #[tokio::test]
    async fn public_router_does_not_register_management() {
        let response = api_router(state().await)
            .oneshot(
                Request::get("/v0/management/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn public_unknown_paths_and_methods_are_plain_not_found() {
        for request in [
            Request::get("/").body(Body::empty()).unwrap(),
            Request::get("/definitely-not-a-route")
                .body(Body::empty())
                .unwrap(),
            Request::delete("/v1/models").body(Body::empty()).unwrap(),
        ] {
            let response = api_router(state().await).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert!(body.is_empty());
        }
    }

    #[tokio::test]
    async fn admin_router_requires_management_key() {
        let response = admin_router(state().await)
            .oneshot(
                Request::get("/v0/management/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn usage_statistics_toggle_round_trips() {
        let app = admin_router(state().await);
        let response = app
            .clone()
            .oneshot(
                Request::get("/v0/management/usage-statistics-enabled")
                    .header("authorization", "Bearer admin-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await["usage-statistics-enabled"],
            serde_json::json!(false)
        );

        let response = app
            .clone()
            .oneshot(
                Request::put("/v0/management/usage-statistics-enabled")
                    .header("authorization", "Bearer admin-test")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::get("/v0/management/usage-statistics-enabled")
                    .header("authorization", "Bearer admin-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["usage-statistics-enabled"],
            serde_json::json!(true)
        );
    }

    #[tokio::test]
    async fn usage_queue_pops_records_and_rejects_bad_count() {
        let app = state().await;
        let usage = app.usage.clone();
        usage.set_usage_statistics_enabled(true);
        usage.enqueue(serde_json::json!({"id": 1}));
        usage.enqueue(serde_json::json!({"id": 2}));
        let router = admin_router(app);

        let response = router
            .clone()
            .oneshot(
                Request::get("/v0/management/usage-queue?count=2")
                    .header("authorization", "Bearer admin-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            serde_json::json!([{"id": 1}, {"id": 2}])
        );

        let response = router
            .oneshot(
                Request::get("/v0/management/usage-queue?count=0")
                    .header("authorization", "Bearer admin-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
