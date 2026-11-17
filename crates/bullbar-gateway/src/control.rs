//! The control channel (ADR 0016): where the user answers Bullbar's questions. A listener of its
//! own, apart from the chat endpoint, reached with a token of its own. Chat clients can send
//! requests but never answer: a provider's reply can print anything, so an answer typed into the
//! chat can't be trusted to come from the user.
//!
//! The token (`token.rs`) is generated on first start into a file only the user can read, and
//! never printed. The command line reads the same file.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::activity::{Activity, LogSink};
use crate::pending::{Answer, AnswerError, Listed, Pending};
use crate::shutter::Shutter;
use crate::trace::Trace;

use crate::token::{self, ForControl, Kind};
pub use crate::token::{ControlToken, TokenError};

/// The control token's file name in the state folder.
pub const TOKEN_FILE: &str = ForControl::FILE;

/// Reads the control token from the state folder, creating it on first start.
pub fn load_or_create_token(state: &Path) -> Result<ControlToken, TokenError> {
    token::load_or_create(state)
}

/// Reads an existing control token (the command line's side).
pub fn read_token(state: &Path) -> Result<ControlToken, TokenError> {
    token::read(state)
}

/// What the control channel works on.
pub struct Parts {
    /// The open questions.
    pub pending: Arc<Pending>,
    /// The control token.
    pub token: ControlToken,
    /// The kill switch.
    pub shutter: Arc<Shutter>,
    /// The counts.
    pub activity: Arc<Activity>,
    /// Where shutter events are logged.
    pub events: LogSink,
    /// The trace of what left (ADR 0018 amendment).
    pub trace: Arc<Trace>,
}

type Control = Parts;

/// The control channel's routes: `GET /v1/pending`, `POST /v1/pending/{id}/answer`,
/// `GET /v1/status`, `POST /v1/shutter` and `POST /v1/trace`.
pub fn router(parts: Parts) -> Router {
    Router::new()
        .route("/v1/pending", get(list))
        .route("/v1/pending/{id}/answer", post(answer))
        .route("/v1/status", get(status))
        .route("/v1/shutter", post(shutter))
        .route("/v1/trace", post(trace))
        .with_state(Arc::new(parts))
}

fn authorised(control: &Control, headers: &HeaderMap) -> bool {
    control.token.authorises(headers)
}

