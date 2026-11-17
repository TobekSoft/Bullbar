//! Requests through the endpoint, end to end, against a fake local runner.

mod common;

use bullbar_gateway::server::Options;
use common::{ask, chat, config, fake_runner, gateway};
use serde_json::{Value, json};

fn content(body: &Value) -> &str {
    body["choices"][0]["message"]["content"].as_str().unwrap()
}

#[tokio::test]
async fn a_local_route_is_answered_by_the_runner_with_the_source_shown() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let response = chat(gw, ask("bullbar/public/easy")).await;
    assert_eq!(response.status(), 200);
    let route = response.headers()["x-bullbar-route"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(route.starts_with("local at"), "{route}");
    assert!(response.headers().contains_key("x-bullbar-why"));
    let body: Value = response.json().await.unwrap();
    assert!(
        content(&body).starts_with("42\n\n[Bullbar] answered by local at"),
        "{body}"
    );
    // The client sees the name it asked for; the runner got the configured model.
    assert_eq!(body["model"], "bullbar/public/easy");
    let received = received.all();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1["model"], "fake-small");
}

#[tokio::test]
async fn the_footer_can_be_turned_off() {
    let (runner, _) = fake_runner().await;
    let gw = gateway(
        config(runner),
        Options {
            footer: false,
            ..Options::default()
        },
    )
    .await;
    let body: Value = chat(gw, ask("bullbar/public/easy"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(content(&body), "42");
}

#[tokio::test]
async fn a_route_that_leaves_is_not_carried_out() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    // Hard needs the top column, which only the cloud account offers.
    let response = chat(gw, ask("bullbar/public/hard")).await;
    assert_eq!(response.status(), 200);
    let route = response.headers()["x-bullbar-route"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(route.starts_with("provider-a"), "{route}");
    let body: Value = response.json().await.unwrap();
    assert!(content(&body).starts_with("[Bullbar] this request would go to provider-a"));
    assert!(content(&body).contains("nothing was sent"));
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn a_model_name_bullbar_does_not_know_is_a_bad_request() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let response = chat(gw, ask("bullbar/pubic")).await;
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("[Bullbar]")
    );
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn a_runner_that_is_down_fails_closed() {
    // Bind a port, then free it: nothing listens there.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);
    let gw = gateway(config(dead), Options::default()).await;
    let response = chat(gw, ask("bullbar/public/easy")).await;
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("[Bullbar]")
    );
}

#[tokio::test]
async fn bullbar_words_never_reach_the_runner() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let body = json!({
        "model": "bullbar/public/easy",
        "messages": [
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "[Bullbar] refused; nothing was sent." },
            { "role": "user", "content": "second" },
            { "role": "assistant", "content": "42\n\n[Bullbar] answered at local / small" },
            { "role": "user", "content": "third" },
        ],
    });
    assert_eq!(chat(gw, body).await.status(), 200);
    let sent = &received.all()[0].1;
    let text = sent["messages"].to_string();
    assert!(!text.contains("[Bullbar]"), "{text}");
    assert_eq!(sent["messages"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn a_local_account_on_the_internet_stops_the_gateway() {
    let mut config = config("127.0.0.1:1".parse().unwrap());
    for account in &mut config.accounts {
        if let Some(address) = &mut account.address {
            address.base_url = "https://203.0.113.9/v1".to_owned();
        }
    }
    let err = bullbar_gateway::server::router(config, Options::default(), common::wiring())
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("on the internet"), "{err}");
}

#[tokio::test]
async fn models_lists_the_bullbar_names() {
    let (runner, _) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let body: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{gw}/v1/models"))
        .bearer_auth(common::TEST_CHAT_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"bullbar"));
    assert!(ids.contains(&"bullbar/public/hard"));
}
