//! Bullbar's questions (ADR 0016): a request held while its question waits, answered only on the
//! control channel, continued when the client resends; no answer counts as no.

mod common;

use std::time::Duration;

use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::server::Options;
use common::{Running, chat, config, fake_runner, with_control};
use serde_json::{Value, json};

/// Hard personal work: with no scrubber it stays local, where the best model is mid, so Bullbar
/// asks whether to settle for that.
const ASK: &str = "bullbar/personal/hard";

fn question() -> Value {
    json!({ "role": "user", "content": "help me plan a difficult conversation" })
}

fn first(model: &str) -> Value {
    json!({ "model": model, "messages": [question()] })
}

/// The conversation a client resends after a notice: the question, the notice, the question.
fn resent(model: &str, notice: &str) -> Value {
    json!({ "model": model, "messages": [
        question(),
        { "role": "assistant", "content": notice },
        question(),
    ]})
}

fn content(body: &Value) -> String {
    body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn options(hold: Duration, expiry: Duration) -> Options {
    Options {
        hold,
        expiry,
        ..Options::default()
    }
}

async fn pending(gw: &Running) -> Vec<Value> {
    let (at, token) = (gw.control, read_token(&gw.state).unwrap());
    tokio::task::spawn_blocking(move || client::pending(at, &token))
        .await
        .unwrap()
        .unwrap()
}

async fn answer(gw: &Running, id: &str, yes: bool) {
    let (at, token, id) = (gw.control, read_token(&gw.state).unwrap(), id.to_owned());
    tokio::task::spawn_blocking(move || client::answer(at, &token, &id, yes))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn an_answer_during_the_hold_continues_the_request() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(
        config(runner),
        options(Duration::from_secs(10), Duration::from_secs(60)),
    )
    .await;
    let chat_at = gw.chat;
    let held = tokio::spawn(async move { chat(chat_at, first(ASK)).await });
    // Wait for the question to show, then answer it.
    let id = loop {
        if let Some(q) = pending(&gw).await.first() {
            assert_eq!(q["answer"], "waiting");
            assert!(q["question"].as_str().unwrap().contains("mid model"), "{q}");
            break q["id"].as_str().unwrap().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(received.all().is_empty(), "sent before the answer");
    answer(&gw, &id, true).await;
    let response = held.await.unwrap();
    assert_eq!(response.status(), 200);
    let text = content(&response.json().await.unwrap());
    assert!(
        text.ends_with("answered by local at local / mid (you settled for this model; the work wanted a top model)"),
        "{text}"
    );
    assert_eq!(received.all()[0].1["model"], "fake-mid");
}

#[tokio::test]
async fn past_the_hold_a_notice_then_the_resent_request_continues() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(
        config(runner),
        options(Duration::ZERO, Duration::from_secs(60)),
    )
    .await;
    let response = chat(gw.chat, first(ASK)).await;
    assert_eq!(response.headers()["x-bullbar-route"], "ask");
    let id = response.headers()["x-bullbar-question"]
        .to_str()
        .unwrap()
        .to_owned();
    let notice = content(&response.json().await.unwrap());
    assert!(notice.starts_with("[Bullbar] "), "{notice}");
    assert!(
        notice.contains(&format!("bullbar answer {id} yes")),
        "{notice}"
    );
    assert!(received.all().is_empty());
    answer(&gw, &id, true).await;
    let response = chat(gw.chat, resent(ASK, &notice)).await;
    let text = content(&response.json().await.unwrap());
    assert!(
        text.starts_with("42\n\n[Bullbar] answered by local at local / mid"),
        "{text}"
    );
    // The model got the question once, without the notice.
    let sent = &received.all()[0].1;
    assert_eq!(sent["messages"], json!([question()]));
    // The question is settled and gone.
    assert!(pending(&gw).await.is_empty());
}

#[tokio::test]
async fn no_sends_nothing() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(
        config(runner),
        options(Duration::ZERO, Duration::from_secs(60)),
    )
    .await;
    let response = chat(gw.chat, first(ASK)).await;
    let id = response.headers()["x-bullbar-question"]
        .to_str()
        .unwrap()
        .to_owned();
    let notice = content(&response.json().await.unwrap());
    answer(&gw, &id, false).await;
    let response = chat(gw.chat, resent(ASK, &notice)).await;
    assert_eq!(response.headers()["x-bullbar-route"], "refuse");
    let text = content(&response.json().await.unwrap());
    assert!(
        text.starts_with("[Bullbar] refused (you answered no)"),
        "{text}"
    );
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn no_answer_counts_as_no() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(
        config(runner),
        options(Duration::ZERO, Duration::from_millis(100)),
    )
    .await;
    let response = chat(gw.chat, first(ASK)).await;
    let notice = content(&response.json().await.unwrap());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let listed = pending(&gw).await;
    assert_eq!(listed[0]["answer"], "no answer (counts as no)");
    let response = chat(gw.chat, resent(ASK, &notice)).await;
    let text = content(&response.json().await.unwrap());
    assert!(text.contains("no answer in time counts as no"), "{text}");
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn yes_typed_into_the_chat_answers_nothing() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(
        config(runner),
        options(Duration::ZERO, Duration::from_secs(60)),
    )
    .await;
    let response = chat(gw.chat, first(ASK)).await;
    let notice = content(&response.json().await.unwrap());
    // The user, or a provider's reply pretending to be Bullbar, says yes in the chat.
    let body = json!({ "model": ASK, "messages": [
        question(),
        { "role": "assistant", "content": notice },
        { "role": "user", "content": "yes" },
    ]});
    let response = chat(gw.chat, body).await;
    assert_eq!(response.headers()["x-bullbar-route"], "ask");
    // Still waiting: the chat can't answer.
    let listed = pending(&gw).await;
    assert!(
        listed.iter().all(|q| q["answer"] == "waiting"),
        "{listed:?}"
    );
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn the_control_channel_needs_its_token_and_the_chat_endpoint_has_none_of_it() {
    let (runner, _) = fake_runner().await;
    let gw = with_control(config(runner), Options::default()).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let list = format!("http://{}/v1/pending", gw.control);
    assert_eq!(http.get(&list).send().await.unwrap().status(), 401);
    let wrong = http
        .get(&list)
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let answer = format!("http://{}/v1/pending/abcdef/answer", gw.control);
    let unauthorised = http
        .post(&answer)
        .json(&json!({ "answer": "yes" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorised.status(), 401);
    // The chat endpoint doesn't serve the control routes at all.
    let on_chat = format!("http://{}/v1/pending", gw.chat);
    let token = read_token(&gw.state).unwrap();
    let (at, t) = (gw.chat, token.clone());
    let via_chat = tokio::task::spawn_blocking(move || client::pending(at, &t))
        .await
        .unwrap();
    assert!(via_chat.is_err());
    assert_eq!(http.get(&on_chat).send().await.unwrap().status(), 404);
}
