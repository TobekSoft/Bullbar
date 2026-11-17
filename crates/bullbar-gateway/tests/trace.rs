//! `bullbar trace` end to end (ADR 0018 amendment): for a while, on request, what left a cloud
//! account and the provider's raw reply are logged; never the originals, never local answers.
//! Against fake providers on loopback.

mod common;

use bullbar_core::policy::{AdminPolicy, Policy};
use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::server::Options;
use common::{
    Running, chat, cloud_config, fake_provider, fake_runner, fake_scrubber, key_file,
    refusing_provider, with_control, with_scrubber,
};
use serde_json::{Value, json};

const SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
    "credential-scanner", "local-runner", "provider-adapters", "terms-register", "pii-scrubber"]
    pii_scrubber_grade = "entities""#;

fn asking(model: &str, text: &str, stream: bool) -> Value {
    json!({ "model": model, "stream": stream, "messages": [{ "role": "user", "content": text }] })
}

async fn trace(gw: &Running, requests: Option<u32>) -> Result<Value, String> {
    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    tokio::task::spawn_blocking(move || client::trace(at, &token, requests, false))
        .await
        .unwrap()
        .map_err(|e| e.to_string())
}

fn logged(gw: &Running) -> String {
    std::fs::read_to_string(gw.state.join("trace.jsonl")).unwrap_or_default()
}

#[tokio::test]
async fn a_trace_logs_what_left_and_the_reply_never_the_original() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = format!("{:?}", key_file("trace").display().to_string());
    let config = cloud_config(runner, provider, &key, SLOTS, None);
    let gw = with_scrubber(config, Options::default(), fake_scrubber(&["Imogen"])).await;
    // Off at start: nothing is written.
    chat(
        gw.chat,
        asking("bullbar/personal/hard", "Imogen says hi", false),
    )
    .await;
    assert!(!gw.state.join("trace.jsonl").exists());

    let now = trace(&gw, Some(2)).await.unwrap();
    assert_eq!(now["left"], 2);
    let whole = chat(
        gw.chat,
        asking("bullbar/personal/hard", "Imogen says hi", false),
    )
    .await;
    let text: Value = whole.json().await.unwrap();
    let text = text["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        text.contains("trace on: what left and the reply are logged"),
        "{text}"
    );
    assert!(text.contains("Imogen"), "restored as usual: {text}");
    // Answered at home: never traced, and doesn't use up the count.
    chat(
        gw.chat,
        asking("bullbar/public/easy", "Imogen at home", false),
    )
    .await;
    let streamed = chat(
        gw.chat,
        asking("bullbar/personal/hard", "Imogen again", true),
    )
    .await;
    let streamed = streamed.text().await.unwrap();
    assert!(streamed.contains("trace on"), "{streamed}");

    let log = logged(&gw);
    let lines: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        !log.to_lowercase().contains("imogen"),
        "an original was logged: {log}"
    );
    assert!(!log.contains("Imogen at home"));
    assert_eq!(lines.iter().filter(|l| l.get("sent").is_some()).count(), 2);
    assert!(lines.iter().any(|l| l.get("reply").is_some()));
    assert!(lines.iter().any(|l| l.get("chunk").is_some()));
    // Two requests, and the trace is over.
    assert_eq!(trace_state(&gw).await, json!({ "on": false }));
    chat(
        gw.chat,
        asking("bullbar/personal/hard", "Imogen once more", false),
    )
    .await;
    assert_eq!(logged(&gw), log);
}

async fn trace_state(gw: &Running) -> Value {
    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    tokio::task::spawn_blocking(move || client::status(at, &token))
        .await
        .unwrap()
        .unwrap()["trace"]
        .clone()
}

#[tokio::test]
async fn a_trace_keeps_the_providers_own_error() {
    let (runner, _) = fake_runner().await;
    let provider = refusing_provider(400).await;
    let key = format!("{:?}", key_file("trace-error").display().to_string());
    let config = cloud_config(runner, provider, &key, common::CLOUD_SLOTS, None);
    let gw = with_control(config, Options::default()).await;
    trace(&gw, Some(5)).await.unwrap();
    for stream in [false, true] {
        chat(
            gw.chat,
            asking("bullbar/public/hard", "sixty-four squared", stream),
        )
        .await;
    }
    let log = logged(&gw);
    // The provider's words, which the reply never shows, are what a trace is for.
    assert_eq!(
        log.matches("cannot answer: sixty-four squared").count(),
        2,
        "{log}"
    );
    assert!(!log.contains(common::FAKE_PROVIDER_KEY));
}

#[tokio::test]
async fn where_the_policy_forbids_it_a_trace_never_starts() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = format!("{:?}", key_file("trace-forbidden").display().to_string());
    let mut config = cloud_config(runner, provider, &key, common::CLOUD_SLOTS, None);
    config.policy = Policy::from_admin(AdminPolicy {
        trace_allowed: false,
        ..AdminPolicy::default()
    });
    let gw = with_control(config, Options::default()).await;
    let refused = trace(&gw, Some(5)).await.unwrap_err();
    assert!(refused.contains("forbids"), "{refused}");
    chat(gw.chat, asking("bullbar/public/hard", "hello", false)).await;
    assert!(!gw.state.join("trace.jsonl").exists());
}
