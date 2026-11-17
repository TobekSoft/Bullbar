//! Routes that leave the local layer, carried out against a fake provider on loopback. Nothing here
//! reaches a real provider.

mod common;

use bullbar_gateway::server::{Options, router};
use common::{
    CLOUD_SLOTS, FAKE_PROVIDER_KEY, chat, cloud_config, fake_provider, fake_runner, fake_scrubber,
    gateway, key_file, no_hold, refusing_provider, with_scrubber,
};
use serde_json::{Value, json};

const FAKE_AWS: &str = "AKIAFAKEFAKEFAKEFAKE";

fn asking(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

fn content(body: &Value) -> String {
    body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn quoted(path: &std::path::Path) -> String {
    format!("{:?}", path.display().to_string())
}

#[tokio::test]
async fn hard_public_work_is_answered_by_the_provider() {
    let (runner, local) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("answered");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let gw = gateway(config, Options::default()).await;
    let response = chat(
        gw,
        asking("bullbar/public/hard", "what is six times seven?"),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["model"], "bullbar/public/hard");
    assert_eq!(
        content(&body),
        "you said: what is six times seven?\n\n[Bullbar] answered by provider-a at provider-no-training / top"
    );
    let sent = cloud.all();
    assert_eq!(sent.len(), 1);
    let (headers, sent_body) = &sent[0];
    assert_eq!(sent_body["model"], "cloud-top");
    assert_eq!(
        headers["authorization"].to_str().unwrap(),
        format!("Bearer {FAKE_PROVIDER_KEY}")
    );
    assert!(local.all().is_empty());
}

#[tokio::test]
async fn stripped_credentials_leave_as_placeholders_and_stay_that_way() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("stripped");
    let config = cloud_config(
        runner,
        provider,
        &quoted(&key),
        CLOUD_SLOTS,
        Some("strip_and_send = true"),
    );
    let gw = gateway(config, Options::default()).await;
    let response = chat(
        gw,
        asking("bullbar/public/hard", &format!("why does {FAKE_AWS} fail?")),
    )
    .await;
    let text = content(&response.json().await.unwrap());
    // The provider saw the placeholder; the answer keeps it (ADR 0009), and says what it stands for.
    assert!(!cloud.all()[0].1.to_string().contains(FAKE_AWS));
    assert!(
        text.starts_with("you said: why does [CREDENTIAL_1] fail?"),
        "{text}"
    );
    assert!(
        text.contains("[CREDENTIAL_1] stands for a token you sent"),
        "{text}"
    );
    assert!(!text.contains(FAKE_AWS), "{text}");
}

#[tokio::test]
async fn the_placeholder_note_shows_even_without_the_footer() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("nofooter");
    let config = cloud_config(
        runner,
        provider,
        &quoted(&key),
        CLOUD_SLOTS,
        Some("strip_and_send = true"),
    );
    let gw = gateway(
        config,
        Options {
            footer: false,
            ..Options::default()
        },
    )
    .await;
    let text = content(
        &chat(
            gw,
            asking("bullbar/public/hard", &format!("why {FAKE_AWS}?")),
        )
        .await
        .json()
        .await
        .unwrap(),
    );
    assert!(!text.contains("answered by"), "{text}");
    assert!(text.contains("[CREDENTIAL_1] stands for"), "{text}");
}

#[tokio::test]
async fn a_route_needing_protections_not_built_is_not_sent() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("unbuilt");
    // Personal work's protections are all built (chunk 3). Sensitive work also wants an exposure
    // ledger, which isn't: with its slot installed, the engine asks for it.
    let slots = SCRUBBING_SLOTS.replace(
        "\"pii-scrubber\"]",
        "\"pii-scrubber\", \"federation\", \"exposure-ledger\"]",
    );
    let config = cloud_config(runner, provider, &quoted(&key), &slots, None);
    let gw = with_scrubber(config, Options::default(), fake_scrubber(&["Imogen"])).await;
    let response = chat(
        gw.chat,
        asking("bullbar/sensitive/hard", "my neighbour Imogen said..."),
    )
    .await;
    let text = content(&response.json().await.unwrap());
    // Only what isn't built is named as needed.
    assert!(text.contains("which needs exposure ledger."), "{text}");
    assert!(text.contains("nothing was sent"), "{text}");
    assert!(cloud.all().is_empty());
}

