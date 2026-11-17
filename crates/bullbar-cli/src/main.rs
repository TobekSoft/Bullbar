//! `bullbar`: runs Bullbar's endpoint (`serve`), answers its questions (`pending`, `answer`), and
//! dry-run tools for the decision engine that read a config directory, decide where a request would
//! go and explain why, sending nothing.

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bullbar_core::capabilities::Capabilities;
use bullbar_core::config::{Config, ConfigText};
use bullbar_core::levels::{Category, Column, Complexity, Row, ScrubberGrade, Sensitivity};
use bullbar_core::lifecycle::Attempt;
use bullbar_core::policy::Escalation;
use bullbar_core::profile::RequestProfile;
use bullbar_core::provider::{AccountStatus, Date, ProviderAccount};
use bullbar_core::scenario;
use bullbar_core::slots::{Slot, SlotState};
use bullbar_gateway::key;
use bullbar_gateway::scrub::{Scrubber, scrub_text};
use bullbar_gateway::server::{self, Listen, Options};
use bullbar_gateway::{control, shutter};
use clap::{Args, Parser, Subcommand};

/// Bullbar's endpoint, and dry-run tools for its decision engine.
#[derive(Parser)]
#[command(name = "bullbar", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decide where one request would go, and why.
    Decide(DecideArgs),
    /// Load a config directory and report what it sets up.
    CheckConfig {
        /// The config directory (providers.toml, policy.toml, slots.toml; user.toml and
        /// ledger.toml if present).
        #[arg(long)]
        config: PathBuf,
        /// The date to check review dates against, YYYY-MM-DD. Default: today (UTC).
        #[arg(long)]
        today: Option<Date>,
    },
    /// Run every scenario and report which decide as expected.
    Scenarios {
        /// The directory of scenario files.
        #[arg(long, default_value = "scenarios")]
        scenarios: PathBuf,
        /// The directory holding one config directory per edition the scenarios name.
        #[arg(long, default_value = "config")]
        configs: PathBuf,
        /// List every scenario, not only the ones that fail.
        #[arg(long)]
        verbose: bool,
    },
    /// Show what would leave if a message were scrubbed: reads the message from stdin, prints it
    /// with each personal detail replaced, and what was taken out. Nothing is sent anywhere.
    Scrub {
        /// The entity model's folder (`tokenizer.json`, `model.onnx`), for names, places and
        /// organisations. Without it only patterns are used.
        #[arg(long)]
        model: Option<PathBuf>,
        /// The state folder, whose scrubbing secret makes the surrogates (made if missing), so
        /// they are the ones `serve` would send. Default as for `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Run Bullbar's endpoint: point a chat client at it and ask for the model `bullbar`.
    /// A request leaves only with every protection its route needs; where one isn't built or set
    /// up, Bullbar answers itself and says which.
    Serve {
        /// The config directory.
        #[arg(long)]
        config: PathBuf,
        /// The address to listen on. Loopback by default: anything that reaches it can use the
        /// local runner.
        #[arg(long, default_value = "127.0.0.1:8480")]
        listen: SocketAddr,
        /// Leave out the line naming where each answer came from.
        #[arg(long)]
        no_footer: bool,
        /// Where the control channel listens: where `pending` and `answer` reach Bullbar.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, holding the control token. Default: $XDG_STATE_HOME/bullbar, or
        /// ~/.local/state/bullbar.
        #[arg(long)]
        state: Option<PathBuf>,
        /// Seconds to hold a chat request open while its question waits, before replying with a
        /// notice saying where to answer.
        #[arg(long, default_value_t = 20)]
        hold: u64,
        /// The same for a streamed reply: the question shows at once, and the stream stays open
        /// this many seconds for the answer.
        #[arg(long, default_value_t = 120)]
        stream_hold: u64,
        /// Log each decision to standard error: outcome, route and reasons, never content. Off by
        /// default: nothing about requests is written anywhere.
        #[arg(long, value_parser = ["decisions"])]
        log: Option<String>,
        /// The PII scrubber's entity model folder (`scripts/fetch-pii-model.sh`). Without it the
        /// scrubber has patterns only, which count as no scrubber: personal work stays local.
        #[arg(long)]
        pii_model: Option<PathBuf>,
    },
    /// Close the shutter: nothing leaves the local layer; local answers go on. Kept across
    /// restarts; `reopen` lifts it.
    Kill {
        /// Why, for the record (never request content).
        #[arg(long, default_value = "closed by hand")]
        reason: String,
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Reopen the shutter.
    Reopen {
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Log what leaves, for a while: the next N requests sent to a cloud account (an hour at most),
    /// as sent (scrubbed) and the provider's raw reply, to trace.jsonl in the state folder. Never
    /// the originals, nor anything answered locally. `off` stops it; `clear` also deletes the file.
    Trace {
        /// How many requests, 1 to 100; or `off`, or `clear`.
        what: String,
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Bullbar's state: the shutter, open questions, and counts since it started.
    Status {
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// List Bullbar's open questions (from a running `serve`).
    Pending {
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Answer one of Bullbar's questions. No answer counts as no.
    Answer {
        /// The question's id, from `pending` or the notice in the chat.
        id: String,
        /// Your answer.
        #[arg(value_parser = ["yes", "no"])]
        answer: String,
        /// Where the control channel listens.
        #[arg(long, default_value = DEFAULT_CONTROL)]
        control: SocketAddr,
        /// The state folder, as given to `serve`.
        #[arg(long)]
        state: Option<PathBuf>,
    },
}

/// The control channel's default address: loopback, next to the chat endpoint.
const DEFAULT_CONTROL: &str = "127.0.0.1:8481";

#[derive(Args)]
struct DecideArgs {
    /// The config directory.
    #[arg(long)]
    config: PathBuf,
    /// How hard the request is: easy, medium, hard.
    #[arg(long)]
    complexity: Complexity,
    /// Your rating: public, personal, sensitive, secret. Unrated counts as sensitive.
    #[arg(long)]
    sensitivity: Option<Sensitivity>,
    /// The classifier's rating, if it gave one.
    #[arg(long)]
    classifier: Option<Sensitivity>,
    /// What the request is about: health, legal, finance, work-code, personal, general, or your own.
    #[arg(long, default_value = "general")]
    category: Category,
    /// The credential scanner found credentials in it.
    #[arg(long)]
    credentials: bool,
    /// The most exposed row this request may reach, e.g. `local` for "local only".
    #[arg(long)]
    row_limit: Option<Row>,
    /// Ask for at least this column: small, mid, top.
    #[arg(long)]
    want: Option<Column>,
    /// You have confirmed a rating below the classifier's.
    #[arg(long)]
    confirm_lower: bool,
    /// One-off exception rungs, for a retry after a poor answer (ADR 0011).
    #[arg(long, default_value_t = 0)]
    rungs: u8,
    /// The date to decide on, YYYY-MM-DD. Default: today (UTC).
    #[arg(long)]
    today: Option<Date>,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Decide(args) => decide(args),
        Command::CheckConfig { config, today } => {
            check_config(&config, today.map_or_else(system_today, Ok)?)
        }
        Command::Scenarios {
            scenarios,
            configs,
            verbose,
        } => run_scenarios(&scenarios, &configs, verbose),
        Command::Serve {
            config,
            listen,
            no_footer,
            control,
            state,
            hold,
            stream_hold,
            log,
            pii_model,
        } => {
            let loaded = read_config(&config)?;
            eprintln!(
                "bullbar: serving {} on http://{listen}/v1",
                config.display()
            );
            // ADR 0022: said where whoever runs it sees it; never in a chat.
            eprintln!(
                "bullbar: a proof of concept, not a tool to rely on: what it misses is in the README"
            );
            let listen = Listen {
                chat: listen,
                control,
                state: state_dir(state)?,
            };
            let options = Options {
                footer: !no_footer,
                hold: Duration::from_secs(hold),
                stream_hold: Duration::from_secs(stream_hold),
                ..Options::default()
            };
            let scrubber = match &pii_model {
                Some(dir) => {
                    eprintln!("bullbar: loading the entity model from {}", dir.display());
                    Some(Scrubber::with_entities(dir)?)
                }
                None => None,
            };
            server::serve(loaded, listen, options, log.is_some(), scrubber)
                .with_context(|| format!("serving {}", config.display()))
        }
        Command::Scrub { model, state } => scrub(model.as_deref(), &state_dir(state)?),
        Command::Pending { control, state } => list_pending(control, &state_dir(state)?),
        Command::Kill {
            reason,
            control,
            state,
        } => set_shutter(control, &state_dir(state)?, Some(&reason)),
        Command::Reopen { control, state } => set_shutter(control, &state_dir(state)?, None),
        Command::Status { control, state } => show_status(control, &state_dir(state)?),
        Command::Trace {
            what,
            control,
            state,
        } => {
            let state = state_dir(state)?;
            let (requests, clear) = match what.as_str() {
                "off" => (None, false),
                "clear" => (None, true),
                n => (
                    Some(
                        n.parse::<u32>()
                            .context("give a number of requests, `off` or `clear`")?,
                    ),
                    false,
                ),
            };
            let token = control::read_token(&state)?;
            let now = control::client::trace(control, &token, requests, clear)?;
            println!("{}", trace_line(&now, &state));
            Ok(())
        }
        Command::Answer {
            id,
            answer,
            control,
            state,
        } => {
            let token = control::read_token(&state_dir(state)?)?;
            control::client::answer(control, &token, &id, answer == "yes")?;
            println!(
                "answered {id}: {answer}. A chat still waiting carries on by itself; otherwise send your message again."
            );
            Ok(())
        }
    }
}

/// The state folder: as given, else $XDG_STATE_HOME/bullbar, else ~/.local/state/bullbar.
/// `bullbar scrub`: the scrubber on a message from stdin, nothing sent.
fn scrub(model: Option<&Path>, state: &Path) -> Result<()> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).context("reading stdin")?;
    let scrubber = match model {
        Some(dir) => Scrubber::with_entities(dir)?,
        None => Scrubber::patterns_only()?,
    };
    let keyed = server::scrubbing_keys(state)?;
    let (leaves, replaced) = scrub_text(&scrubber, &keyed, text.trim_end_matches('\n'))?;
    println!("leaves as:\n{leaves}\n");
    if replaced.is_empty() {
        println!("taken out: nothing");
    } else {
        let width = replaced
            .entries()
            .map(|(_, _, s)| s.chars().count())
            .max()
            .unwrap_or(0);
        println!("taken out:");
        for (kind, original, surrogate) in replaced.entries() {
            println!("  {surrogate:width$}  {kind}: {original}");
        }
    }
    match scrubber.grade() {
        ScrubberGrade::Patterns => println!(
            "\nFound by patterns only: names, places and organisations need --model <dir>."
        ),
        grade => println!("\nScrubber grade: {grade}."),
    }
    Ok(())
}

fn state_dir(given: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = given {
        return Ok(dir);
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("bullbar"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .context("no state folder: give --state, or set HOME or XDG_STATE_HOME")?;
    Ok(PathBuf::from(home).join(".local/state/bullbar"))
}

/// Closes the shutter (with a reason) or reopens it (without). Through the running gateway if
/// there is one; otherwise in the state folder, so the next start is closed or open.
fn set_shutter(at: SocketAddr, state: &Path, close: Option<&str>) -> Result<()> {
    let word = if close.is_some() { "closed" } else { "open" };
    let token = control::read_token(state);
    let through = match &token {
        Ok(token) => control::client::shutter(at, token, close.is_some(), close),
        Err(_) => Err(control::client::ClientError::Unreachable(at)),
    };
    match through {
        Ok(_) => println!("shutter {word}"),
        Err(control::client::ClientError::Unreachable(_)) => {
            match close {
                Some(reason) => shutter::close(state, reason).map(|_| ())?,
                None => shutter::open(state)?,
            }
            println!("Bullbar isn't running at {at}; it will start with the shutter {word}");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Prints the shutter, the questions and the counts; with Bullbar not running, the shutter alone.
/// The trace's state, in a line.
fn trace_line(trace: &control::client::Value, state: &Path) -> String {
    if trace["on"] == true {
        format!(
            "trace: on, {} more requests (ends within {} min); logging what leaves to {}",
            trace["left"],
            trace["ends_in_seconds"]
                .as_u64()
                .unwrap_or_default()
                .div_ceil(60),
            state.join(bullbar_gateway::trace::FILE).display()
        )
    } else {
        "trace: off".to_owned()
    }
}

fn show_status(at: SocketAddr, state: &Path) -> Result<()> {
    let status = control::read_token(state)
        .map_err(anyhow::Error::from)
        .and_then(|token| Ok(control::client::status(at, &token)?));
    let Ok(status) = status else {
        println!("Bullbar isn't running at {at}");
        match shutter::read(state) {
            Some(closed) => println!(
                "shutter: closed since {} ({}); it will start closed",
                shutter::utc(closed.since),
                closed.reason
            ),
            None => println!("shutter: open"),
        }
        return Ok(());
    };
    let s = &status["shutter"];
    if s["closed"] == true {
        println!(
            "shutter: closed since {} ({}): nothing leaves; `bullbar reopen` to lift it",
            shutter::utc(s["since"].as_u64().unwrap_or_default()),
            s["reason"].as_str().unwrap_or_default()
        );
    } else {
        println!("shutter: open");
    }
    println!("{}", trace_line(&status["trace"], state));
    let q = &status["questions"];
    println!(
        "questions: {} waiting, {} settled",
        q["waiting"], q["settled"]
    );
    let c = &status["counts"];
    println!(
        "since start: {} answered locally, {} answered outside, {} refused, {} asked, {} blocked, \
         {} not sent, {} failed",
        c["local"], c["left"], c["refused"], c["asked"], c["blocked"], c["not_sent"], c["failed"]
    );
    let (left, refused) = (
        &c["left_for_want_of_local"],
        &c["refused_for_want_of_local"],
    );
    if left != 0 || refused != 0 {
        println!(
            "for want of a local runner: {left} sensitive request(s) sent out scrubbed, {refused} \
             refused"
        );
    }
    Ok(())
}

/// Prints the open questions, oldest first.
fn list_pending(at: SocketAddr, state: &Path) -> Result<()> {
    let token = control::read_token(state)?;
    let questions = control::client::pending(at, &token)?;
    if questions.is_empty() {
        println!("no open questions");
    }
    for q in questions {
        let text = |key: &str| q[key].as_str().unwrap_or_default().to_owned();
        println!(
            "{}  {}  {}s  {}",
            text("id"),
            text("answer"),
            q["waiting_seconds"].as_u64().unwrap_or_default(),
            text("model")
        );
        println!("    {}", text("question"));
        if let Some(why) = q["why"].as_array() {
            for (n, reason) in why.iter().enumerate() {
                println!("    {}. {}", n + 1, reason.as_str().unwrap_or_default());
            }
        }
    }
    Ok(())
}

fn run_scenarios(dir: &Path, configs: &Path, verbose: bool) -> Result<()> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    files.retain(|path| path.extension().is_some_and(|e| e == "toml"));
    files.sort();
    let mut editions: BTreeMap<String, Config> = BTreeMap::new();
    let (mut passed, mut failed) = (0, 0);
    for file in &files {
        let text =
            fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let scenarios = scenario::parse(&text).with_context(|| format!("in {}", file.display()))?;
        let file_name = file
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        for s in scenarios {
            if !editions.contains_key(&s.edition) {
                let config = read_config(&configs.join(&s.edition))
                    .with_context(|| format!("edition `{}`, for {file_name}", s.edition))?;
                editions.insert(s.edition.clone(), config);
            }
            let Some(config) = editions.get(&s.edition) else {
                bail!("edition `{}` didn't load", s.edition);
            };
            let checked = s.run(config);
            if checked.passed() {
                passed += 1;
                if verbose {
                    println!("ok    {file_name}: {}", checked.name);
                }
            } else {
                failed += 1;
                println!("FAIL  {file_name}: {}", checked.name);
                for failure in &checked.failures {
                    println!("      {failure}");
                }
                for line in checked.decision.lines() {
                    println!("      | {line}");
                }
            }
        }
    }
    println!(
        "\n{} scenarios: {passed} passed, {failed} failed",
        passed + failed
    );
    if failed > 0 {
        bail!("{failed} scenario(s) failed");
    }
    Ok(())
}

fn decide(args: DecideArgs) -> Result<()> {
    let today = args.today.map_or_else(system_today, Ok)?;
    let engine = read_config(&args.config)?
        .engine()
        .context("the engine won't start")?;
    let profile = RequestProfile {
        classifier_rating: args.classifier,
        user_rating: args.sensitivity,
        credentials_found: args.credentials,
        row_limit: args.row_limit,
        wanted_column: args.want,
        ..RequestProfile::new(args.complexity, args.category)
    };
    let attempt = Attempt {
        lower_rating_confirmed: args.confirm_lower,
        rungs: args.rungs,
        ..Attempt::default()
    };
    let decision = engine.decide_attempt(&profile, &attempt, today);
    print!("{}\n\nwhy:\n{}", decision.outcome, decision.explanation);
    Ok(())
}

fn check_config(dir: &Path, today: Date) -> Result<()> {
    let config = read_config(dir)?;
    let started = config.engine();
    let usable = |slot| config.slots.usable(slot);

    println!("{}, checked on {today}", dir.display());
    println!("\naccounts:");
    for account in &config.accounts {
        let offers: Vec<String> = account
            .offers
            .iter()
            .map(|o| format!("{} {}", o.column, o.cost))
            .collect();
        println!(
            "  {:<14} {:<22} {}",
            account.id.as_str(),
            account.row.as_str(),
            offers.join(", ")
        );
        for note in account_notes(account, today, &usable) {
            println!("  {:<14} - {note}", "");
        }
    }

    println!("\nslots:");
    for state in [SlotState::Healthy, SlotState::Unhealthy, SlotState::Empty] {
        let names: Vec<String> = Slot::ALL
            .iter()
            .filter(|&&slot| config.slots.state(slot) == state)
            .map(|&slot| {
                if slot.is_required() {
                    format!("{} (required)", slot.key())
                } else {
                    slot.key()
                }
            })
            .collect();
        if !names.is_empty() {
            println!("  {state}: {}", names.join(", "));
        }
    }

    if let Some(grade) = config.slots.scrubber_grade() {
        let note = if grade < ScrubberGrade::Entities {
            " (too weak: counts as empty)"
        } else {
            ""
        };
        println!("  pii-scrubber grade: {grade}{note}");
    }

    let capabilities = Capabilities::new(&config.slots);
    println!("\ncapabilities (what the slots can do, given what each depends on):");
    let working: Vec<&str> = capabilities.working().map(|c| c.as_str()).collect();
    if !working.is_empty() {
        println!("  working: {}", working.join(", "));
    }
    for (cap, slot) in capabilities.lacking() {
        println!("  without {cap}: no working {slot}");
    }

    let (admin, user) = (config.policy.admin(), config.policy.user());
    println!("\npolicy (the user's values, within the admin's):");
    for rating in [
        Sensitivity::Public,
        Sensitivity::Personal,
        Sensitivity::Sensitive,
    ] {
        println!(
            "  {:<10} up to {}",
            rating.as_str(),
            user.ceilings.for_rating(rating).as_str()
        );
    }
    println!("  {:<10} local only, always", Sensitivity::Secret.as_str());
    for (category, rule) in &admin.categories {
        let row = config
            .policy
            .category_ceiling(category)
            .unwrap_or(rule.ceiling);
        let locked = if rule.locked { ", locked" } else { "" };
        println!("  category {category}: up to {}{locked}", row.as_str());
    }
    for (category, row) in &user.categories {
        if !admin.categories.contains_key(category) {
            println!("  category {category}: up to {} (yours)", row.as_str());
        }
    }
    let m = &admin.minimum_columns;
    println!(
        "  columns: easy {}, medium {}, hard {}; at most {}",
        m.easy, m.medium, m.hard, user.max_column
    );
    println!(
        "  strip-and-send {}, one-off exceptions {}, lower ratings {}, \
         sensitive without a local model {}",
        on_off(user.strip_and_send),
        on_off(user.exceptions),
        allowed(admin.lower_ratings_allowed),
        if user.send_sensitive_without_local {
            "leaves scrubbed"
        } else {
            "refused"
        }
    );
    println!("  escalation: {}", escalation(user.escalation));
    if !config.slots.usable(Slot::LocalRunner) {
        println!(
            "  chaff without a local runner: {}",
            if user.chaff_without_local {
                "canned"
            } else {
                "off"
            }
        );
    }
    if config.ledger.threshold() < u32::MAX {
        println!(
            "  ledger: skip an account past {} of a category",
            config.ledger.threshold()
        );
    }

    started.context("the engine won't start")?;
    println!("\nok: the engine starts");
    Ok(())
}

/// Why an account is used differently from how it is written, today.
fn account_notes(
    account: &ProviderAccount,
    today: Date,
    usable: &dyn Fn(Slot) -> bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    match (&account.address, account.row) {
        (Some(address), _) => {
            notes.push(format!("at {}", address.base_url));
            if let Some(path) = &address.key_file {
                notes.push(match key::read(path) {
                    Ok(_) => format!("key from {}", path.display()),
                    Err(e) => format!("{e}: `serve` leaves it out"),
                });
                if key::readable_by_others(path) == Some(true) {
                    notes.push(format!(
                        "{} is readable by other users; `chmod 600` it",
                        path.display()
                    ));
                }
            }
        }
        (None, Row::Local) => {
            notes.push("no address: dry runs only, `serve` leaves it out".to_owned())
        }
        (None, _) => notes.push("no address: dry runs only".to_owned()),
    }
    if account.status == AccountStatus::Out {
        notes.push("out of rotation: never used".to_owned());
        return notes;
    }
    if account.row == Row::Local {
        if !usable(Slot::LocalRunner) {
            notes.push("not used: no working local runner".to_owned());
        }
        return notes;
    }
    if !usable(Slot::ProviderAdapters) {
        notes.push("not used: no working provider adapters".to_owned());
        return notes;
    }
    if !usable(Slot::TermsRegister) {
        notes.push("counts as provider-standard: no working terms register".to_owned());
        return notes;
    }
    let demoted_to = account.row.up(1).as_str();
    if account.status == AccountStatus::Demoted {
        notes.push(format!("demoted until reviewed: counts as {demoted_to}"));
    } else if let Some(due) = account.review_due {
        if due < today {
            notes.push(format!(
                "review overdue since {due}: counts as {demoted_to}"
            ));
        } else {
            notes.push(format!("review due {due}"));
        }
    } else {
        notes.push("no review date".to_owned());
    }
    notes
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn allowed(on: bool) -> &'static str {
    if on { "allowed" } else { "forbidden" }
}

fn escalation(setting: Escalation) -> &'static str {
    match setting {
        Escalation::Auto => "automatic",
        Escalation::AskBeforeLeavingLocal => "ask before first leaving the local layer",
        Escalation::Ask => "ask every time",
        Escalation::Off => "off",
    }
}

/// Reads the config files in a directory. user.toml and ledger.toml are optional.
fn read_config(dir: &Path) -> Result<Config> {
    let read = |name: &str| {
        let path = dir.join(name);
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    };
    let optional = |name: &str| -> Result<Option<String>> {
        if dir.join(name).exists() {
            read(name).map(Some)
        } else {
            Ok(None)
        }
    };
    let (providers, policy, slots) = (
        read("providers.toml")?,
        read("policy.toml")?,
        read("slots.toml")?,
    );
    let (user, ledger) = (optional("user.toml")?, optional("ledger.toml")?);
    let mut config = Config::load(ConfigText {
        providers: &providers,
        policy: &policy,
        user: user.as_deref(),
        slots: &slots,
        ledger: ledger.as_deref(),
    })
    .with_context(|| format!("in {}", dir.display()))?;
    // Key files are named relative to the config directory.
    for account in &mut config.accounts {
        if let Some(path) = account.address.as_mut().and_then(|a| a.key_file.as_mut())
            && path.is_relative()
        {
            *path = dir.join(&*path);
        }
    }
    Ok(config)
}

/// Today's date in UTC, from the system clock.
fn system_today() -> Result<Date> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the system clock is before 1970")?
        .as_secs();
    Date::from_days_since_1970(i64::try_from(secs / 86_400)?)
        .context("the system date is out of range")
}
