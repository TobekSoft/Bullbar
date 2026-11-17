//! The always-on floor in the request path: what the credential scanner finds changes the decision,
//! and what would leave passes the outbound check first.

mod common;

use bullbar_gateway::server::Options;
use common::{chat, config, fake_runner, gateway, no_hold};
use serde_json::{Value, json};

const FAKE_KEY: &str = "AKIAFAKEFAKEFAKEFAKE";

fn asking(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

#[tokio::test]
async fn a_request_with_credentials_does_not_leave() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), no_hold()).await;
    // Hard public work would go out. The key keeps it on the local layer, whose best model is
    // mid, so Bullbar asks whether to settle for that; unanswered, nothing goes anywhere.
    let response = chat(
        gw,
        asking("bullbar/public/hard", &format!("my key {FAKE_KEY} fails")),
    )
    .await;
    let route = response.headers()["x-bullbar-route"]
        .to_str()
        .unwrap()
        .to_owned();
    let why = response.headers()["x-bullbar-why"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(route, "ask");
    assert!(why.contains("credentials"), "{why}");
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn the_local_runner_may_see_credentials() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let response = chat(
        gw,
        asking("bullbar/public/easy", &format!("my key {FAKE_KEY} fails")),
    )
    .await;
    assert_eq!(response.status(), 200);
    // Credentials stay on the local layer, not off it: the runner gets the request as written.
    assert!(received.all()[0].1.to_string().contains(FAKE_KEY));
}