const SCRUBBING_SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
    "credential-scanner", "local-runner", "provider-adapters", "terms-register", "pii-scrubber"]
    pii_scrubber_grade = "entities""#;

#[tokio::test]
async fn a_scrubber_configured_but_not_built_counts_as_none() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("unbuilt-scrubber");
    let config = cloud_config(runner, provider, &quoted(&key), SCRUBBING_SLOTS, None);
    let started = router(config.clone(), Options::default(), common::wiring())
        .await
        .unwrap();
    assert!(
        started
            .notices
            .iter()
            .any(|n| n.contains("configured for entities but built for patterns")),
        "{:?}",
        started.notices
    );
    // Personal work then has nowhere to go but local: hard work asks to settle for the local model.
    let gw = gateway(config, no_hold()).await;
    let response = chat(
        gw,
        asking("bullbar/personal/hard", "my neighbour Imogen said..."),
    )
    .await;
    assert_eq!(
        response.headers()["x-bullbar-route"].to_str().unwrap(),
        "ask"
    );
    assert!(cloud.all().is_empty());
}

const UNBUILT_SLOTS: &str = r#"installed = ["decision-engine", "outbound-check", "shutters", "policy",
    "credential-scanner", "local-runner", "provider-adapters", "classifier", "answer-check"]"#;

#[tokio::test]
async fn a_slot_declared_but_not_built_is_named_at_start() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("unbuilt-slots");
    let config = cloud_config(runner, provider, &quoted(&key), UNBUILT_SLOTS, None);
    let started = router(config, Options::default(), common::wiring())
        .await
        .unwrap();
    let unbuilt: Vec<&String> = started
        .notices
        .iter()
        .filter(|n| n.contains("declared in slots.toml but not built"))
        .collect();
    assert_eq!(unbuilt.len(), 2, "{:?}", started.notices);
    assert!(
        unbuilt
            .iter()
            .any(|n| n.starts_with("the classifier ") && n.contains("model name")),
        "{unbuilt:?}"
    );
    assert!(
        unbuilt.iter().any(|n| n.starts_with("the answer check ")),
        "{unbuilt:?}"
    );
}

#[tokio::test]
async fn the_terms_register_declared_says_the_rows_are_set_by_hand() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("unbuilt-terms");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let started = router(config, Options::default(), common::wiring())
        .await
        .unwrap();
    assert_eq!(started.notices.len(), 1, "{:?}", started.notices);
    assert!(
        started.notices[0].starts_with("the terms register ")
            && started.notices[0].contains("set by hand"),
        "{:?}",
        started.notices
    );
}

#[tokio::test]
async fn an_unreadable_key_file_leaves_the_account_out_and_says_so() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let missing = std::env::temp_dir().join("bullbar-test-no-such-key-file");
    let config = cloud_config(runner, provider, &quoted(&missing), CLOUD_SLOTS, None);
    let started = router(config.clone(), Options::default(), common::wiring())
        .await
        .unwrap();
    let left_out: Vec<&String> = started
        .notices
        .iter()
        .filter(|n| n.contains("left out"))
        .collect();
    assert_eq!(left_out.len(), 1, "{:?}", started.notices);
    assert!(
        left_out[0].contains("`provider-a` left out"),
        "{:?}",
        started.notices
    );
    // Without it, hard work has nowhere to go but to ask about the local model.
    let gw = gateway(config, no_hold()).await;
    let response = chat(
        gw,
        asking("bullbar/public/hard", "what is six times seven?"),
    )
    .await;
    assert_ne!(
        response.headers()["x-bullbar-route"].to_str().unwrap(),
        "provider-a at provider-no-training / top"
    );
    assert!(cloud.all().is_empty());
}

