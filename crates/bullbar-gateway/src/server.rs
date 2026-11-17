//! Bullbar's endpoint (ADR 0017): chat clients talk to Bullbar, the floor scans, the engine
//! decides, and only then is anything forwarded. A route that leaves the local layer is carried out
//! only when every protection it lists is built, and only after the floor passes the exact body;
//! otherwise Bullbar answers itself and nothing is sent. A decision that is a question waits for
//! the user's answer on the control channel (ADR 0016).

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use axum::extract::{Request as HttpRequest, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bullbar_core::config::Config;
use bullbar_core::decision::{Explanation, Mechanism, Question, Route};
use bullbar_core::engine::{Engine, EngineError};
use bullbar_core::levels::Category;
use bullbar_core::levels::{Column, Row, ScrubberGrade};
use bullbar_core::lifecycle::{Lifecycle, State as Stage};
use bullbar_core::provider::{AccountId, Date};
use bullbar_core::slots::{Slot, Slots};
use serde_json::{Value, json};

use crate::activity::{Activity, Ended, LogSink};
use crate::address::{AddressError, check_cloud, check_local};
use crate::control::{self, ControlToken, TokenError};
use crate::fields;
use crate::floor::{Floor, Stripped};
use crate::key;
use crate::pending::{Answer, Pending, new_id};
use crate::pins::{Pins, PinsError};
use crate::reply::{ANSWER_SEPARATOR, FOOTER_LINE_BREAK, FOOTER_SEPARATOR};
use crate::scrub::surrogate::Keyed;
use crate::scrub::{Restorer, ScrubError, Scrubber, scrub_conversation};
use crate::shutter::Shutter;
use crate::token::{self, ChatToken, ForChat, ForScrubbing};
use crate::trace::{Ticket, Trace};
use crate::{model_name, reply, stream};
use futures_util::StreamExt;

/// How the endpoint behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Add a line to each answer naming where it came from (ADR 0016: show the source).
    pub footer: bool,
    /// How long a chat request is held open while its question waits, before Bullbar replies with
    /// a notice saying where to answer.
    pub hold: Duration,
    /// The same for a streamed reply: the question shows at once and keep-alive lines hold the
    /// connection, so it can wait longer than a silent request.
    pub stream_hold: Duration,
    /// How long a question stays open. Unanswered by then, it counts as no.
    pub expiry: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            footer: true,
            hold: Duration::from_secs(20),
            stream_hold: Duration::from_secs(120),
            expiry: Duration::from_secs(15 * 60),
        }
    }
}

/// Where `serve` listens and keeps its state.
#[derive(Debug, Clone)]
pub struct Listen {
    /// The chat endpoint.
    pub chat: SocketAddr,
    /// The control channel.
    pub control: SocketAddr,
    /// The state folder: the control token lives here.
    pub state: PathBuf,
}

/// What a gateway is wired to besides its config: the two tokens, the state folder (the shutter's
/// file), and where log lines go.
pub struct Wiring {
    /// The control channel's token.
    pub control_token: ControlToken,
    /// The chat endpoint's token: every chat request must carry it.
    pub chat_token: ChatToken,
    /// The state folder.
    pub state: PathBuf,
    /// Where decisions are logged, without content; `None` (the default) logs nothing.
    pub decisions: Option<LogSink>,
    /// Where shutter events are logged (what, when, why, never content). Always on.
    pub events: LogSink,
    /// The PII scrubber, if one was built. Its grade, not the configured one, caps what the
    /// engine may rely on (ADR 0020, point 6); none counts as patterns only.
    pub scrubber: Option<Scrubber>,
}