fn refused(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn list(State(control): State<Arc<Control>>, headers: HeaderMap) -> Response {
    if !authorised(&control, &headers) {
        return refused(
            StatusCode::UNAUTHORIZED,
            "the control token is missing or wrong",
        );
    }
    let items: Vec<Value> = control.pending.list().iter().map(listed_json).collect();
    Json(json!({ "pending": items })).into_response()
}

async fn answer(
    State(control): State<Arc<Control>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if !authorised(&control, &headers) {
        return refused(
            StatusCode::UNAUTHORIZED,
            "the control token is missing or wrong",
        );
    }
    let answer = match body.get("answer").and_then(Value::as_str) {
        Some("yes") => Answer::Yes,
        Some("no") => Answer::No,
        _ => return refused(StatusCode::BAD_REQUEST, "answer `yes` or `no`"),
    };
    match control.pending.answer(&id, answer) {
        Ok(()) => Json(json!({ "answered": id })).into_response(),
        Err(e @ AnswerError::Unknown(_)) => refused(StatusCode::NOT_FOUND, &e.to_string()),
        Err(e @ AnswerError::Settled(_)) => refused(StatusCode::CONFLICT, &e.to_string()),
    }
}

async fn status(State(control): State<Arc<Control>>, headers: HeaderMap) -> Response {
    if !authorised(&control, &headers) {
        return refused(
            StatusCode::UNAUTHORIZED,
            "the control token is missing or wrong",
        );
    }
    let questions = control.pending.list();
    let waiting = questions.iter().filter(|q| q.answer.is_none()).count();
    let c = control.activity.counts();
    Json(json!({
        "shutter": shutter_json(&control.shutter),
        "trace": trace_json(&control.trace),
        "questions": { "waiting": waiting, "settled": questions.len() - waiting },
        "counts": {
            "local": c.local, "left": c.left, "refused": c.refused, "asked": c.asked,
            "blocked": c.blocked, "not_sent": c.not_sent, "failed": c.failed,
            "left_for_want_of_local": c.left_for_want_of_local,
            "refused_for_want_of_local": c.refused_for_want_of_local,
        },
    }))
    .into_response()
}

fn shutter_json(shutter: &Shutter) -> Value {
    match shutter.closed() {
        Some(closed) => json!({ "closed": true, "since": closed.since, "reason": closed.reason }),
        None => json!({ "closed": false }),
    }
}

async fn shutter(
    State(control): State<Arc<Control>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorised(&control, &headers) {
        return refused(
            StatusCode::UNAUTHORIZED,
            "the control token is missing or wrong",
        );
    }
    let changed = match body.get("close").and_then(Value::as_bool) {
        Some(true) => {
            let reason = body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("closed by hand");
            control.shutter.close(reason).map(|closed| {
                (control.events)(&format!("shutter closed: {}", closed.reason));
            })
        }
        Some(false) => control.shutter.open().map(|()| {
            (control.events)("shutter reopened");
        }),
        None => return refused(StatusCode::BAD_REQUEST, "send `close`: true or false"),
    };
    match changed {
        Ok(()) => Json(shutter_json(&control.shutter)).into_response(),
        // Closing still holds in memory if the file couldn't be written; say so.
        Err(e) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("{e}; the shutter is {}", state_word(&control.shutter)),
        ),
    }
}