#[tokio::test]
async fn the_key_never_reaches_the_client() {
    let (runner, _) = fake_runner().await;
    // The provider is down: the error the client gets must not carry the key either.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let provider = dead.local_addr().unwrap();
    drop(dead);
    let key = key_file("down");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let gw = gateway(config, Options::default()).await;
    let response = chat(gw, asking("bullbar/public/hard", "hello")).await;
    assert_eq!(response.status(), 502);
    let headers = format!("{:?}", response.headers());
    let body = response.text().await.unwrap();
    assert!(
        body.starts_with("{\"error\":{\"message\":\"[Bullbar] provider-a didn't answer"),
        "{body}"
    );
    for shown in [&headers, &body] {
        assert!(!shown.contains(FAKE_PROVIDER_KEY), "{shown}");
    }
}

#[tokio::test]
async fn a_cloud_account_over_plain_http_off_this_machine_stops_the_gateway() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("plain");
    let mut config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    for account in &mut config.accounts {
        if account.id.as_str() == "provider-a"
            && let Some(address) = &mut account.address
        {
            address.base_url = "http://provider-a.example/v1".to_owned();
        }
    }
    let err = router(config, Options::default(), common::wiring())
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("isn't https"), "{err}");
}

#[tokio::test]
async fn a_credential_the_scanner_cannot_strip_is_blocked() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("blocked");
    let config = cloud_config(
        runner,
        provider,
        &quoted(&key),
        CLOUD_SLOTS,
        Some("strip_and_send = true"),
    );
    let gw = gateway(config, Options::default()).await;
    // In a field name, where stripping doesn't reach; the outbound check stops it.
    let mut body = asking("bullbar/public/hard", "hello");
    body[FAKE_AWS] = json!(true);
    let response = chat(gw, body).await;
    assert_eq!(response.headers()["x-bullbar-route"], "blocked");
    let text = content(&response.json().await.unwrap());
    assert!(
        text.starts_with("[Bullbar] blocked: the outbound check found a token"),
        "{text}"
    );
    assert!(!text.contains(FAKE_AWS), "{text}");
    assert!(cloud.all().is_empty());
}

#[tokio::test]
async fn a_refusal_says_what_kind_never_what_the_provider_wrote() {
    let (runner, _) = fake_runner().await;
    let provider = refusing_provider(400).await;
    let key = key_file("refused");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let gw = gateway(config, Options::default()).await;
    for stream in [false, true] {
        let mut body = asking("bullbar/public/hard", "what is sixty-four squared?");
        body["stream"] = json!(stream);
        body["seed"] = json!(20);
        body["temperature"] = json!(0.7);
        let response = chat(gw, body).await;
        assert_eq!(response.status(), 502);
        let text = response.text().await.unwrap();
        assert!(text.contains("provider-a didn't answer"), "{text}");
        assert!(text.contains("HTTP 400"), "{text}");
        assert!(text.contains("model_not_found"), "{text}");
        // What the request carried, by name, so a setting the provider refuses can be found.
        assert!(text.contains("sent: "), "{text}");
        assert!(
            text.contains("seed") && text.contains("temperature"),
            "{text}"
        );
        assert!(text.contains("messages (user)"), "{text}");
        // The provider's message can quote the request: never passed on.
        assert!(!text.contains("sixty-four"), "{text}");
    }
}

#[tokio::test]
async fn only_the_chat_apis_own_fields_leave_and_the_rest_are_named() {
    let (runner, local) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("known-fields");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let gw = gateway(config, Options::default()).await;
    // As a chat client sends a second turn: its own settings, an identifier, and the provider's
    // earlier reasoning handed back (refused with a 400 in the chunk 3 hand check).
    let body = |model: &str, stream: bool| {
        json!({
            "model": model, "stream": stream, "temperature": 0.7, "top_k": 20, "user": "fake-user-7",
            "store": true, "metadata": { "client": "fake" },
            "messages": [
                { "role": "user", "content": "what is six times seven?", "name": "Imogen" },
                { "role": "assistant", "content": "42", "reasoning_content": "six sevens" },
                { "role": "user", "content": "and eight?" }
            ]
        })
    };
    for stream in [false, true] {
        let response = chat(gw, body("bullbar/public/hard", stream)).await;
        let text = response.text().await.unwrap();
        assert!(
            text.contains("not sent (not in the chat API): messages.name, messages.reasoning_content, metadata, store, top_k, user"),
            "{text}"
        );
    }
    for (_, sent) in cloud.all() {
        for gone in ["top_k", "user", "store", "metadata"] {
            assert!(sent.get(gone).is_none(), "{gone} left: {sent}");
        }
        assert_eq!(sent["temperature"], 0.7);
        for message in sent["messages"].as_array().unwrap() {
            let fields: Vec<&String> = message.as_object().unwrap().keys().collect();
            assert!(
                fields.iter().all(|f| *f == "role" || *f == "content"),
                "{message}"
            );
        }
    }
    // The local runner is ours: it gets the request as the client wrote it.
    chat(gw, body("bullbar/public/easy", false)).await;
    assert_eq!(local.all()[0].1["top_k"], 20);
}