/// Why the gateway won't start.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// The engine won't start (a required slot is missing).
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A local account's address isn't local.
    #[error(transparent)]
    Address(#[from] AddressError),
    /// The floor's rules didn't compile: a bug, and Bullbar won't run without its floor.
    #[error("the always-on floor couldn't be built: {0}")]
    Floor(#[from] regex::Error),
    /// The HTTP client couldn't be built.
    #[error("building the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
    /// The control token couldn't be made or read.
    #[error("the control token: {0}")]
    Token(#[from] TokenError),
    /// The topic pins in the state folder can't be read.
    #[error(transparent)]
    Pins(#[from] PinsError),
    /// The scrubbing secret in the state folder isn't one Bullbar made.
    #[error("{0} doesn't hold a scrubbing secret; delete it and restart `bullbar serve`")]
    Secret(String),
    /// Binding the listening address, or serving, failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The protections this version can carry out. A route listing any other stays unsent until it is
/// built: sending without a protection the engine asked for would be a silent bypass (rule 4).
const CARRIED_OUT: &[Mechanism] = &[
    Mechanism::StripCredentials,
    Mechanism::Scrub,
    Mechanism::Restore,
    Mechanism::TopicPinning,
];

/// The slots this version doesn't build, and what stands in for each. The engine believes
/// `slots.toml`, so a declared one is named at start (honest about the setup, ADR 0014). Not
/// capped to empty, as the scrubber's grade is: an empty terms register would demote every account
/// to standard terms, and the rows in the accounts file are the user's own judgement (2026-11-17).
const NOT_BUILT: &[(Slot, &str)] = &[
    (
        Slot::Classifier,
        "ratings come from the model name; unrated work counts as Sensitive",
    ),
    (
        Slot::TermsRegister,
        "each account's row is as set by hand in the accounts file",
    ),
    (
        Slot::ExposureLedger,
        "nothing is counted; a ledger file, if any, is read as written, and routes that need the ledger aren't sent",
    ),
    (
        Slot::Obfuscator,
        "routes that need personas, decoy content or chaff aren't sent",
    ),
    (
        Slot::Deobfuscator,
        "replies are restored, not de-obfuscated",
    ),
    (Slot::Canaries, "no canaries are planted"),
    (
        Slot::AnswerCheck,
        "answers aren't judged, so nothing escalates by itself",
    ),
    (
        Slot::Interfaces,
        "there is the chat endpoint and the command line only",
    ),
];

/// How long to wait for a model before giving up (fail closed: nothing goes elsewhere).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// An account Bullbar can forward to.
struct Upstream {
    client: Client<OpenAIConfig>,
    models: BTreeMap<Column, String>,
}

/// A gateway ready to serve, and what it left out on the way.
pub struct Started {
    /// The chat endpoint.
    pub chat: Router,
    /// The control channel, to be served on a listener of its own.
    pub control: Router,
    /// Accounts left out, and why: shown when `serve` starts, so nothing is dropped silently.
    pub notices: Vec<String>,
}

struct Gateway {
    engine: Engine,
    floor: Arc<Floor>,
    scrubber: Option<Arc<Scrubber>>,
    keyed: Keyed,
    pins: Pins,
    upstreams: BTreeMap<AccountId, Upstream>,
    options: Options,
    pending: Arc<Pending>,
    shutter: Arc<Shutter>,
    activity: Arc<Activity>,
    trace: Arc<Trace>,
}

impl Gateway {
    /// A ticket in the trace for a request about to leave, if a trace is on (ADR 0018 amendment).
    /// Local answers never left, so they are never traced.
    fn traced(&self, route: &Route, body: &Value) -> Option<Ticket> {
        route
            .leaves_local()
            .then(|| self.trace.take(&route.to_string(), body))
            .flatten()
    }

    /// A reply Bullbar writes itself, noted and with the decision in the headers.
    fn own(
        &self,
        ended: Ended,
        model: &str,
        text: &str,
        route: &str,
        explanation: &Explanation,
    ) -> Response {
        self.activity.note(ended, route, explanation);
        own(model, text, route, explanation)
    }
}

/// The endpoint and control channel for this setup.
///
/// Local accounts must reach only local addresses, and cloud accounts must use https off this
/// machine, or the gateway won't start. A local account without an address is left out. A cloud
/// account whose key file can't be read is left out and named in the notices: never a request
/// without its key. Cloud accounts without an address stay in the engine, so decisions say where a
/// request would go, and Bullbar answers that it wasn't sent.
pub async fn router(
    config: Config,
    options: Options,
    wiring: Wiring,
) -> Result<Started, GatewayError> {
    let Config {
        policy,
        accounts,
        slots,
        ledger,
    } = config;
    let mut upstreams = BTreeMap::new();
    let mut reachable = Vec::new();
    let mut notices = Vec::new();
    for account in accounts {
        let Some(address) = account.address.clone() else {
            if account.row != Row::Local {
                reachable.push(account);
            }
            continue;
        };
        let http = if account.row == Row::Local {
            check_local(&account.id, &address.base_url).await?;
            // A proxy from the environment would carry local content off the machine.
            reqwest::Client::builder().no_proxy()
        } else {
            check_cloud(&account.id, &address.base_url)?;
            // A proxy from the environment is honoured: it sees only the encrypted connection.
            reqwest::Client::builder()
        };
        let http = http
            .connect_timeout(CONNECT_TIMEOUT)
            // Between reads, not in all: a streamed answer may take longer than this overall.
            .read_timeout(ANSWER_TIMEOUT)
            .build()?;
        let key = match &address.key_file {
            None => None,
            Some(path) => match key::read(path) {
                Ok(key) => Some(key),
                Err(e) => {
                    notices.push(format!("account `{}` left out: {e}", account.id));
                    continue;
                }
            },
        };
        let key = key.as_ref().map_or("", key::ApiKey::expose);
        upstreams.insert(
            account.id.clone(),
            Upstream {
                client: Client::with_config(isolated(&address.base_url, key))
                    .with_http_client(http),
                models: address.models,
            },
        );
        reachable.push(account);
    }
    let slots = scrubber_grade(slots, wiring.scrubber.as_ref(), &mut notices);
    not_built(&slots, &mut notices);
    let trace = Arc::new(Trace::new(&wiring.state, policy.admin().trace_allowed));
    let engine = Engine::new(policy, reachable, slots)?.with_ledger(ledger);
    let keyed = scrubbing_keys(&wiring.state)?;
    let pending = Arc::new(Pending::new(options.expiry));
    let shutter = Arc::new(Shutter::load(&wiring.state));
    if let Some(closed) = shutter.closed() {
        notices.push(format!(
            "the shutter is closed (since {}, {}): nothing leaves until `bullbar reopen`",
            crate::shutter::utc(closed.since),
            closed.reason
        ));
    }
    let activity = Arc::new(Activity::new(wiring.decisions));
    let gateway = Arc::new(Gateway {
        engine,
        floor: Arc::new(Floor::new()?),
        scrubber: wiring.scrubber.map(Arc::new),
        keyed,
        pins: Pins::load(&wiring.state)?,
        upstreams,
        options,
        pending: pending.clone(),
        shutter: shutter.clone(),
        activity: activity.clone(),
        trace: trace.clone(),
    });
    let chat = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/models", get(models))
        .route_layer(middleware::from_fn_with_state(
            Arc::new(wiring.chat_token),
            require_chat_token,
        ))
        .with_state(gateway);
    Ok(Started {
        chat,
        control: control::router(control::Parts {
            pending,
            token: wiring.control_token,
            shutter,
            activity,
            events: wiring.events,
            trace,
        }),
        notices,
    })
}

/// The keys for surrogates, from the state folder's scrubbing secret, made on first use. `serve`
/// and `bullbar scrub` share it, so `scrub` shows the surrogates `serve` would send.
pub fn scrubbing_keys(state: &std::path::Path) -> Result<Keyed, GatewayError> {
    let secret = token::load_or_create::<ForScrubbing>(state)?;
    Keyed::from_secret(secret.expose()).ok_or_else(|| {
        GatewayError::Secret(token::path::<ForScrubbing>(state).display().to_string())
    })
}

/// The slots with the scrubber's grade capped at what was built (ADR 0020, point 6), and a notice
/// when that is lower than configured.
fn scrubber_grade(slots: Slots, scrubber: Option<&Scrubber>, notices: &mut Vec<String>) -> Slots {
    let Some(configured) = slots.scrubber_grade() else {
        return slots;
    };
    let built = scrubber.map_or(ScrubberGrade::Patterns, Scrubber::grade);
    if built >= configured {
        return slots;
    }
    let effect = if built < ScrubberGrade::Entities {
        "it counts as empty, so personal work stays local"
    } else {
        "work that needs more stays local"
    };
    notices.push(format!(
        "the PII scrubber is configured for {configured} but built for {built}: {effect}{}",
        if scrubber.is_none() {
            " (start with --pii-model <dir> to load the entity model)"
        } else {
            ""
        }
    ));
    slots.with_scrubber_grade(built)
}

/// A notice for each slot `slots.toml` declares that this version doesn't build.
fn not_built(slots: &Slots, notices: &mut Vec<String>) {
    for (slot, instead) in NOT_BUILT {
        if slots.usable(*slot) {
            let are = if matches!(slot, Slot::Canaries | Slot::Interfaces) {
                "are"
            } else {
                "is"
            };
            notices.push(format!(
                "the {slot} {are} declared in slots.toml but not built: {instead}"
            ));
        }
    }
}

/// Log lines to standard error, marked as Bullbar's, with the time.
fn stderr() -> LogSink {
    Arc::new(|line: &str| eprintln!("bullbar: {} {line}", crate::shutter::utc(now())))
}

/// Serves until interrupted. Blocks; builds its own runtime. `log_decisions` turns the decision log
/// on (off by default: nothing about requests is written anywhere).
pub fn serve(
    config: Config,
    listen: Listen,
    options: Options,
    log_decisions: bool,
    scrubber: Option<Scrubber>,
) -> Result<(), GatewayError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let wiring = Wiring {
            control_token: control::load_or_create_token(&listen.state)?,
            chat_token: token::load_or_create(&listen.state)?,
            state: listen.state.clone(),
            decisions: log_decisions.then(stderr),
            events: stderr(),
            scrubber,
        };
        let started = router(config, options, wiring).await?;
        for notice in &started.notices {
            eprintln!("bullbar: {notice}");
        }
        let chat = tokio::net::TcpListener::bind(listen.chat).await?;
        let control_listener = tokio::net::TcpListener::bind(listen.control).await?;
        eprintln!(
            "bullbar: chat clients use the API key in {}",
            token::path::<ForChat>(&listen.state).display()
        );
        eprintln!(
            "bullbar: control channel on http://{}, token in {}",
            listen.control,
            listen.state.join(control::TOKEN_FILE).display()
        );
        tokio::select! {
            served = axum::serve(chat, started.chat) => served?,
            served = axum::serve(control_listener, started.control) => served?,
            // If the signal can't be watched, run until the process is stopped.
            signal = tokio::signal::ctrl_c() => if signal.is_err() {
                std::future::pending::<()>().await;
            },
        }
        Ok(())
    })
}

/// A client config that takes nothing from the environment. The library's default reads an API
/// key, organisation and project from environment variables and sends them to whatever address it
/// is given: a key meant for one provider would reach another, or the local runner.
fn isolated(base_url: &str, key: &str) -> OpenAIConfig {
    OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(key)
        .with_org_id("")
        .with_project_id("")
}

/// Every chat route needs the chat token, checked before the request is read. Anything on this
/// machine can reach a loopback port, a web page included (DNS rebinding), and anything on the
/// network once Bullbar listens beyond loopback (ADR 0016). The token is never forwarded: requests
/// to models are built afresh.
async fn require_chat_token(
    State(token): State<Arc<ChatToken>>,
    request: HttpRequest,
    next: Next,
) -> Response {
    if token.authorises(request.headers()) {
        next.run(request).await
    } else {
        error(
            StatusCode::UNAUTHORIZED,
            "the chat token is missing or wrong. Set it as the API key in your chat client; \
             `bullbar serve` names its file when it starts",
        )
    }
}

async fn models() -> Json<Value> {
    let data: Vec<Value> = model_name::listed()
        .into_iter()
        .map(|id| json!({ "id": id, "object": "model", "created": 0, "owned_by": "bullbar" }))
        .collect();
    Json(json!({ "object": "list", "data": data }))
}

/// Which conversation a request belongs to: its model name and messages, once Bullbar's own words
/// are dropped. A client resending after a notice sends the same, so its question is found again.
/// Only this hash is kept while a question waits, never the conversation.
fn conversation_key(model: &str, body: &Value) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    model.hash(&mut hasher);
    body.get("messages")
        .map(Value::to_string)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Where a request ends up once decided, before anything is written to the client.
enum Outcome {
    /// Bullbar answers itself; nothing is sent.
    Own {
        ended: Ended,
        text: String,
        route: String,
        explanation: Explanation,
    },
    /// A question still waits when the hold runs out.
    Ask {
        id: String,
        question: Question,
        explanation: Explanation,
        lifecycle: Box<Lifecycle>,
        note: Option<String>,
    },
    /// Forward to a model. A body that leaves has passed the floor.
    Forward {
        route: Route,
        body: Value,
        stripped: Stripped,
        restorer: Arc<Restorer>,
        explanation: Explanation,
        note: Option<String>,
    },
    /// Something went wrong; nothing is sent (rule 4).
    Error(StatusCode, String),
}

/// One chat request, ready to decide: the model name it asked for, its conversation, its body with
/// Bullbar's own words dropped.
struct Request {
    model: String,
    key: u64,
    body: Value,
}

async fn chat(State(gateway): State<Arc<Gateway>>, Json(mut body): Json<Value>) -> Response {
    let Some(model) = body.get("model").and_then(Value::as_str).map(str::to_owned) else {
        return error(StatusCode::BAD_REQUEST, "the request names no model");
    };
    let streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    let mut profile = match model_name::profile(&model, gateway.engine.policy()) {
        Ok(profile) => profile,
        Err(e) => return error(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    // Bullbar's own words go before anything else looks at the request.
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        reply::strip_own(messages);
    }
    profile.credentials_found = gateway.floor.credentials_found(&body);
    profile.shutter_closed = gateway.shutter.closed().is_some();
    profile.pinned_to = gateway.pins.get(&profile.category);
    let Some(today) = today() else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the system date is out of range",
        );
    };
    let key = conversation_key(&model, &body);
    let started = Instant::now();
    // A question settled for this conversation picks up from its answer, but only while a fresh
    // decision still asks the same question. Otherwise the answer was to a question that no longer
    // arises (the shutter has reopened, an account came back) and the fresh decision stands; the
    // stale answer is dropped either way.
    let fresh = Lifecycle::start(&gateway.engine, profile, today);
    let (lifecycle, note) = match gateway.pending.take_settled(key) {
        Some((_, settled, answer)) if settled.state() == fresh.state() => {
            match settle(&gateway.engine, settled, answer) {
                Ok(settled) => settled,
                Err(outcome) => return respond(&gateway, &model, outcome).await,
            }
        }
        _ => (fresh, None),
    };
    let request = Request { model, key, body };
    if !streaming {
        let deadline = started + gateway.options.hold;
        let outcome = drive(&gateway, lifecycle, note, &request, deadline).await;
        return respond(&gateway, &request.model, outcome).await;
    }
    // Streamed: decide without holding, so the reply starts at once; a question then waits on
    // the open stream.
    let outcome = drive(&gateway, lifecycle, note, &request, started).await;
    stream_reply(gateway, request, outcome, started).await
}

