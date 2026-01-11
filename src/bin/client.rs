use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// Shared state
#[derive(Clone)]
struct AppState {
    counter: Arc<Mutex<u32>>,
}

// Request/Response DTOs
#[derive(Deserialize)]
struct IncrementRequest {
    amount: u32,
}

#[derive(Serialize)]
struct IncrementResponse {
    new_value: u32,
}

#[derive(Serialize)]
struct StatusResponse {
    message: String,
    counter: u32,
}

// RPC Endpoints
async fn increment(
    State(state): State<AppState>,
    Json(payload): Json<IncrementRequest>,
) -> Result<Json<IncrementResponse>, StatusCode> {
    let mut counter = state.counter.lock().await;
    *counter = counter.saturating_add(payload.amount);
    Ok(Json(IncrementResponse {
        new_value: *counter,
    }))
}

async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let counter = *state.counter.lock().await;
    Json(StatusResponse {
        message: "Server is running".to_string(),
        counter,
    })
}

#[tokio::main]
async fn main() {
    // Initialize shared state
    let app_state = AppState {
        counter: Arc::new(Mutex::new(0)),
    };

    // Build router
    let app = Router::new()
        .route("/rpc/increment", post(increment))
        .route("/status", get(get_status))
        .with_state(app_state);

    // Start server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    println!("RPC server running on http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}