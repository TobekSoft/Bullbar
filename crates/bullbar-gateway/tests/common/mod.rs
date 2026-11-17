//! A fake local runner on loopback, and a gateway in front of it. Nothing here reaches a real
//! provider (no test calls any AI provider).

#![allow(dead_code)] // each test binary uses a different part

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bullbar_core::config::{Config, ConfigText};
use bullbar_gateway::server::{Options, Wiring, router};
use serde_json::{Value, json};

/// What the fake runner received: each request's headers and body.
#[derive(Clone, Default)]
pub struct Received(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

impl Received {
    pub fn all(&self) -> Vec<(HeaderMap, Value)> {
        self.0.lock().unwrap().clone()
    }
}

/// The last message's text in a chat request.
fn last_message(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// A streamed answer, a few characters per chunk. A last message asking to "cut off" ends the
/// stream early without saying it is finished; one asking to "fail" sends an error object quoting
/// the request mid-stream.
fn streamed(model: &Value, text: &str, last: &str, reasoning: Option<&str>) -> Response {
    let chunk = |delta: Value, finish: Value| {
        let chunk = json!({
            "id": "fake-stream-1",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        format!("data: {chunk}\n\n")
    };
    let mut events = chunk(json!({ "role": "assistant", "content": "" }), Value::Null);
    if let Some(reasoning) = reasoning {
        events.push_str(&chunk(json!({ "reasoning": reasoning }), Value::Null));
    }
    let chars: Vec<char> = text.chars().collect();
    for (n, piece) in chars.chunks(3).enumerate() {
        let piece: String = piece.iter().collect();
        events.push_str(&chunk(json!({ "content": piece }), Value::Null));
        if n == 0 && last.contains("cut off") {
            return sse(events);
        }
        if n == 0 && last.contains("fail") {
            let error = json!({ "error": { "message": format!("upstream quotes: {last}") } });
            events.push_str(&format!("data: {error}\n\n"));
            return sse(events);
        }
    }
    events.push_str(&chunk(json!({}), json!("stop")));
    events.push_str("data: [DONE]\n\n");
    sse(events)
}

fn sse(events: String) -> Response {
    ([("content-type", "text/event-stream")], events).into_response()
}

fn wants_stream(body: &Value) -> bool {
    body["stream"] == json!(true)
}

/// Starts a fake OpenAI-format runner that answers "42" and records what it gets. It streams when
/// asked to.
pub async fn fake_runner() -> (SocketAddr, Received) {
    async fn answer(
        State(received): State<Received>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let model = body["model"].clone();
        let (stream, last) = (wants_stream(&body), last_message(&body));
        received.0.lock().unwrap().push((headers, body));
        if stream {
            return streamed(&model, "42", &last, None);
        }
        Json(json!({
            "id": "fake-1",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "42" },
                "finish_reason": "stop",
            }],
        }))
        .into_response()
    }
    let received = Received::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(answer))
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, received)
}

/// Starts a fake OpenAI-format provider that answers "you said: <last message>", with reasoning
/// beside it as reasoning models send, and records what it gets. Echoing shows what a provider would see, and whether placeholders come back as sent.
/// It streams when asked to.
pub async fn fake_provider() -> (SocketAddr, Received) {
    async fn answer(
        State(received): State<Received>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let last = last_message(&body);
        let model = body["model"].clone();
        let stream = wants_stream(&body);
        // Reasoning models send their reasoning beside the answer, in the provider's words.
        let reasoning = format!("thinking about: {last}");
        received.0.lock().unwrap().push((headers, body));
        if stream {
            return streamed(
                &model,
                &format!("you said: {last}"),
                &last,
                Some(&reasoning),
            );
        }
        Json(json!({
            "id": "fake-cloud-1",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": format!("you said: {last}"),
                    "reasoning": reasoning,
                },
                "finish_reason": "stop",
            }],
        }))
        .into_response()
    }
    let received = Received::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(answer))
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, received)
}