/// Runs the lifecycle until it is sent, refused, or a question is still open at `deadline`.
async fn drive(
    gateway: &Gateway,
    mut lifecycle: Lifecycle,
    mut note: Option<String>,
    request: &Request,
    deadline: Instant,
) -> Outcome {
    let key = request.key;
    loop {
        let explanation = lifecycle.explanation().clone();
        match lifecycle.state().clone() {
            Stage::Sent(route) => {
                let topic = Topic {
                    category: &lifecycle.profile().category,
                    pinned: lifecycle.profile().pinned_to.as_ref(),
                };
                return carry_out(
                    gateway,
                    &route,
                    topic,
                    request.body.clone(),
                    explanation,
                    note,
                )
                .await;
            }
            Stage::AwaitingUser(question) => {
                let waiting = match gateway.pending.waiting_for(key) {
                    Some(waiting) => waiting,
                    None => match new_id() {
                        Ok(id) => gateway
                            .pending
                            .park(id, key, &request.model, lifecycle.clone()),
                        Err(_) => {
                            return Outcome::Error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "no random source for a question id; nothing was sent".to_owned(),
                            );
                        }
                    },
                };
                // Hold the request open until answered or the hold runs out.
                let notified = waiting.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if gateway.pending.waiting_for(key).is_some() {
                    let left = deadline.saturating_duration_since(Instant::now());
                    let _ = tokio::time::timeout(left, notified).await;
                }
                match gateway.pending.take_settled(key) {
                    Some((_, settled, answer)) => {
                        (lifecycle, note) = match settle(&gateway.engine, settled, answer) {
                            Ok(next) => next,
                            Err(outcome) => return outcome,
                        };
                    }
                    None => {
                        return Outcome::Ask {
                            id: waiting.id,
                            question,
                            explanation,
                            lifecycle: Box::new(lifecycle),
                            note,
                        };
                    }
                }
            }
            Stage::Refused => {
                let why = explanation.to_string();
                let said = note.map(|n| format!(" ({n})")).unwrap_or_default();
                return Outcome::Own {
                    ended: Ended::Refused,
                    text: format!("refused{said}; nothing was sent.\n\nwhy:\n{why}"),
                    route: "refuse".to_owned(),
                    explanation,
                };
            }
            // A new request is never judged poor, done or stopped before it is sent.
            Stage::Poor(_) | Stage::Done(_) | Stage::Stopped => {
                return Outcome::Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the request ended in a state it can't start in; nothing was sent".to_owned(),
                );
            }
        }
    }
}