/// Starts (`{"requests": n}`), stops (`{"off": true}`) or clears (`{"clear": true}`) the trace.
/// Starting and stopping are logged as events: they hold no content.
async fn trace(
    State(control): State<Arc<Control>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorised(&control, &headers) {
        return refused(
            StatusCode::UNAUTHORIZED,
            "the control token is missing or wrong",
        );
    }
    let done = if let Some(n) = body.get("requests").and_then(Value::as_u64) {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        control.trace.start(n).map(|()| {
            (control.events)(&format!("trace on for {n} requests"));
        })
    } else if body.get("off").and_then(Value::as_bool) == Some(true) {
        control.trace.stop();
        (control.events)("trace off");
        Ok(())
    } else if body.get("clear").and_then(Value::as_bool) == Some(true) {
        control
            .trace
            .clear()
            .map(|()| (control.events)("trace off, file deleted"))
    } else {
        return refused(
            StatusCode::BAD_REQUEST,
            "send `requests`: n, `off`: true or `clear`: true",
        );
    };
    match done {
        Ok(()) => Json(trace_json(&control.trace)).into_response(),
        Err(e @ crate::trace::TraceError::Forbidden) => {
            refused(StatusCode::FORBIDDEN, &e.to_string())
        }
        Err(e) => refused(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn trace_json(trace: &Trace) -> Value {
    match trace.running() {
        Some(r) => json!({ "on": true, "left": r.left, "ends_in_seconds": r.ends_in.as_secs() }),
        None => json!({ "on": false }),
    }
}

fn state_word(shutter: &Shutter) -> &'static str {
    if shutter.closed().is_some() {
        "closed"
    } else {
        "open"
    }
}

fn answer_name(answer: Option<Answer>) -> &'static str {
    match answer {
        None => "waiting",
        Some(Answer::Yes) => "yes",
        Some(Answer::No) => "no",
        Some(Answer::Expired) => "no answer (counts as no)",
    }
}

fn listed_json(item: &Listed) -> Value {
    json!({
        "id": item.id,
        "model": item.model,
        "question": item.question,
        "why": item.why,
        "waiting_seconds": item.waiting.as_secs(),
        "answer": answer_name(item.answer),
    })
}

/// The command line's side: talks to a running gateway's control channel.
pub mod client {
    use super::*;

    /// What the control channel answers with, for callers that read it.
    pub use serde_json::Value;

    /// Why a control command failed.
    #[derive(Debug, thiserror::Error)]
    pub enum ClientError {
        /// Couldn't reach the control channel.
        #[error("can't reach Bullbar's control channel at {0}; is `bullbar serve` running?")]
        Unreachable(SocketAddr),
        /// It answered with an error.
        #[error("{0}")]
        Refused(String),
        /// No async runtime could be built.
        #[error("starting the client: {0}")]
        Runtime(std::io::Error),
    }

    fn block_on<T>(f: impl Future<Output = Result<T, ClientError>>) -> Result<T, ClientError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ClientError::Runtime)?
            .block_on(f)
    }

    fn http(at: SocketAddr) -> Result<reqwest::Client, ClientError> {
        // Never through a proxy: the token goes only to this machine's control channel.
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ClientError::Unreachable(at))
    }

    async fn send(
        at: SocketAddr,
        request: reqwest::RequestBuilder,
        token: &ControlToken,
    ) -> Result<Value, ClientError> {
        let response = request
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|_| ClientError::Unreachable(at))?;
        let ok = response.status().is_success();
        let body: Value = response
            .json()
            .await
            .map_err(|_| ClientError::Unreachable(at))?;
        if ok {
            Ok(body)
        } else {
            let message = body["error"].as_str().unwrap_or("refused").to_owned();
            Err(ClientError::Refused(message))
        }
    }

    /// The open questions, as the control channel lists them.
    pub fn pending(at: SocketAddr, token: &ControlToken) -> Result<Vec<Value>, ClientError> {
        block_on(async {
            let request = http(at)?.get(format!("http://{at}/v1/pending"));
            let body = send(at, request, token).await?;
            Ok(body["pending"].as_array().cloned().unwrap_or_default())
        })
    }

    /// The gateway's status: the shutter, the questions, the counts.
    pub fn status(at: SocketAddr, token: &ControlToken) -> Result<Value, ClientError> {
        block_on(async {
            let request = http(at)?.get(format!("http://{at}/v1/status"));
            send(at, request, token).await
        })
    }

    /// Closes (with a reason) or reopens the shutter; returns its new state.
    pub fn shutter(
        at: SocketAddr,
        token: &ControlToken,
        close: bool,
        reason: Option<&str>,
    ) -> Result<Value, ClientError> {
        block_on(async {
            let mut body = json!({ "close": close });
            if let Some(reason) = reason {
                body["reason"] = json!(reason);
            }
            let request = http(at)?
                .post(format!("http://{at}/v1/shutter"))
                .json(&body);
            send(at, request, token).await
        })
    }

    /// Starts a trace of the next `n` requests that leave, or with `None` stops it; `clear` also
    /// deletes its file. Returns its new state.
    pub fn trace(
        at: SocketAddr,
        token: &ControlToken,
        requests: Option<u32>,
        clear: bool,
    ) -> Result<Value, ClientError> {
        block_on(async {
            let body = match (requests, clear) {
                (_, true) => json!({ "clear": true }),
                (Some(n), false) => json!({ "requests": n }),
                (None, false) => json!({ "off": true }),
            };
            let request = http(at)?.post(format!("http://{at}/v1/trace")).json(&body);
            send(at, request, token).await
        })
    }

    /// Answers a question.
    pub fn answer(
        at: SocketAddr,
        token: &ControlToken,
        id: &str,
        yes: bool,
    ) -> Result<(), ClientError> {
        block_on(async {
            let request = http(at)?
                .post(format!("http://{at}/v1/pending/{id}/answer"))
                .json(&json!({ "answer": if yes { "yes" } else { "no" } }));
            send(at, request, token).await.map(|_| ())
        })
    }
}
