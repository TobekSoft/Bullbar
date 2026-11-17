//! Nothing from the environment reaches a runner: no API key, organisation or project the HTTP
//! library would pick up by default, and no proxy that would carry local content off the machine.
//! Its own test binary, since it sets environment variables for the whole process.

mod common;

use bullbar_gateway::server::Options;
use common::{ask, chat, config, fake_runner, gateway};

#[tokio::test]
async fn the_environment_is_not_forwarded() {
    // SAFETY: this binary has one test, and it sets these before starting anything that reads them.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "fake-key-must-not-leak");
        std::env::set_var("OPENAI_ORG_ID", "fake-org-must-not-leak");
        std::env::set_var("OPENAI_PROJECT_ID", "fake-project-must-not-leak");
        std::env::set_var("OPENAI_BASE_URL", "http://203.0.113.9/v1");
        // A proxy nothing listens on: if it were used, the request would fail.
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:9");
        std::env::set_var("http_proxy", "http://127.0.0.1:9");
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:9");
    }
    let (runner, received) = fake_runner().await;
    let gw = gateway(config(runner), Options::default()).await;
    assert_eq!(chat(gw, ask("bullbar/public/easy")).await.status(), 200);
    let received = received.all();
    assert_eq!(received.len(), 1, "the runner wasn't reached directly");
    let (headers, _) = &received[0];
    for (name, value) in headers {
        let value = value.to_str().unwrap_or_default();
        assert!(!value.contains("must-not-leak"), "{name}: {value}");
    }
}