/// Feeds a settled question's answer into its lifecycle, with a note saying what was answered.
#[allow(clippy::result_large_err)] // the error is the outcome itself, written at once
fn settle(
    engine: &Engine,
    mut lifecycle: Lifecycle,
    answer: Answer,
) -> Result<(Lifecycle, Option<String>), Outcome> {
    let Stage::AwaitingUser(question) = lifecycle.state().clone() else {
        return Err(Outcome::Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "a settled question with no question; nothing was sent".to_owned(),
        ));
    };
    if lifecycle.handle(engine, answer.event(&question)).is_err() {
        return Err(Outcome::Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the answer didn't fit the question; nothing was sent".to_owned(),
        ));
    }
    let note = match (answer, &question) {
        (Answer::Yes, Question::SettleLocal { wanted, .. }) => {
            format!("you settled for this model; the work wanted a {wanted} model")
        }
        (Answer::Yes, _) => "you answered yes".to_owned(),
        (Answer::No, _) => "you answered no".to_owned(),
        (Answer::Expired, _) => "no answer in time counts as no".to_owned(),
    };
    Ok((lifecycle, Some(note)))
}

/// Writes an outcome as an ordinary (not streamed) reply.
async fn respond(gateway: &Gateway, model: &str, outcome: Outcome) -> Response {
    match outcome {
        Outcome::Own {
            ended,
            text,
            route,
            explanation,
        } => gateway.own(ended, model, &text, &route, &explanation),
        Outcome::Ask {
            id,
            question,
            explanation,
            ..
        } => {
            gateway.activity.note(Ended::Asked, "ask", &explanation);
            let text = format!(
                "{question}\n\n{}",
                still_open(&id, &explanation, gateway.options.expiry)
            );
            question_header(own(model, &text, "ask", &explanation), &id)
        }
        Outcome::Forward {
            route,
            body,
            stripped,
            restorer,
            explanation,
            note,
        } => {
            let sent = Sent {
                stripped: &stripped,
                restorer: &restorer,
                explanation: &explanation,
            };
            forward(gateway, &route, model, body, sent, note).await
        }
        Outcome::Error(status, message) => error(status, &message),
    }
}