/// Starts a fake provider that refuses every request with this status and an error naming what it
/// was sent, as real providers' error messages may.
pub async fn refusing_provider(status: u16) -> SocketAddr {
    async fn refuse(State(status): State<u16>, Json(body): Json<Value>) -> Response {
        let error = json!({ "error": {
            "message": format!("cannot answer: {}", last_message(&body)),
            "type": "invalid_request_error",
            "param": "model",
            "code": "model_not_found",
        }});
        let status = axum::http::StatusCode::from_u16(status).unwrap();
        (status, Json(error)).into_response()
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(refuse))
        .with_state(status);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// The obviously fake key the cloud tests use.
pub const FAKE_PROVIDER_KEY: &str = "fake-provider-key-0000-must-not-leak";

/// Writes the fake key to a file of its own and returns the path.
pub fn key_file(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bullbar-test-{}-{name}.key", std::process::id()));
    std::fs::write(&path, format!("{FAKE_PROVIDER_KEY}\n")).unwrap();
    path
}

/// A setup with a local runner (small, mid) and a cloud account `provider-a` (small, mid, top) at
/// `provider`, with `key_file` (a TOML value, e.g. a quoted path). `slots` and `user` as given.
pub fn cloud_config(
    runner: SocketAddr,
    provider: SocketAddr,
    key_file: &str,
    slots: &str,
    user: Option<&str>,
) -> Config {
    let providers = format!(
        r#"
[accounts.local]
row = "local"
offers = {{ small = 0, mid = 0 }}
base_url = "http://{runner}/v1"
models = {{ small = "fake-small", mid = "fake-mid" }}

[accounts.provider-a]
row = "provider-no-training"
offers = {{ small = 2, mid = 5, top = 20 }}
review_due = 2099-01-01
base_url = "http://{provider}/v1"
models = {{ small = "cloud-small", mid = "cloud-mid", top = "cloud-top" }}
key_file = {key_file}
"#
    );
    Config::load(ConfigText {
        providers: &providers,
        policy: "",
        user,
        slots,
        ledger: None,
    })
    .unwrap()
}

/// The slots every cloud test needs: the required five, a local runner and provider adapters, and
/// a terms register so the account's row counts as written.
pub const CLOUD_SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters",
    "policy", "credential-scanner", "local-runner", "provider-adapters", "terms-register"]"#;

/// A setup with a local runner at `runner` (small and mid) and a cloud account with no address.
pub fn config(runner: SocketAddr) -> Config {
    config_with(runner, None)
}

/// The same setup with a `user.toml`.
pub fn config_with(runner: SocketAddr, user: Option<&str>) -> Config {
    let providers = format!(
        r#"
[accounts.local]
row = "local"
offers = {{ small = 0, mid = 0 }}
base_url = "http://{runner}/v1"
models = {{ small = "fake-small", mid = "fake-mid" }}

[accounts.provider-a]
row = "provider-no-training"
offers = {{ small = 2, mid = 5, top = 20 }}
review_due = 2099-01-01
"#
    );
    Config::load(ConfigText {
        providers: &providers,
        policy: "",
        user,
        slots: r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
            "credential-scanner", "local-runner", "provider-adapters"]"#,
        ledger: None,
    })
    .unwrap()
}

/// Starts a gateway for `config` and returns its chat address.
pub async fn gateway(config: Config, options: Options) -> SocketAddr {
    with_control(config, options).await.chat
}

/// A running gateway: its chat and control addresses, its state folder, and what it logged.
pub struct Running {
    pub chat: SocketAddr,
    pub control: SocketAddr,
    pub state: std::path::PathBuf,
    /// Decision log lines (logging is on in these tests, to check what it holds).
    pub decisions: Lines,
    /// Shutter events.
    pub events: Lines,
}

/// Collected log lines.
#[derive(Clone, Default)]
pub struct Lines(Arc<Mutex<Vec<String>>>);

impl Lines {
    pub fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub fn sink(&self) -> bullbar_gateway::activity::LogSink {
        let lines = self.0.clone();
        Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_owned()))
    }
}

/// The obviously fake chat token the tests' gateways take (written before they start, so a test
/// can send it without reading the file).
pub const TEST_CHAT_TOKEN: &str = "fake-chat-token-for-tests-only";

/// Wiring for a gateway in the given state folder, logging into the returned collectors.
pub fn wiring_in(state: &std::path::Path) -> (Wiring, Lines, Lines) {
    let (decisions, events) = (Lines::default(), Lines::default());
    // The control token first: it makes the folder, readable only by the user.
    let control_token = bullbar_gateway::control::load_or_create_token(state).unwrap();
    let chat_file = state.join("chat.token");
    if !chat_file.exists() {
        std::fs::write(&chat_file, format!("{TEST_CHAT_TOKEN}\n")).unwrap();
    }
    let wiring = Wiring {
        control_token,
        chat_token: bullbar_gateway::token::load_or_create(state).unwrap(),
        state: state.to_owned(),
        decisions: Some(decisions.sink()),
        events: events.sink(),
        scrubber: None,
    };
    (wiring, decisions, events)
}

