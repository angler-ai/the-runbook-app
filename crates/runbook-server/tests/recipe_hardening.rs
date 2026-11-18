//! F71: the webhook recipes under `examples/webhooks/`.
//!
//! - The chat notifier escapes for the chat it posts to: Slack-style
//!   markup (`&`, `<`, `>`) by default, or Markdown (Mattermost,
//!   Rocket.Chat) with `CHAT_MARKUP=markdown`, where @-mentions and links
//!   are the injection vectors. Operator-configured `MENTIONS` handles are
//!   never escaped (they must work); event text always is.
//! - The relay answers a signed body that is valid JSON but not the event
//!   shape (an array, a string, non-object members) with 4xx, and keeps
//!   serving.
//! - The launchers that hold the service token never run mutable image or
//!   action tags.
//!
//! Python tests skip when `python3` is not installed.
#[path = "support/examples.rs"]
mod examples;
use examples::{MockHttp, Program, deliver, example, python};
use runbook_core::webhooks::{encode_webhook_secret, signature_header};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ1";

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signed(url: &str, secret: &str, kind: &str, body: &str) -> u16 {
    let ts = now_s();
    let signature = signature_header(&[secret], ts, body.as_bytes());
    deliver(url, "evt_x", kind, &ts.to_string(), &signature, body).0
}

fn notifier(secret: &str, chat: &MockHttp, extra: &[(&str, String)]) -> Program {
    let mut env = vec![
        ("RUNBOOK_WEBHOOK_SECRET", secret.to_owned()),
        ("CHAT_WEBHOOK_URL", chat.url()),
        ("RELAY_HOST", "127.0.0.1".to_owned()),
        ("RELAY_PORT", "0".to_owned()),
    ];
    env.extend(extra.iter().cloned());
    Program::start("chat-notifier/notifier.py", &env)
}

fn hostile(id: &str) -> Value {
    json!({
        "id": id, "type": "approval.requested",
        "execution": {"id": EXE, "title": "@channel [Approve](https://evil.example) **now** <!here>"},
        "data": {"recipients": [{"id": "usr_alice"}, {"id": "usr_bob"}],
                 "link": "https://runbooks.example/executions/x"},
    })
}

#[test]
fn chat_notifier_escapes_markdown_chats_and_keeps_configured_mentions() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([21; 32]);
    let chat = MockHttp::start(200);
    let program = notifier(
        &secret,
        &chat,
        &[
            ("CHAT_MARKUP", "markdown".to_owned()),
            ("MENTIONS", r#"{"usr_alice": "@alice"}"#.to_owned()),
        ],
    );
    let body = hostile("evt_01JABCDEFGHJKMNPQRSTVWXYZA").to_string();
    assert_eq!(
        signed(&program.url, &secret, "approval.requested", &body),
        200
    );
    let text = chat.seen()[0].json()["text"].as_str().unwrap().to_owned();
    // Event text cannot mention a channel or render a link or emphasis.
    assert!(!text.contains("@channel"), "{text}");
    assert!(!text.contains("](") && !text.contains("**"), "{text}");
    // The configured handle is a real mention; the unmapped ID is inert text.
    assert!(text.contains("for @alice, "), "{text}");
    assert!(
        text.contains("https://runbooks.example/executions/x"),
        "{text}"
    );
}

#[test]
fn chat_notifier_keeps_configured_slack_mentions_working() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([22; 32]);
    let chat = MockHttp::start(200);
    let program = notifier(
        &secret,
        &chat,
        &[("MENTIONS", r#"{"usr_alice": "<@U012345>"}"#.to_owned())],
    );
    let body = hostile("evt_01JABCDEFGHJKMNPQRSTVWXYZB").to_string();
    assert_eq!(
        signed(&program.url, &secret, "approval.requested", &body),
        200
    );
    let text = chat.seen()[0].json()["text"].as_str().unwrap().to_owned();
    assert!(text.contains("<@U012345>"), "{text}");
    assert!(text.contains("&lt;!here&gt;"), "{text}");
    assert_eq!(text.matches('<').count(), 1, "{text}");
}

#[test]
fn chat_notifier_refuses_unknown_markup() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let mut child = std::process::Command::new("python3")
        .arg("-I")
        .arg(example("chat-notifier/notifier.py"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUNBOOK_WEBHOOK_SECRET", "whsec_x")
        .env("CHAT_WEBHOOK_URL", "http://127.0.0.1:9/")
        .env("CHAT_MARKUP", "html")
        .env("RELAY_PORT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // It must refuse to start, not serve with the wrong escaping.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(status.is_some_and(|s| !s.success()), "{status:?}");
}

#[test]
fn relay_answers_wrongly_shaped_signed_bodies_with_4xx_and_keeps_serving() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([23; 32]);
    let github = MockHttp::start(204);
    let program = Program::start(
        "github-actions-relay/relay.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
            ("GITHUB_TOKEN", "ghs_relaytesttoken".to_owned()),
            ("GITHUB_API_URL", github.url()),
            ("GITHUB_REPOSITORY", "acme/api".to_owned()),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ],
    );
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZC";
    for body in [
        json!([]),
        json!("agent.work_available"),
        json!(7),
        json!({"type": "agent.work_available", "id": id, "execution": "exe"}),
        json!({"type": "agent.work_available", "id": id, "execution": {"id": EXE}, "data": []}),
        json!({"type": "agent.work_available", "id": id, "execution": {"id": EXE}, "data": {"params": "x"}}),
    ] {
        let status = signed(
            &program.url,
            &secret,
            "agent.work_available",
            &body.to_string(),
        );
        assert!((400..500).contains(&status), "{body}: {status}");
    }
    assert!(github.seen().is_empty());
    // Still serving: a well-formed event dispatches.
    let good =
        json!({"type": "agent.work_available", "id": id, "execution": {"id": EXE}, "data": {}});
    assert_eq!(
        signed(
            &program.url,
            &secret,
            "agent.work_available",
            &good.to_string()
        ),
        200
    );
    assert_eq!(github.seen().len(), 1);
}

#[test]
fn launchers_that_hold_the_service_token_run_no_mutable_tags() {
    let job = std::fs::read_to_string(example("kubernetes-job/job.yaml")).unwrap();
    for line in job.lines().filter(|l| l.trim_start().starts_with("image:")) {
        assert!(!line.contains(":latest"), "{line}");
        assert!(
            line.contains("@sha256:") || line.contains("__"),
            "an image must be pinned by digest or filled in by the launcher: {line}"
        );
    }
    let readme = std::fs::read_to_string(example("kubernetes-job/README.md")).unwrap();
    assert!(readme.contains("@sha256:"), "{readme}");
    let workflow =
        std::fs::read_to_string(example("github-actions-relay/runbook-resume.yml")).unwrap();
    for line in workflow
        .lines()
        .filter(|l| l.trim_start().starts_with("- uses:"))
    {
        let pin = line
            .split('@')
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert!(
            pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()),
            "actions must be pinned to a full commit SHA: {line}"
        );
    }
    assert!(
        workflow.contains("sha256sum -c"),
        "the CLI download must be checksum-verified"
    );
}