/// What to do about a question still open: where to answer, then resend.
fn still_open(id: &str, explanation: &Explanation, expiry: Duration) -> String {
    let minutes = expiry.as_secs().div_ceil(60);
    format!(
        "Answer in Bullbar: `bullbar answer {id} yes` or `bullbar answer {id} no`, then send your \
         message again (retry or regenerate). No answer within {minutes} minute(s) counts as no. \
         Nothing was sent.\n\nwhy:\n{explanation}"
    )
}

fn question_header(mut response: Response, id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-bullbar-question"), value);
    }
    response
}

/// A request's topic, and where the decision saw it pinned.
struct Topic<'a> {
    category: &'a Category,
    pinned: Option<&'a AccountId>,
}

/// Carries out a route: local routes are forwarded; a route that leaves is forwarded only when
/// every protection it lists is built, it has an address, and the floor passes the body. A route
/// with topic pinning records the pin before anything leaves.
async fn carry_out(
    gateway: &Gateway,
    route: &Route,
    topic: Topic<'_>,
    body: Value,
    explanation: Explanation,
    note: Option<String>,
) -> Outcome {
    let why = explanation.to_string();
    let label = route.to_string();
    let own = |ended, text: String, route: &str| Outcome::Own {
        ended,
        text,
        route: route.to_owned(),
        explanation: explanation.clone(),
    };
    if !route.leaves_local() {
        return Outcome::Forward {
            route: route.clone(),
            body,
            stripped: Stripped::default(),
            restorer: Arc::new(Restorer::none()),
            explanation,
            note,
        };
    }
    // Checked again here, not only in the decision: a question answered after the shutter closed
    // carries a decision made before it.
    if gateway.shutter.closed().is_some() {
        let text = format!(
            "blocked: the shutter is closed, so nothing leaves the local layer (`bullbar reopen` \
             to reopen). Nothing was sent.\n\nwhy:\n{why}"
        );
        return own(Ended::Blocked, text, "blocked");
    }
    let unbuilt: Vec<String> = route
        .mechanisms()
        .iter()
        .filter(|m| !CARRIED_OUT.contains(m))
        .map(ToString::to_string)
        .collect();
    if !unbuilt.is_empty() {
        let text = format!(
            "this request would go to {route}, which needs {}. Bullbar can't do that yet, so \
             nothing was sent.\n\nwhy:\n{why}",
            unbuilt.join(", ")
        );
        return own(Ended::NotSent, text, &label);
    }
    if !gateway.upstreams.contains_key(route.account()) {
        let text = format!(
            "this request would go to {route}, but that account has no address in \
             providers.toml, so nothing was sent.\n\nwhy:\n{why}"
        );
        return own(Ended::NotSent, text, &label);
    }
    // Scrubbing runs the entity model (about 0.06 s a message): off the async threads.
    let (floor, scrubber, keyed, leaving) = (
        gateway.floor.clone(),
        gateway.scrubber.clone(),
        gateway.keyed,
        route.clone(),
    );
    let prepared = tokio::task::spawn_blocking(move || {
        let scrub = |body: &mut Value| match &scrubber {
            Some(scrubber) => scrub_conversation(scrubber, &keyed, body),
            None => Err(ScrubError::Missing),
        };
        floor.prepare_with(body, &leaving, Some(&scrub))
    })
    .await;
    let Ok(prepared) = prepared else {
        return Outcome::Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the request couldn't be readied; nothing was sent".to_owned(),
        );
    };
    if prepared.is_ok()
        && route.mechanisms().contains(&Mechanism::TopicPinning)
        && let Err(e) = gateway
            .pins
            .record(topic.category, route.account(), topic.pinned)
    {
        return own(
            Ended::Blocked,
            format!("blocked: {e}. Nothing was sent; send it again.\n\nwhy:\n{why}"),
            "blocked",
        );
    }
    match prepared {
        // Built before anything leaves: an answer that couldn't be restored isn't asked for.
        Ok(prepared) => match Restorer::new(&prepared.replaced) {
            Ok(restorer) => Outcome::Forward {
                route: route.clone(),
                body: prepared.body,
                stripped: prepared.stripped,
                restorer: Arc::new(restorer),
                explanation,
                note,
            },
            Err(_) => Outcome::Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the reply couldn't be readied for restoring; nothing was sent".to_owned(),
            ),
        },
        Err(e) => own(
            Ended::Blocked,
            format!("blocked: {e}. Nothing was sent.\n\nwhy:\n{why}"),
            "blocked",
        ),
    }
}

