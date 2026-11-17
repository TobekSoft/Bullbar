//! The chat endpoint's token (ADR 0016, option C): every chat route needs it, each listener takes
//! only its own token, and it never travels on to a model.

mod common;

use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::server::Options;
use common::{
    CLOUD_SLOTS, FAKE_PROVIDER_KEY, TEST_CHAT_TOKEN, ask, chat, cloud_config, config,
    fake_provider, fake_runner, key_file, with_control,
};
use serde_json::Value;

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn a_chat_request_without_the_token_is_turned_away_unread() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(config(runner), Options::default()).await;
    let url = format!("http://{}/v1/chat/completions", gw.chat);
    let control = read_token(&gw.state).unwrap();
    // No token, a wrong one, and the control token: none is the chat token.
    let offered = [
        None,
        Some("not-the-token".to_owned()),
        Some(
            std::fs::read_to_string(gw.state.join("control.token"))
                .unwrap()
                .trim()
                .to_owned(),
        ),
    ];
    assert!(control.matches(offered[2].as_deref().unwrap()));
    for token in offered {
        let mut request = http().post(&url).json(&ask("bullbar/public/easy"));
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401, "{token:?}");
        let body: Value = response.json().await.unwrap();
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("[Bullbar] the chat token"), "{message}");
    }
    let models = http()
        .get(format!("http://{}/v1/models", gw.chat))
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), 401);
    assert!(received.all().is_empty());
    // Turned away before deciding: nothing counted, nothing logged.
    assert!(gw.decisions.all().is_empty());
}

#[tokio::test]
async fn the_chat_token_works_nothing_on_the_control_channel() {
    let (runner, _) = fake_runner().await;
    let gw = with_control(config(runner), Options::default()).await;
    let pending = format!("http://{}/v1/pending", gw.control);
    let response = http()
        .get(&pending)
        .bearer_auth(TEST_CHAT_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    // The control token still does.
    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    let listed = tokio::task::spawn_blocking(move || client::pending(at, &token))
        .await
        .unwrap();
    assert!(listed.is_ok());
}

#[tokio::test]
async fn the_chat_token_never_reaches_a_model() {
    let (runner, local) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("chat-token");
    let config = cloud_config(
        runner,
        provider,
        &format!("{:?}", key.display().to_string()),
        CLOUD_SLOTS,
        None,
    );
    let gw = with_control(config, Options::default()).await;
    assert_eq!(
        chat(gw.chat, ask("bullbar/public/easy")).await.status(),
        200
    );
    assert_eq!(
        chat(gw.chat, ask("bullbar/public/hard")).await.status(),
        200
    );
    let (local, cloud) = (local.all(), cloud.all());
    assert_eq!((local.len(), cloud.len()), (1, 1));
    for (headers, body) in local.iter().chain(&cloud) {
        for (_, value) in headers {
            assert!(!value.to_str().unwrap_or_default().contains(TEST_CHAT_TOKEN));
        }
        assert!(!body.to_string().contains(TEST_CHAT_TOKEN));
    }
    assert_eq!(
        cloud[0].0["authorization"].to_str().unwrap(),
        format!("Bearer {FAKE_PROVIDER_KEY}")
    );
}
