//! The `bullbar` binary against the sample configs.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn bullbar(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bullbar"))
        .args(args)
        .output()
        .expect("the binary runs")
}

fn edition(name: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../config")
        .join(name);
    dir.to_str().expect("a UTF-8 path").to_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn decide_prints_the_route_and_why() {
    let config = edition("casual");
    let out = bullbar(&[
        "decide",
        "--config",
        &config,
        "--sensitivity",
        "public",
        "--complexity",
        "hard",
        "--today",
        "2026-11-15",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("route: provider-b at provider-standard / top"),
        "{text}"
    );
    assert!(text.contains("\nwhy:\n1. rated public by you\n"), "{text}");
}

#[test]
fn decide_passes_retry_rungs_to_the_engine() {
    // Paranoid's relay is the admin's own limit for personal requests, so the rung stops there.
    let config = edition("paranoid");
    let decide = |rungs: &str| {
        let out = bullbar(&[
            "decide",
            "--config",
            &config,
            "--sensitivity",
            "personal",
            "--complexity",
            "hard",
            "--rungs",
            rungs,
            "--today",
            "2026-11-15",
        ]);
        stdout(&out)
    };
    let stop = "the admin's limit is anonymous-relay, so the exception stops there";
    assert!(!decide("0").contains(stop));
    assert!(decide("1").contains(stop), "{}", decide("1"));
}

#[test]
fn every_sample_config_checks_out() {
    for name in ["casual", "paranoid", "corporate", "gateway-only"] {
        let config = edition(name);
        let out = bullbar(&["check-config", "--config", &config, "--today", "2026-11-15"]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert!(stdout(&out).ends_with("ok: the engine starts\n"), "{name}");
    }
}

#[test]
fn the_container_accounts_differ_from_casual_only_in_where_the_runner_is() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
    let (casual, container) = (
        read("config/casual/providers.toml"),
        read("container/providers.toml"),
    );
    // The container file's header comment is its own; compare from the first account on.
    let accounts = |text: &str| text[text.find("[accounts.").unwrap()..].to_owned();
    assert_eq!(
        accounts(&casual).replace("http://127.0.0.1:11434/v1", "http://ollama:11434/v1"),
        accounts(&container),
        "container/providers.toml has drifted from config/casual/providers.toml"
    );
}

#[test]
fn check_config_names_an_overdue_review() {
    let config = edition("paranoid");
    let out = bullbar(&["check-config", "--config", &config, "--today", "2026-12-20"]);
    let text = stdout(&out);
    assert!(
        text.contains("review overdue since 2026-12-15: counts as provider-standard"),
        "{text}"
    );
}

#[test]
fn check_config_names_what_a_down_runner_takes_away() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("runner-down");
    fs::create_dir_all(&dir).unwrap();
    for file in ["providers.toml", "policy.toml", "user.toml", "ledger.toml"] {
        fs::copy(
            PathBuf::from(edition("paranoid")).join(file),
            dir.join(file),
        )
        .unwrap();
    }
    let slots = fs::read_to_string(PathBuf::from(edition("paranoid")).join("slots.toml")).unwrap();
    fs::write(
        dir.join("slots.toml"),
        format!("{slots}unhealthy = [\"local-runner\"]\n"),
    )
    .unwrap();
    let out = bullbar(&["check-config", "--config", dir.to_str().unwrap()]);
    let text = stdout(&out);
    assert!(
        text.contains("without personas: no working local runner"),
        "{text}"
    );
    assert!(text.contains("working: topic pinning, chaff,"), "{text}");
}

#[test]
fn a_loosened_user_file_fails_with_a_clear_message() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("loosened");
    fs::create_dir_all(&dir).unwrap();
    for file in ["providers.toml", "policy.toml", "slots.toml"] {
        fs::copy(
            PathBuf::from(edition("corporate")).join(file),
            dir.join(file),
        )
        .unwrap();
    }
    fs::write(
        dir.join("user.toml"),
        "[ceilings]\nsensitive = \"provider-standard\"\n",
    )
    .unwrap();
    let out = bullbar(&["check-config", "--config", dir.to_str().unwrap()]);
    assert!(!out.status.success());
    let text = stderr(&out);
    assert!(
        text.contains(
            "user.toml: the user's ceiling for sensitive requests is provider-standard, \
                       more exposed than the admin's local"
        ),
        "{text}"
    );
}

#[test]
fn a_missing_config_file_names_the_path() {
    let out = bullbar(&["check-config", "--config", "/nonexistent-bullbar-config"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("reading /nonexistent-bullbar-config/providers.toml"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn every_scenario_passes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (scenarios, configs) = (root.join("scenarios"), root.join("config"));
    let out = bullbar(&[
        "scenarios",
        "--scenarios",
        scenarios.to_str().unwrap(),
        "--configs",
        configs.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(
        stdout(&out).contains(" passed, 0 failed"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn scrub_with_a_missing_model_fails_rather_than_falling_back() {
    let out = bullbar(&[
        "scrub",
        "--model",
        "/nonexistent/bullbar-model",
        "--state",
        "/nonexistent/bullbar-state",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("tokenizer.json is missing"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn scrub_shows_what_would_leave_and_what_was_taken_out() {
    use std::io::Write;
    use std::process::Stdio;
    let state = std::env::temp_dir().join(format!("bullbar-cli-scrub-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_bullbar"))
        .args(["scrub", "--state", state.to_str().expect("a UTF-8 path")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"Call me on 07700 900123 about the flat at 3B Gloamwick Court.\n")
        .expect("written");
    let out = child.wait_with_output().expect("finished");
    let _ = fs::remove_dir_all(&state);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "leaves as:\n\
         Call me on [PHONE_1] about the flat at [ADDRESS_1].\n\
         \n\
         taken out:\n  \
         [PHONE_1]    a phone number: 07700 900123\n  \
         [ADDRESS_1]  an address: 3B Gloamwick Court\n\
         \n\
         Found by patterns only: names, places and organisations need --model <dir>.\n"
    );
}

/// `serve` says among its start lines that Bullbar is a proof of concept: someone who runs it
/// without reading the docs still learns it (ADR 0022, decided 2026-11-17). In the log only, never
/// in a chat.
#[test]
fn serve_says_it_is_a_proof_of_concept_at_start() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::Duration;
    let state = std::env::temp_dir().join(format!("bullbar-cli-serve-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_bullbar"))
        .args([
            "serve",
            "--config",
            &edition("casual"),
            "--listen",
            "127.0.0.1:0",
            "--control",
            "127.0.0.1:0",
            "--state",
            state.to_str().expect("a UTF-8 path"),
        ])
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let stderr = child.stderr.take().expect("stderr");
    let (lines, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let found = loop {
        match got.recv_timeout(Duration::from_secs(20)) {
            Ok(line) if line.contains("proof of concept") => break Some(line),
            Ok(line) => seen.push(line),
            Err(_) => break None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&state);
    let found = found.unwrap_or_else(|| panic!("no notice in: {seen:#?}"));
    assert_eq!(
        found,
        "bullbar: a proof of concept, not a tool to rely on: what it misses is in the README"
    );
}
