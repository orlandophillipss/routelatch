use routelatch::{AppState, app};
#[tokio::main]
async fn main() {
    let address = std::env::var("ROUTELATCH_ADDR").unwrap_or_else(|_| "127.0.0.1:8082".into());
    let state = AppState::new(
        std::env::var("TYPESAFE_API_KEY").ok(),
        "https://api.typesafe.ai/v1/systemone".into(),
    );
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("bind address");
    println!("RouteLatch listening on http://{address}");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}
