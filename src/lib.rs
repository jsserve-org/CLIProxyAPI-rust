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

use std::sync::{Arc, RwLock};

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
    pub config: Arc<RwLock<Config>>,
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
        let routing = Arc::new(routing::RoutingState::new(&config)?);
        Ok(Self {
            routing,
            config: Arc::new(RwLock::new(config)),
            auth,
            client,
            usage,
        })
    }

    pub fn config(&self) -> std::sync::RwLockReadGuard<'_, Config> {
        self.config.read().unwrap()
    }
}

pub fn api_router(state: AppState) -> Router {
    api_routes(&state).with_state(state)
}

fn api_routes(state: &AppState) -> Router<AppState> {
    let max_body_bytes = state.config().max_body_bytes;
    let max_concurrency = state.config().max_concurrency;
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
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(ConcurrencyLimitLayer::new(max_concurrency))
}

pub fn admin_router(state: AppState) -> Router {
    let max_body_bytes = state.config().max_body_bytes;
    let max_concurrency = state.config().max_concurrency;
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
        .route(
            "/debug",
            get(management::get_debug)
                .put(management::put_debug)
                .patch(management::put_debug),
        )
        .route(
            "/logging-to-file",
            get(management::get_logging_to_file)
                .put(management::put_logging_to_file)
                .patch(management::put_logging_to_file),
        )
        .route(
            "/logs-max-total-size-mb",
            get(management::get_logs_max_total_size_mb)
                .put(management::put_logs_max_total_size_mb)
                .patch(management::put_logs_max_total_size_mb),
        )
        .route(
            "/error-logs-max-files",
            get(management::get_error_logs_max_files)
                .put(management::put_error_logs_max_files)
                .patch(management::put_error_logs_max_files),
        )
        .route(
            "/request-retry",
            get(management::get_request_retry)
                .put(management::put_request_retry)
                .patch(management::put_request_retry),
        )
        .route(
            "/max-retry-credentials",
            get(management::get_max_retry_credentials)
                .put(management::put_max_retry_credentials)
                .patch(management::put_max_retry_credentials),
        )
        .route(
            "/max-retry-interval",
            get(management::get_max_retry_interval)
                .put(management::put_max_retry_interval)
                .patch(management::put_max_retry_interval),
        )
        .route(
            "/force-model-prefix",
            get(management::get_force_model_prefix)
                .put(management::put_force_model_prefix)
                .patch(management::put_force_model_prefix),
        )
        .route(
            "/proxy-url",
            get(management::get_proxy_url)
                .put(management::put_proxy_url)
                .patch(management::put_proxy_url)
                .delete(management::delete_proxy_url),
        )
        .route(
            "/routing/strategy",
            get(management::get_routing_strategy)
                .put(management::put_routing_strategy)
                .patch(management::put_routing_strategy),
        )
        .route(
            "/api-keys",
            get(management::get_api_keys)
                .put(management::put_api_keys)
                .patch(management::patch_api_keys)
                .delete(management::delete_api_keys),
        )
        .fallback(management::not_implemented)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security::require_management_key,
        ));
    let router = api_routes(&state)
        .nest("/v0/management", management)
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(ConcurrencyLimitLayer::new(max_concurrency));
    router.with_state(state)
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

    fn admin_get(uri: &str) -> Request<Body> {
        Request::get(uri)
            .header("authorization", "Bearer admin-test")
            .body(Body::empty())
            .unwrap()
    }

    fn admin_json(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", "Bearer admin-test")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn management_config_toggles_round_trip() {
        let app = admin_router(state().await);
        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/debug"))
            .await
            .unwrap();
        assert_eq!(body_json(response).await["debug"], serde_json::json!(false));

        let response = app
            .clone()
            .oneshot(admin_json(
                "PUT",
                "/v0/management/debug",
                r#"{"value":true}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/request-retry"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["request-retry"],
            serde_json::json!(2)
        );

        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/v0/management/request-retry",
                r#"{"value":5}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(admin_get("/v0/management/request-retry"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["request-retry"],
            serde_json::json!(5)
        );
    }

    #[tokio::test]
    async fn management_routing_strategy_validates_and_normalizes() {
        let app = admin_router(state().await);
        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/routing/strategy"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["strategy"],
            serde_json::json!("round-robin")
        );

        let response = app
            .clone()
            .oneshot(admin_json(
                "PUT",
                "/v0/management/routing/strategy",
                r#"{"value":"wrr"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/routing/strategy"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["strategy"],
            serde_json::json!("weighted-round-robin")
        );

        let response = app
            .oneshot(admin_json(
                "PUT",
                "/v0/management/routing/strategy",
                r#"{"value":"bogus"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn management_api_keys_lifecycle() {
        let app = admin_router(state().await);
        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/api-keys"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["api-keys"],
            serde_json::json!(["api-test"])
        );

        let response = app
            .clone()
            .oneshot(
                Request::put("/v0/management/api-keys")
                    .header("authorization", "Bearer admin-test")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"["k1","k2"]"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/v0/management/api-keys",
                r#"{"old":"k1","new":"k3"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(admin_get("/v0/management/api-keys"))
            .await
            .unwrap();
        assert_eq!(
            body_json(response).await["api-keys"],
            serde_json::json!(["k3", "k2"])
        );

        let response = app
            .oneshot(
                Request::delete("/v0/management/api-keys?value=k2")
                    .header("authorization", "Bearer admin-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