/// The route's account and the model name it uses for the route's column, with the body readied
/// for it. Checked before forwarding, and config gives every address a model per column offered,
/// so `None` is a bug.
/// The account's address and the body addressed to it: its model named, and for a cloud account
/// only the chat API's own fields (what was dropped is returned, for the footer).
fn upstream<'g>(
    gateway: &'g Gateway,
    route: &Route,
    body: &mut Value,
) -> Option<(&'g Upstream, Vec<String>)> {
    let upstream = gateway.upstreams.get(route.account())?;
    let model = upstream.models.get(&route.cell().column)?;
    body["model"] = Value::String(model.clone());
    let dropped = if route.leaves_local() {
        fields::known_only(body)
    } else {
        Vec::new()
    };
    Some((upstream, dropped))
}

/// What happened to a request on its way out, for the footer: fields left behind, and the trace
/// ticket if a trace covers it.
#[derive(Default)]
struct Left {
    dropped: Vec<String>,
    traced: Option<Ticket>,
}

const NO_UPSTREAM: &str = "no address for the chosen account; nothing was sent";

/// The footer under a forwarded answer: where it came from (if footers are on), and what the
/// placeholders in it stand for (always: ADR 0009).
fn footer(
    gateway: &Gateway,
    route: &Route,
    note: Option<String>,
    stripped: &Stripped,
    left: &Left,
) -> Option<String> {
    let mut footer = Vec::new();
    if gateway.options.footer {
        let said = note.map(|n| format!(" ({n})")).unwrap_or_default();
        footer.push(format!(
            "answered by {} at {}{said}",
            route.account(),
            route.cell()
        ));
        if !left.dropped.is_empty() {
            footer.push(format!(
                "not sent (not in the chat API): {}",
                left.dropped.join(", ")
            ));
        }
    }
    footer.extend(stripped.described());
    // Said even without the footer: what's written down must never be a surprise.
    footer.extend(left.traced.as_ref().map(Ticket::footer));
    (!footer.is_empty()).then(|| footer.join(FOOTER_LINE_BREAK))
}

/// Who answers on a route, as the failure messages name it.
fn who(route: &Route) -> String {
    if route.leaves_local() {
        route.account().to_string()
    } else {
        "the local runner".to_owned()
    }
}

fn answered(route: &Route) -> Ended {
    if route.leaves_local() {
        Ended::Left
    } else {
        Ended::Local
    }
}

/// Sends a request on to the route's account and returns its answer, with the footer. A body that
/// leaves has passed the floor; `stripped` says which placeholders it carries.
/// What went with a forwarded request, needed to write its reply.
struct Sent<'a> {
    stripped: &'a Stripped,
    restorer: &'a Restorer,
    explanation: &'a Explanation,
}

async fn forward(
    gateway: &Gateway,
    route: &Route,
    model: &str,
    mut body: Value,
    sent: Sent<'_>,
    note: Option<String>,
) -> Response {
    let Some((upstream, dropped)) = upstream(gateway, route, &mut body) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, NO_UPSTREAM);
    };
    let label = route.to_string();
    let carried = carried(&body);
    let left = Left {
        traced: gateway.traced(route, &body),
        dropped,
    };
    let answer: Result<Value, _> = upstream.client.chat().create_byot(body).await;
    match answer {
        Ok(mut answer) => {
            if let Some(ticket) = &left.traced {
                ticket.reply(&answer);
            }
            answer["model"] = Value::String(model.to_owned());
            let footer = footer(gateway, route, note, sent.stripped, &left);
            restore_answer(&mut answer, sent.restorer, footer.as_deref());
            gateway
                .activity
                .note(answered(route), &label, sent.explanation);
            with_headers(Json(answer).into_response(), &label, sent.explanation)
        }
        // The model's own message isn't passed on: it can quote the request. A trace keeps it.
        Err(e) => {
            if let Some(ticket) = &left.traced {
                ticket.error(&e.to_string());
            }
            gateway
                .activity
                .note(Ended::Failed, &label, sent.explanation);
            let message = format!(
                "{} didn't answer ({}; sent: {carried}); nothing was sent anywhere else",
                who(route),
                failure(&e)
            );
            error(StatusCode::BAD_GATEWAY, &message)
        }
    }
}

