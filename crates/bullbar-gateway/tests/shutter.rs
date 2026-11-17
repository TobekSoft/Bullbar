//! The kill switch, status and the decision log: closing stops everything that leaves while local
//! answers go on; it survives a restart; only the control token reopens it; counts and logs hold no
//! content.

mod common;

use std::time::Duration;

use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::server::Options;
use common::{
    CLOUD_SLOTS, Running, chat, cloud_config, fake_provider, fake_runner, fresh_state, key_file,
    with_control_in,
};
use serde_json::{Value, json};

const SECRET_TEXT: &str = "the text of my request must never be logged";
const FAKE_KEY: &str = "AKIAFAKEFAKEFAKEFAKE";

fn asking(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

fn options() -> Options {
    Options {
        hold: Duration::ZERO,
        ..Options::default()
    }
}

async fn control<T: Send + 'static>(
    gw: &Running,
    f: impl FnOnce(std::net::SocketAddr, &bullbar_gateway::control::ControlToken) -> T + Send + 'static,
) -> T {
    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    tokio::task::spawn_blocking(move || f(at, &token))
        .await
        .unwrap()
}

async fn route_of(gw: &Running, model: &str) -> (String, String) {
    let response = chat(gw.chat, asking(model, SECRET_TEXT)).await;
    let header = |name: &str| response.headers()[name].to_str().unwrap().to_owned();
    (header("x-bullbar-route"), header("x-bullbar-why"))
}

async fn start(state: &std::path::Path) -> (Running, common::Received, common::Received) {
    start_with(state, options()).await
}

async fn start_with(
    state: &std::path::Path,
    options: Options,
) -> (Running, common::Received, common::Received) {
    let (runner, local) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file(&format!("shutter-{}", state.display().to_string().len()));
    let key = format!("{:?}", key.display().to_string());
    let config = cloud_config(runner, provider, &key, CLOUD_SLOTS, None);
    (with_control_in(config, options, state).await, local, cloud)
}

/// Found in the hand check (2026-11-16): a question asked while the shutter was closed and left to
/// expire settled as "no", and the same conversation sent again after reopening was refused with
/// that stale answer, though it could now go out. A settled answer only counts while a fresh
/// decision asks the same question.
#[tokio::test]
async fn a_stale_answer_does_not_override_a_fresh_decision() {
    let state = fresh_state();
    let (gw, _, cloud) = start_with(
        &state,
        Options {
            hold: Duration::ZERO,
            expiry: Duration::from_millis(100),
            ..Options::default()
        },
    )
    .await;
    control(&gw, |at, t| client::shutter(at, t, true, Some("drill")))
        .await
        .unwrap();
    assert_eq!(route_of(&gw, "bullbar/public/hard").await.0, "ask");
    tokio::time::sleep(Duration::from_millis(200)).await;
    control(&gw, |at, t| client::shutter(at, t, false, None))
        .await
        .unwrap();
    let (route, why) = route_of(&gw, "bullbar/public/hard").await;
    assert!(route.starts_with("provider-a"), "{route}: {why}");
    assert_eq!(cloud.all().len(), 1);
}

#[tokio::test]
async fn kill_stops_what_leaves_and_local_goes_on() {
    let state = fresh_state();
    let (gw, local, cloud) = start(&state).await;
    assert!(
        route_of(&gw, "bullbar/public/hard")
            .await
            .0
            .starts_with("provider-a")
    );
    assert_eq!(cloud.all().len(), 1);

    let closed = control(&gw, |at, t| client::shutter(at, t, true, Some("drill")))
        .await
        .unwrap();
    assert_eq!(closed["closed"], true);
    // Hard work can't leave: Bullbar offers the local mid model instead, and says why.
    let (route, why) = route_of(&gw, "bullbar/public/hard").await;
    assert_eq!(route, "ask");
    assert!(why.contains("the shutter is closed"), "{why}");
    // Easy work is answered locally as before.
    assert!(
        route_of(&gw, "bullbar/public/easy")
            .await
            .0
            .starts_with("local at")
    );
    assert_eq!(cloud.all().len(), 1, "something left while closed");
    assert_eq!(local.all().len(), 1);
    assert_eq!(gw.events.all(), ["shutter closed: drill"]);

    control(&gw, |at, t| client::shutter(at, t, false, None))
        .await
        .unwrap();
    assert!(
        route_of(&gw, "bullbar/public/hard")
            .await
            .0
            .starts_with("provider-a")
    );
    assert_eq!(gw.events.all().last().unwrap(), "shutter reopened");
}

#[tokio::test]
async fn a_closed_shutter_stays_closed_across_a_restart() {
    let state = fresh_state();
    let (gw, _, _) = start(&state).await;
    control(&gw, |at, t| client::shutter(at, t, true, None))
        .await
        .unwrap();
    // A new gateway on the same state folder, as after a restart.
    let (again, _, cloud) = start(&state).await;
    let (route, _) = route_of(&again, "bullbar/public/hard").await;
    assert_eq!(route, "ask");
    assert!(cloud.all().is_empty());
    let status = control(&again, client::status).await.unwrap();
    assert_eq!(status["shutter"]["closed"], true);
    assert_eq!(status["shutter"]["reason"], "closed by hand");
}

#[tokio::test]
async fn only_the_control_token_reopens() {
    let state = fresh_state();
    let (gw, _, _) = start(&state).await;
    control(&gw, |at, t| client::shutter(at, t, true, None))
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{}/v1/shutter", gw.control);
    for token in [None, Some("not-the-token")] {
        let mut request = http.post(&url).json(&json!({ "close": false }));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    let on_chat = format!("http://{}/v1/shutter", gw.chat);
    assert_eq!(
        http.post(&on_chat)
            .json(&json!({ "close": false }))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let status = control(&gw, client::status).await.unwrap();
    assert_eq!(status["shutter"]["closed"], true);
}

#[tokio::test]
async fn status_counts_without_content() {
    let state = fresh_state();
    let (gw, _, _) = start(&state).await;
    route_of(&gw, "bullbar/public/easy").await;
    route_of(&gw, "bullbar/public/hard").await;
    route_of(&gw, "bullbar/personal/hard").await; // stays local, asks to settle for mid
    let status = control(&gw, client::status).await.unwrap();
    let counts = &status["counts"];
    assert_eq!(counts["local"], 1);
    assert_eq!(counts["left"], 1);
    assert_eq!(counts["asked"], 1);
    assert_eq!(status["questions"]["waiting"], 1);
    assert_eq!(status["shutter"]["closed"], false);
    assert!(!status.to_string().contains(SECRET_TEXT));
}

#[tokio::test]
async fn the_decision_log_holds_routes_and_reasons_never_content() {
    let state = fresh_state();
    let (gw, _, _) = start(&state).await;
    route_of(&gw, "bullbar/public/easy").await;
    let body = asking("bullbar/public/hard", &format!("{SECRET_TEXT} {FAKE_KEY}"));
    chat(gw.chat, body).await;
    let lines = gw.decisions.all();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("decision local route=local at local / small why="),
        "{lines:?}"
    );
    for line in &lines {
        assert!(!line.contains(SECRET_TEXT), "{line}");
        assert!(!line.contains(FAKE_KEY), "{line}");
    }
}
