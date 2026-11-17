//! Topic pinning end to end (ADR 0021): a topic's personal work goes to the first account it left
//! for, recorded in the state folder; a pin that can't be kept asks before moving. Against a fake
//! provider on loopback; nothing here reaches a real provider.

mod common;

use std::time::Duration;

use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::server::{Options, router};
use common::{
    Running, chat, cloud_config, fake_provider, fake_runner, fake_scrubber, fresh_state, key_file,
    with_scrubber_in,
};
use serde_json::{Value, json};

/// A scrubber, a local runner, provider-a, and federation for topic pinning.
const SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
    "credential-scanner", "local-runner", "provider-adapters", "terms-register", "pii-scrubber",
    "federation"]
    pii_scrubber_grade = "entities""#;

const HEALTH: &str = "bullbar/personal/hard/health";

fn asking(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

async fn start(name: &str, state: &std::path::Path) -> (Running, common::Received) {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = format!("{:?}", key_file(name).display().to_string());
    let config = cloud_config(runner, provider, &key, SLOTS, None);
    let options = Options {
        hold: Duration::from_millis(200),
        ..Options::default()
    };
    let gw = with_scrubber_in(config, options, state, fake_scrubber(&["Imogen"])).await;
    (gw, cloud)
}

fn pins(state: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state.join("pins.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn a_topics_first_trip_out_pins_it() {
    let state = fresh_state();
    let (gw, cloud) = start("pins-first", &state).await;
    let response = chat(gw.chat, asking(HEALTH, "Imogen's results came back")).await;
    assert!(
        response.headers()["x-bullbar-route"]
            .to_str()
            .unwrap()
            .starts_with("provider-a"),
        "{:?}",
        response.headers()
    );
    assert_eq!(cloud.all().len(), 1);
    assert_eq!(pins(&state), json!({ "health": "provider-a" }));
    // Public work in the topic leaves no pin of its own and isn't held to it.
    let public = chat(
        gw.chat,
        asking("bullbar/public/hard/general", "what is 6 x 7?"),
    )
    .await;
    assert_eq!(public.status(), 200);
    assert_eq!(pins(&state), json!({ "health": "provider-a" }));
}

#[tokio::test]
async fn a_pin_that_cant_be_kept_asks_and_moves_only_on_yes() {
    let state = fresh_state();
    std::fs::create_dir_all(&state).unwrap();
    // Pinned to an account this setup no longer has.
    std::fs::write(state.join("pins.json"), r#"{ "health": "provider-z" }"#).unwrap();
    let (gw, cloud) = start("pins-move", &state).await;
    let body = asking(HEALTH, "Imogen's results came back");
    let response = chat(gw.chat, body.clone()).await;
    assert_eq!(response.headers()["x-bullbar-route"], "ask");
    let id = response.headers()["x-bullbar-question"]
        .to_str()
        .unwrap()
        .to_owned();
    let text: Value = response.json().await.unwrap();
    let text = text["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(
        text.contains("pinned to provider-z") && text.contains("Move the topic to provider-a"),
        "{text}"
    );
    assert!(cloud.all().is_empty());
    assert_eq!(pins(&state), json!({ "health": "provider-z" }));

    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    tokio::task::spawn_blocking(move || client::answer(at, &token, &id, true))
        .await
        .unwrap()
        .unwrap();
    let resent = chat(gw.chat, body).await;
    assert!(
        resent.headers()["x-bullbar-route"]
            .to_str()
            .unwrap()
            .starts_with("provider-a")
    );
    assert_eq!(cloud.all().len(), 1);
    assert_eq!(pins(&state), json!({ "health": "provider-a" }));
}

#[tokio::test]
async fn a_damaged_pins_file_stops_the_start() {
    let state = fresh_state();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("pins.json"), "{ not json").unwrap();
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = format!("{:?}", key_file("pins-damaged").display().to_string());
    let config = cloud_config(runner, provider, &key, SLOTS, None);
    let (wiring, ..) = common::wiring_in(&state);
    let started = router(config, Options::default(), wiring).await;
    assert!(started.is_err_and(|e| e.to_string().contains("pins")));
}