/// Keeps only the answer's own fields, puts the surrogates back in every choice's text and adds
/// the footer, with what didn't come back exactly.
fn restore_answer(answer: &mut Value, restorer: &Restorer, footer: Option<&str>) {
    let Some(choices) = answer.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices {
        if let Some(message) = choice.get_mut("message") {
            fields::answer_only(message);
        }
        if let Some(Value::String(content)) = choice.pointer_mut("/message/content") {
            let report = restorer.report(content);
            *content = restorer.restore(content, None);
            if let Some(footer) = stream::with_report(footer, &report) {
                content.push_str(FOOTER_SEPARATOR);
                content.push_str(&footer);
            }
        }
    }
}

async fn open_stream(
    gateway: &Gateway,
    route: &Route,
    mut body: Value,
) -> Result<(stream::Chunks, Left), (StatusCode, String)> {
    let Some((upstream, dropped)) = upstream(gateway, route, &mut body) else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, NO_UPSTREAM.to_owned()));
    };
    body["stream"] = Value::Bool(true);
    let carried = carried(&body);
    let traced = gateway.traced(route, &body);
    let opened: Result<stream::Chunks, _> = upstream.client.chat().create_stream_byot(body).await;
    if let (Err(e), Some(ticket)) = (&opened, &traced) {
        ticket.error(&e.to_string());
    }
    // Each chunk as the provider sent it, before restoring, goes to the trace.
    let opened = opened.map(|chunks| {
        let chunks: stream::Chunks = match traced.clone() {
            None => chunks,
            Some(logging) => Box::pin(chunks.inspect(move |item| match item {
                Ok(chunk) => logging.chunk(chunk),
                Err(e) => logging.error(&e.to_string()),
            })),
        };
        (chunks, Left { dropped, traced })
    });
    // The model's own message isn't passed on: it can quote the request.
    opened.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} didn't answer ({}; sent: {carried}); nothing was sent anywhere else",
                who(route),
                failure(&e)
            ),
        )
    })
}

/// What a request carried, by name only, so a setting a provider refuses can be found: its fields,
/// and the roles of its messages. Never a value; a field name that isn't a plain identifier shows
/// as "(a field)", since it could itself be a detail.
fn carried(body: &Value) -> String {
    let Some(fields) = body.as_object() else {
        return "(not an object)".to_owned();
    };
    let plain = |name: &str| {
        name.len() <= 40
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let mut names: Vec<String> = fields
        .iter()
        .map(|(name, value)| match (name.as_str(), value) {
            ("messages", Value::Array(messages)) => {
                let roles: Vec<&str> = messages
                    .iter()
                    .map(|m| m["role"].as_str().filter(|r| plain(r)).unwrap_or("?"))
                    .collect();
                format!("messages ({})", roles.join(", "))
            }
            (name, _) if plain(name) => name.to_owned(),
            _ => "(a field)".to_owned(),
        })
        .collect();
    names.sort();
    names.join(", ")
}

/// A provider-chosen string, shown only if it looks like an identifier.
fn plain(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.[]".contains(c))
    })
}