/// Bullbar's footer names originals for the user. A client that copies the transcript into a new
/// request (a title request) must not send them on, even where the scrubber wouldn't find them
/// again (chunk 3 hand check).
#[tokio::test]
async fn a_footer_copied_into_a_new_request_never_leaves() {
    let (runner, _) = fake_runner().await;
    let (provider, cloud) = fake_provider().await;
    let key = key_file("copied-footer");
    let config = cloud_config(runner, provider, &quoted(&key), SCRUBBING_SLOTS, None);
    // A scrubber that finds nothing: only the footer stripping stands between the name and out.
    let gw = with_scrubber(config, Options::default(), fake_scrubber(&[])).await;
    let title = "Summarize this conversation.\n\nUser: a card please\n\nAssistant: Happy birthday!\
        \n\n[Bullbar] answered by provider-a at provider-no-training / top\
        \nnot in the answer: Imogen Fakeworth\n\nUser: shorter";
    let response = chat(gw.chat, asking("bullbar/personal/hard", title)).await;
    assert_eq!(response.status(), 200);
    let sent = cloud.all()[0].1.to_string();
    assert!(!sent.contains("Fakeworth"), "{sent}");
    assert!(
        sent.contains("Happy birthday!") && sent.contains("User: shorter"),
        "{sent}"
    );
}

/// Footer lines end in two spaces, Markdown's line break: a client that renders Markdown would
/// otherwise run them into one line (seen in the chunk 3 hand check). No blank line between them,
/// so the footer stays one block and is stripped as one when sent back.
#[tokio::test]
async fn footer_lines_keep_their_breaks_in_markdown() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("footer-breaks");
    let config = cloud_config(
        runner,
        provider,
        &quoted(&key),
        CLOUD_SLOTS,
        Some("strip_and_send = true"),
    );
    let gw = gateway(config, Options::default()).await;
    for stream in [false, true] {
        let mut body = asking("bullbar/public/hard", &format!("why {FAKE_AWS}?"));
        body["stream"] = json!(stream);
        let text = chat(gw, body).await.text().await.unwrap();
        assert!(
            text.contains("provider-no-training / top  \\n[CREDENTIAL_1] stands for"),
            "{text}"
        );
    }
}

/// The provider's reasoning doesn't reach the client: it is in the provider's words, with the
/// surrogates and placeholders the provider saw, and a chat client sends it back on the next turn
/// (Jan as `reasoning_content`, seen 2026-11-17). Only the answer's own fields are relayed; the
/// trace still shows the reasoning (decided 2026-11-17, ADR 0017).
#[tokio::test]
async fn the_providers_reasoning_is_not_relayed() {
    let (runner, _) = fake_runner().await;
    let (provider, _) = fake_provider().await;
    let key = key_file("reasoning");
    let config = cloud_config(runner, provider, &quoted(&key), CLOUD_SLOTS, None);
    let gw = gateway(config, Options::default()).await;
    for stream in [false, true] {
        let mut body = asking("bullbar/public/hard", "what is six times seven?");
        body["stream"] = json!(stream);
        let text = chat(gw, body).await.text().await.unwrap();
        // A stream carries the answer in three-letter pieces.
        let answer = if stream {
            r#""content":"you""#
        } else {
            "you said:"
        };
        assert!(text.contains(answer), "the answer came: {text}");
        assert!(!text.contains("thinking about"), "stream {stream}: {text}");
        assert!(!text.contains("\"reasoning\""), "stream {stream}: {text}");
    }
}