/// Wiring for tests that only need a gateway to start.
pub fn wiring() -> Wiring {
    wiring_in(&fresh_state()).0
}

/// Starts a gateway with its control channel, in a fresh state folder.
pub async fn with_control(config: Config, options: Options) -> Running {
    with_control_in(config, options, &fresh_state()).await
}

/// Starts a gateway with its control channel, in the given state folder.
pub async fn with_control_in(config: Config, options: Options, state: &std::path::Path) -> Running {
    start(config, options, state, None).await
}

/// A stand-in for the entity model: finds the listed names as people, nothing else. Lets tests
/// have a scrubber at the entities grade without the 1.2 GB model.
pub struct FakeNames(pub Vec<&'static str>);

impl bullbar_gateway::scrub::Detector for FakeNames {
    fn find(
        &self,
        text: &str,
    ) -> Result<Vec<bullbar_gateway::scrub::Found>, bullbar_gateway::scrub::EntityError> {
        Ok(self
            .0
            .iter()
            .flat_map(|name| text.match_indices(name).map(|(at, n)| at..at + n.len()))
            .map(|range| bullbar_gateway::scrub::Found {
                range,
                kind: bullbar_gateway::scrub::PiiKind::Person,
            })
            .collect())
    }
}

/// A scrubber at the entities grade that knows the listed names.
pub fn fake_scrubber(names: &[&'static str]) -> bullbar_gateway::scrub::Scrubber {
    bullbar_gateway::scrub::Scrubber::with_detector(Box::new(FakeNames(names.to_vec()))).unwrap()
}

/// Starts a gateway with a scrubber, in a fresh state folder.
pub async fn with_scrubber(
    config: Config,
    options: Options,
    scrubber: bullbar_gateway::scrub::Scrubber,
) -> Running {
    start(config, options, &fresh_state(), Some(scrubber)).await
}

/// Starts a gateway with a scrubber, in the given state folder.
pub async fn with_scrubber_in(
    config: Config,
    options: Options,
    state: &std::path::Path,
    scrubber: bullbar_gateway::scrub::Scrubber,
) -> Running {
    start(config, options, state, Some(scrubber)).await
}

async fn start(
    config: Config,
    options: Options,
    state: &std::path::Path,
    scrubber: Option<bullbar_gateway::scrub::Scrubber>,
) -> Running {
    let (mut wiring, decisions, events) = wiring_in(state);
    wiring.scrubber = scrubber;
    let started = router(config, options, wiring).await.unwrap();
    let serve = |app: Router| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    };
    Running {
        chat: serve(started.chat).await,
        control: serve(started.control).await,
        state: state.to_owned(),
        decisions,
        events,
    }
}

/// Sends a chat request to the gateway, bypassing any proxy, and returns the response.
pub async fn chat(gateway: SocketAddr, body: Value) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{gateway}/v1/chat/completions"))
        .bearer_auth(TEST_CHAT_TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// A one-message chat asking for `model`.
pub fn ask(model: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": "what is six times seven?" }] })
}

/// A state folder no other test uses.
pub fn fresh_state() -> std::path::PathBuf {
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let state = std::env::temp_dir().join(format!("bullbar-test-state-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    state
}

/// Options that reply to a question at once, for tests that only look at the decision.
pub fn no_hold() -> Options {
    Options {
        hold: std::time::Duration::ZERO,
        ..Options::default()
    }
}

/// A streamed reply read whole: the text of every chunk joined, the chunks, and whether it ended
/// with `[DONE]`.
pub struct Streamed {
    pub text: String,
    pub chunks: Vec<Value>,
    pub done: bool,
}

/// Reads a streamed reply to its end.
pub async fn read_stream(response: reqwest::Response) -> Streamed {
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "text/event-stream"
    );
    let body = response.text().await.unwrap();
    let mut streamed = Streamed {
        text: String::new(),
        chunks: Vec::new(),
        done: false,
    };
    for data in body.lines().filter_map(|l| l.strip_prefix("data: ")) {
        assert!(!streamed.done, "data after [DONE]: {body}");
        if data == "[DONE]" {
            streamed.done = true;
            continue;
        }
        let chunk: Value = serde_json::from_str(data).unwrap();
        if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str() {
            streamed.text.push_str(text);
        }
        streamed.chunks.push(chunk);
    }
    streamed
}

/// The same request, streamed.
pub fn streaming(mut body: Value) -> Value {
    body["stream"] = json!(true);
    body
}
