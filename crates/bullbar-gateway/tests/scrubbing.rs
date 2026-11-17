//! Personal work leaving scrubbed and coming back restored, end to end against a fake provider on
//! loopback (ADR 0020). The scrubber's entity part is a stand-in that knows a few fake names; the
//! patterns are the real ones. Nothing here reaches a real provider.

mod common;

use bullbar_gateway::server::Options;
use common::{
    chat, cloud_config, fake_provider, fake_runner, fake_scrubber, key_file, read_stream,
    streaming, with_scrubber,
};
use serde_json::{Value, json};

/// A scrubber, a local runner and provider-a; no federation, so no topic pinning (the concept
/// design's slot table: "one account per provider, no topic pinning").
const SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
    "credential-scanner", "local-runner", "provider-adapters", "terms-register", "pii-scrubber"]
    pii_scrubber_grade = "entities""#;

const NAMES: &[&str] = &["Imogen Fakeworth"];

const MESSAGE: &str = "Write a note to Imogen Fakeworth: call 07700 900123 before 22nd of August.";

async fn running(name: &str) -> (common::Running, common::Received) {
    running_with(name, None).await
}

async fn running_with(name: &str, user: Option<&str>) -> (common::Running, common::Received) {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file(name);
    let key = format!("{:?}", key.display().to_string());
    let config = cloud_config(runner, provider, &key, SLOTS, user);
    (
        with_scrubber(config, Options::default(), fake_scrubber(NAMES)).await,
        cloud,
    )
}

fn personal(messages: Value) -> Value {
    json!({ "model": "bullbar/personal/hard", "messages": messages })
}

/// Everything the fake provider received, as one lowercase string.
fn received(cloud: &common::Received) -> String {
    cloud
        .all()
        .iter()
        .map(|(_, body)| body.to_string())
        .collect::<String>()
        .to_lowercase()
}

fn assert_clean(sent: &str) {
    for detail in ["imogen", "fakeworth", "07700", "22nd of august"] {
        assert!(
            !sent.contains(detail),
            "{detail} reached the provider: {sent}"
        );
    }
}

#[tokio::test]
async fn personal_work_leaves_scrubbed_and_comes_back_restored() {
    let (gw, cloud) = running("restored").await;
    let response = chat(
        gw.chat,
        personal(json!([{ "role": "user", "content": MESSAGE }])),
    )
    .await;
    assert_eq!(response.status(), 200);
    let route = response.headers()["x-bullbar-route"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(route.starts_with("provider-a"), "{route}");
    let body: Value = response.json().await.unwrap();
    let text = body["choices"][0]["message"]["content"].as_str().unwrap();
    assert_eq!(cloud.all().len(), 1);
    assert_clean(&received(&cloud));
    // The fake provider echoes what it got; restored, that is the user's own message.
    assert!(text.starts_with(&format!("you said: {MESSAGE}")), "{text}");
}

#[tokio::test]
async fn a_streamed_answer_is_restored_across_chunks() {
    let (gw, cloud) = running("streamed").await;
    let response = chat(
        gw.chat,
        streaming(personal(json!([{ "role": "user", "content": MESSAGE }]))),
    )
    .await;
    let streamed = read_stream(response).await;
    assert!(streamed.done);
    assert_clean(&received(&cloud));
    assert!(
        streamed.text.starts_with(&format!("you said: {MESSAGE}")),
        "{}",
        streamed.text
    );
}

#[tokio::test]
async fn the_next_turn_leaks_nothing_from_the_restored_answer() {
    let (gw, cloud) = running("next-turn").await;
    let first = chat(
        gw.chat,
        personal(json!([{ "role": "user", "content": MESSAGE }])),
    )
    .await;
    let first: Value = first.json().await.unwrap();
    let answer = first["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(answer.contains("Imogen Fakeworth"));
    let second = chat(
        gw.chat,
        personal(json!([
            { "role": "user", "content": MESSAGE },
            { "role": "assistant", "content": answer },
            { "role": "user", "content": "Shorter, and sign it from Imogen." },
        ])),
    )
    .await;
    assert_eq!(second.status(), 200);
    assert_eq!(cloud.all().len(), 2);
    assert_clean(&received(&cloud));
    // Both turns sent the same surrogate for the name.
    let bodies: Vec<String> = cloud
        .all()
        .iter()
        .map(|(_, b)| b["messages"][0]["content"].to_string())
        .collect();
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn a_credential_stays_a_placeholder_while_the_name_comes_back() {
    let (gw, cloud) = running_with("credential", Some("strip_and_send = true")).await;
    let user = "Imogen Fakeworth's key is AKIAFAKEFAKEFAKEFAKE";
    let response = chat(
        gw.chat,
        personal(json!([{ "role": "user", "content": user }])),
    )
    .await;
    let body: Value = response.json().await.unwrap();
    let text = body["choices"][0]["message"]["content"].as_str().unwrap();
    let sent = received(&cloud);
    assert!(
        !sent.contains("akiafake") && !sent.contains("imogen"),
        "{sent}"
    );
    assert!(text.contains("Imogen Fakeworth"), "{text}");
    assert!(
        text.contains("[CREDENTIAL_1]") && !text.contains("AKIAFAKE"),
        "{text}"
    );
}
