use std::{env, future::IntoFuture as _};

use garage_operator::{
    operator::{self, State},
    telemetry,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;

    // Grab needed env
    let garage_version =
        env::var("GARAGE_VERSION").expect("missing GARAGE_VERSION environment variable");

    // Initialize Kubernetes controller state
    let state = State::default();
    let controller = operator::GarageController::new(state.clone());

    // Start web server
    let listener = TcpListener::bind("0.0.0.0:8080").await.unwrap();
    let router = handlers::router();
    let server = axum::serve(listener, router.with_state(state));

    // Run both the http server and the controller, throwing a panic if either finish early
    tokio::select! {
        c = controller.run(garage_version) => {
            panic!("controller exited early: {}", c.unwrap_err())
        },
        s = server.into_future() => {
            panic!("server exited early: {}", s.unwrap_err())
        }
    };
}

/// Handlers for the web server portion of the operator
mod handlers {
    use axum::{extract::State, http::StatusCode, response::IntoResponse, routing, Json, Router};
    use prometheus::{Encoder, TextEncoder};

    use garage_operator::operator::State as OperatorState;

    /// Construct the router for all the handlers
    pub fn router() -> Router<OperatorState> {
        Router::new()
            .route("/metrics", routing::get(metrics))
            .route("/health", routing::get(health))
            .route("/", routing::get(index))
    }

    /// Handler for exposing prometheus metrics
    async fn metrics(State(state): State<OperatorState>) -> impl IntoResponse {
        let metrics = state.metrics();
        let encoder = TextEncoder::new();
        let mut buffer = vec![];
        encoder.encode(&metrics, &mut buffer).unwrap();

        (StatusCode::OK, buffer)
    }

    /// Handler for checking the health of the server
    async fn health() -> impl IntoResponse {
        (StatusCode::OK, Json("healthy"))
    }

    /// Handler for interacting with the operator
    async fn index(State(state): State<OperatorState>) -> impl IntoResponse {
        let diagnostics = state.diagnostics().await;

        (StatusCode::OK, Json(diagnostics))
    }
}
