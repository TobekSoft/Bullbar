//! Streamed replies (`stream: true`): answers relayed chunk by chunk with the footer before the
//! end, Bullbar's own replies as chunks, cut-off answers marked, and questions asked on the open
//! stream.

mod common;

use std::time::Duration;

use bullbar_gateway::control::{client, read_token};
use bullbar_gateway::reply::ANSWER_SEPARATOR;
use bullbar_gateway::server::Options;
use common::{
    CLOUD_SLOTS, Running, ask, chat, cloud_config, config, fake_provider, fake_runner, gateway,
    key_file, read_stream, streaming, with_control,
};
use serde_json::{Value, json};

/// Hard personal work: with no scrubber it stays local, where the best model is mid, so Bullbar
/// asks whether to settle for that.
const ASK: &str = "bullbar/personal/hard";

fn saying(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

fn options(stream_hold: Duration) -> Options {
    Options {
        stream_hold,
        expiry: Duration::from_secs(60),
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

/// Waits for the question to show on the control channel and returns its id.
async fn question_id(gw: &Running) -> String {
    for _ in 0..250 {
        if let Some(q) = pending(gw).await.first() {
            return q["id"].as_str().unwrap().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no question showed");
}

#[tokio::test]
async fn a_local_answer_streams_with_its_footer_before_the_end() {
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    let response = chat(gw, streaming(ask("bullbar/public/easy"))).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["x-bullbar-route"],
        "local at local / small"
    );
    let streamed = read_stream(response).await;
    assert!(streamed.done);
    assert_eq!(
        streamed.text,
        "42\n\n[Bullbar] answered by local at local / small"
    );
    assert!(
        streamed
            .chunks
            .iter()
            .all(|c| c["model"] == "bullbar/public/easy")
    );
    // The chunk that ends the answer comes last, after the footer.
    let last = streamed.chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    let sent = &received.all()[0].1;
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["model"], "fake-small");
}

#[tokio::test]
async fn bullbar_s_own_reply_streams_marked() {
    let (runner, received) = fake_runner().await;
    // Hard public work goes to the cloud account, which has no address here.
    let gw = gateway(config(runner), Options::default()).await;
    let streamed = read_stream(chat(gw, streaming(ask("bullbar/public/hard"))).await).await;
    assert!(streamed.done);
    assert!(
        streamed
            .text
            .starts_with("[Bullbar] this request would go to"),
        "{}",
        streamed.text
    );
    assert_eq!(
        streamed.chunks.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
    assert!(received.all().is_empty());
}

#[tokio::test]
async fn an_answer_that_stops_early_is_marked_cut_off() {
    let (runner, _) = fake_runner().await;
    let gw = with_control(config(runner), Options::default()).await;
    let body = streaming(saying("bullbar/public/easy", "please cut off"));
    let streamed = read_stream(chat(gw.chat, body).await).await;
    assert!(streamed.done);
    assert!(
        streamed
            .text
            .starts_with("42\n\n[Bullbar] the answer was cut off: the local runner"),
        "{}",
        streamed.text
    );
    assert!(streamed.text.contains("Nothing was sent anywhere else"));
    let log = gw.decisions.all();
    assert!(
        log.iter().any(|l| l.starts_with("decision failed")),
        "{log:?}"
    );
}

#[tokio::test]
async fn an_error_mid_stream_is_not_passed_on() {
    let (runner, provider_received) = fake_provider().await;
    let (local, _) = fake_runner().await;
    let key = key_file("stream-error");
    let config = cloud_config(
        local,
        runner,
        &format!("{:?}", key.display().to_string()),
        CLOUD_SLOTS,
        None,
    );
    let gw = gateway(config, Options::default()).await;
    let body = streaming(saying("bullbar/public/hard", "please fail"));
    let streamed = read_stream(chat(gw, body).await).await;
    assert_eq!(provider_received.all().len(), 1);
    assert!(
        streamed.text.contains("the answer was cut off: provider-a"),
        "{}",
        streamed.text
    );
    // The provider's message quoted the request; it isn't relayed.
    assert!(
        !streamed.text.contains("upstream quotes"),
        "{}",
        streamed.text
    );
    assert!(streamed.chunks.iter().all(|c| c.get("error").is_none()));
}

#[tokio::test]
async fn placeholders_stream_as_sent_with_the_note() {
    let (local, _) = fake_runner().await;
    let (provider, received) = fake_provider().await;
    let key = key_file("stream-strip");
    let config = cloud_config(
        local,
        provider,
        &format!("{:?}", key.display().to_string()),
        CLOUD_SLOTS,
        Some("strip_and_send = true"),
    );
    let gw = gateway(config, Options::default()).await;
    let body = streaming(saying(
        "bullbar/public/hard",
        "why is AKIAFAKEFAKEFAKEFAKE rejected?",
    ));
    let streamed = read_stream(chat(gw, body).await).await;
    assert!(streamed.done);
    // Sent a few characters per chunk, so the placeholder is split across chunks; it comes back
    // whole and unrestored.
    assert!(
        streamed.text.starts_with(
            "you said: why is [CREDENTIAL_1] rejected?\n\n[Bullbar] answered by provider-a"
        ),
        "{}",
        streamed.text
    );
    assert!(
        streamed.text.contains("[CREDENTIAL_1] stands for"),
        "{}",
        streamed.text
    );
    assert!(!streamed.text.contains("AKIAFAKE"));
    assert!(!received.all()[0].1.to_string().contains("AKIAFAKE"));
}

#[tokio::test]
async fn a_question_shows_at_once_and_the_answer_follows_on_the_same_stream() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(config(runner), options(Duration::from_secs(10))).await;
    let chat_at = gw.chat;
    let held = tokio::spawn(async move {
        chat(
            chat_at,
            streaming(saying(ASK, "help me plan a difficult conversation")),
        )
        .await
    });
    let id = question_id(&gw).await;
    assert!(received.all().is_empty(), "sent before the answer");
    answer(&gw, &id, true).await;
    let response = held.await.unwrap();
    assert_eq!(response.headers()["x-bullbar-route"], "ask");
    assert_eq!(response.headers()["x-bullbar-question"], id.as_str());
    let streamed = read_stream(response).await;
    assert!(streamed.done);
    let (question, rest) = streamed.text.split_once(ANSWER_SEPARATOR).unwrap();
    assert!(question.starts_with("[Bullbar] "), "{question}");
    assert!(
        question.contains(&format!("bullbar answer {id} yes")),
        "{question}"
    );
    assert!(
        rest.starts_with("42\n\n[Bullbar] answered by local at local / mid (you settled"),
        "{rest}"
    );
    // The next turn sends the model its own answer only, not the question.
    let next = json!({ "model": "bullbar/public/easy", "messages": [
        { "role": "user", "content": "help me plan a difficult conversation" },
        { "role": "assistant", "content": streamed.text },
        { "role": "user", "content": "thanks" },
    ]});
    chat(gw.chat, next).await;
    let sent = &received.all()[1].1;
    assert_eq!(sent["messages"][1]["content"], "42");
}

#[tokio::test]
async fn unanswered_on_the_stream_says_how_to_continue() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(config(runner), options(Duration::from_millis(200))).await;
    let response = chat(
        gw.chat,
        streaming(saying(ASK, "help me plan a difficult conversation")),
    )
    .await;
    let id = response.headers()["x-bullbar-question"]
        .to_str()
        .unwrap()
        .to_owned();
    let streamed = read_stream(response).await;
    assert!(streamed.done);
    assert!(streamed.text.starts_with("[Bullbar] "), "{}", streamed.text);
    assert!(
        streamed
            .text
            .contains("[Bullbar] no answer yet. Answer in Bullbar"),
        "{}",
        streamed.text
    );
    assert!(streamed.text.contains(&format!("bullbar answer {id} no")));
    assert!(!streamed.text.contains(ANSWER_SEPARATOR));
    assert!(received.all().is_empty());
    let log = gw.decisions.all();
    assert!(
        log.iter().any(|l| l.starts_with("decision asked")),
        "{log:?}"
    );
}

#[tokio::test]
async fn a_client_that_leaves_while_its_question_waits_sends_nothing() {
    let (runner, received) = fake_runner().await;
    let gw = with_control(config(runner), options(Duration::from_secs(10))).await;
    let mut response = chat(
        gw.chat,
        streaming(saying(ASK, "help me plan a difficult conversation")),
    )
    .await;
    // The question arrives; then the client goes away.
    assert!(response.chunk().await.unwrap().is_some());
    drop(response);
    let id = question_id(&gw).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    answer(&gw, &id, true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(received.all().is_empty(), "sent with nobody to answer");
}