/// What kind of failure a model's error was, without its message, which can quote the request:
/// the HTTP status, and the error's type, code and parameter when they are plain identifiers.
fn failure(e: &async_openai::error::OpenAIError) -> String {
    use async_openai::error::OpenAIError;
    match e {
        OpenAIError::ApiError(r) => {
            let mut out = format!("HTTP {}", r.status_code.as_u16());
            let e = &r.api_error;
            for (label, value) in [("", &e.r#type), ("code ", &e.code), ("in ", &e.param)] {
                if let Some(value) = plain(value) {
                    out.push_str(&format!(", {label}{value}"));
                }
            }
            out
        }
        OpenAIError::Reqwest(e) if e.is_timeout() => "it timed out".to_owned(),
        OpenAIError::Reqwest(e) if e.is_connect() => "it couldn't be reached".to_owned(),
        OpenAIError::Reqwest(e) => e.status().map_or("the connection failed".to_owned(), |s| {
            format!("HTTP {}", s.as_u16())
        }),
        OpenAIError::JSONDeserialize(..) => "an answer Bullbar couldn't read".to_owned(),
        OpenAIError::StreamError(_) => "the stream broke".to_owned(),
        _ => "the request couldn't be made".to_owned(),
    }
}

/// Writes an outcome as a streamed reply. What can be known before the stream starts (an error,
/// a model that won't answer) is an ordinary error reply; the rest goes on the stream.
async fn stream_reply(
    gateway: Arc<Gateway>,
    request: Request,
    outcome: Outcome,
    started: Instant,
) -> Response {
    match outcome {
        Outcome::Error(status, message) => error(status, &message),
        Outcome::Own {
            ended,
            text,
            route,
            explanation,
        } => {
            gateway.activity.note(ended, &route, &explanation);
            let (writer, response) = stream::channel(&request.model, now());
            tokio::spawn(async move { writer.own(&text).await });
            with_headers(response, &route, &explanation)
        }
        Outcome::Forward {
            route,
            body,
            stripped,
            restorer,
            explanation,
            note,
        } => {
            let label = route.to_string();
            let (chunks, left) = match open_stream(&gateway, &route, body).await {
                Ok(opened) => opened,
                Err((status, message)) => {
                    gateway.activity.note(Ended::Failed, &label, &explanation);
                    return error(status, &message);
                }
            };
            let footer = footer(&gateway, &route, note, &stripped, &left);
            let (writer, response) = stream::channel(&request.model, now());
            let response = with_headers(response, &label, &explanation);
            tokio::spawn(async move {
                let relayed = writer
                    .relay(chunks, footer.as_deref(), &who(&route), &restorer)
                    .await;
                gateway
                    .activity
                    .note(after_relay(&route, relayed), &label, &explanation);
            });
            response
        }
        Outcome::Ask {
            id,
            question,
            explanation,
            lifecycle,
            note,
        } => {
            let (writer, response) = stream::channel(&request.model, now());
            let response = question_header(with_headers(response, "ask", &explanation), &id);
            let asked = Asked {
                id,
                question,
                explanation,
                lifecycle: *lifecycle,
                note,
            };
            tokio::spawn(wait_on_stream(gateway, writer, request, asked, started));
            response
        }
    }
}

fn after_relay(route: &Route, relayed: stream::Relayed) -> Ended {
    match relayed {
        stream::Relayed::CutOff => Ended::Failed,
        // The request left even if the client went away before the end.
        stream::Relayed::Whole | stream::Relayed::Gone => answered(route),
    }
}

/// A question asked on a streamed reply.
struct Asked {
    id: String,
    question: Question,
    explanation: Explanation,
    lifecycle: Lifecycle,
    note: Option<String>,
}

/// A streamed reply whose question is open: the question goes out at once, the stream stays open
/// (with keep-alive lines) for up to the stream hold, and then carries on from the answer. If the
/// client goes away meanwhile, nothing more happens: the question stays open for a resend.
async fn wait_on_stream(
    gateway: Arc<Gateway>,
    writer: stream::Writer,
    request: Request,
    asked: Asked,
    started: Instant,
) {
    let Asked {
        id,
        question,
        explanation,
        lifecycle,
        note,
    } = asked;
    let waiting = format!(
        "{} {question}\n\nAnswer in Bullbar: `bullbar answer {id} yes` or `bullbar answer {id} no`. \
         Waiting here for up to {} seconds; nothing has been sent.\n\nwhy:\n{explanation}",
        reply::MARKER,
        gateway.options.stream_hold.as_secs()
    );
    if !writer.text(&waiting).await {
        return;
    }
    let deadline = started + gateway.options.stream_hold;
    let outcome = tokio::select! {
        outcome = drive(&gateway, lifecycle, note, &request, deadline) => outcome,
        () = writer.closed() => return,
    };
    match outcome {
        Outcome::Own {
            ended,
            text,
            route,
            explanation,
        } => {
            gateway.activity.note(ended, &route, &explanation);
            let _ =
                writer.text(&format!("{FOOTER_SEPARATOR}{text}")).await && writer.finish().await;
        }
        Outcome::Ask {
            id, explanation, ..
        } => {
            gateway.activity.note(Ended::Asked, "ask", &explanation);
            let text = format!(
                "{FOOTER_SEPARATOR}no answer yet. {}",
                still_open(&id, &explanation, gateway.options.expiry)
            );
            let _ = writer.text(&text).await && writer.finish().await;
        }
        Outcome::Forward {
            route,
            body,
            stripped,
            restorer,
            explanation,
            note,
        } => {
            let label = route.to_string();
            // Nobody to answer: nothing is sent.
            if writer.is_gone() {
                return;
            }
            match open_stream(&gateway, &route, body).await {
                Err((_, message)) => {
                    gateway.activity.note(Ended::Failed, &label, &explanation);
                    let _ = writer.text(&format!("{FOOTER_SEPARATOR}{message}")).await
                        && writer.finish().await;
                }
                Ok((chunks, left)) => {
                    if writer.is_gone() || !writer.text(ANSWER_SEPARATOR).await {
                        return;
                    }
                    let footer = footer(&gateway, &route, note, &stripped, &left);
                    let relayed = writer
                        .relay(chunks, footer.as_deref(), &who(&route), &restorer)
                        .await;
                    gateway
                        .activity
                        .note(after_relay(&route, relayed), &label, &explanation);
                }
            }
        }
        Outcome::Error(_, message) => {
            let _ =
                writer.text(&format!("{FOOTER_SEPARATOR}{message}")).await && writer.finish().await;
        }
    }
}

/// A reply Bullbar writes itself, with the decision in the headers.
fn own(model: &str, text: &str, route: &str, explanation: &Explanation) -> Response {
    let response = Json(reply::own(model, text, now())).into_response();
    with_headers(response, route, explanation)
}

/// An error in the chat API's shape, marked as Bullbar's.
fn error(status: StatusCode, message: &str) -> Response {
    let body = json!({
        "error": { "message": format!("{} {message}", reply::MARKER), "type": "bullbar" }
    });
    (status, Json(body)).into_response()
}

/// Adds the decision as headers: where it went and why (ADR 0016, show the source).
fn with_headers(mut response: Response, route: &str, explanation: &Explanation) -> Response {
    let why: Vec<String> = explanation
        .reasons()
        .iter()
        .map(ToString::to_string)
        .collect();
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-bullbar-route", route.to_owned()),
        ("x-bullbar-why", why.join("; ")),
    ] {
        if let Ok(value) = HeaderValue::from_str(&header_safe(&value)) {
            headers.insert(HeaderName::from_static(name), value);
        }
    }
    response
}

/// Header values must be visible ASCII.
fn header_safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn today() -> Option<Date> {
    Date::from_days_since_1970(i64::try_from(now() / 86_400).ok()?)
}
