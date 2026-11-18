//! C1 recipes under `examples/webhooks/`, run against loopback mocks: the
//! GitHub Actions relay verifies `Runbook-Signature`, rejects tampered and
//! stale deliveries, deduplicates by event ID, and calls `workflow_dispatch`
//! with `execution_id` and `event_id`; the chat notifier formats human
//! events into a chat-webhook POST. Both are Python 3 standard-library
//! programs; these tests skip when `python3` is not installed.
#[path = "support/examples.rs"]
mod examples;
use examples::{MockHttp, Program, deliver, example, python, raw, raw_post};
use runbook_core::webhooks::{encode_webhook_secret, signature_header};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ1";

fn work_available(event_id: &str, repo: &str) -> String {
    json!({
        "id": event_id,
        "type": "agent.work_available",
        "version": 1,
        "created_at": "2026-11-10T12:00:00.000Z",
        "org": "org_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "project": "prj_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "execution": {
            "id": EXE,
            "title": "Deploy api",
            "runbook": "rb_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "runbook_version_id": "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "runbook_version": 3,
            "url": "https://runbooks.example/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ1",
        },
        "sequence": 7,
        "data": {
            "reason": "approval_decided",
            "ready_steps": [{"step_run_id": "sr_01JABCDEFGHJKMNPQRSTVWXYZ0", "path": "rollback", "title": "Roll back"}],
            "params": {"repo": repo, "workflow": "runbook-resume.yml"},
        },
    })
    .to_string()
}

struct Relay {
    secret: String,
    github: MockHttp,
    program: Program,
}
impl Relay {
    fn start(extra: &[(&str, String)]) -> Self {
        let secret = encode_webhook_secret([7; 32]);
        let github = MockHttp::start(204);
        let mut env = vec![
            ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
            ("GITHUB_TOKEN", "ghs_relaytesttoken".to_owned()),
            ("GITHUB_API_URL", github.url()),
            ("GITHUB_REPOSITORY", "acme/api".to_owned()),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ];
        env.extend(extra.iter().cloned());
        let program = Program::start("github-actions-relay/relay.py", &env);
        Self {
            secret,
            github,
            program,
        }
    }
    fn send(&self, event_id: &str, event_type: &str, body: &str) -> (u16, String) {
        let ts = now_s();
        let signature = signature_header(&[&self.secret], ts, body.as_bytes());
        deliver(
            &self.program.url,
            event_id,
            event_type,
            &ts.to_string(),
            &signature,
            body,
        )
    }
}

#[test]
fn relay_verifies_deduplicates_and_dispatches_the_workflow() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let relay = Relay::start(&[]);
    let body = work_available("evt_01JABCDEFGHJKMNPQRSTVWXYZ1", "acme/api");
    let (status, reply) = relay.send(
        "evt_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "agent.work_available",
        &body,
    );
    assert_eq!(status, 200, "{reply}");
    let calls = relay.github.seen();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.method, "POST");
    assert_eq!(
        call.path,
        "/repos/acme/api/actions/workflows/runbook-resume.yml/dispatches"
    );
    assert_eq!(call.header("Authorization"), "Bearer ghs_relaytesttoken");
    assert_eq!(call.header("Accept"), "application/vnd.github+json");
    assert_eq!(
        call.json(),
        json!({"ref": "main", "inputs": {
            "execution_id": EXE,
            "event_id": "evt_01JABCDEFGHJKMNPQRSTVWXYZ1",
        }})
    );
    assert!(!call.body.contains(&relay.secret));

    // The same event again (a redelivery) is acknowledged, not re-dispatched.
    let (status, reply) = relay.send(
        "evt_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "agent.work_available",
        &body,
    );
    assert_eq!(status, 200, "{reply}");
    assert!(reply.contains("duplicate"), "{reply}");
    assert_eq!(relay.github.seen().len(), 1);

    // Tampered body, wrong secret, stale or missing timestamp: 401.
    let ts = now_s();
    let signature = signature_header(&[&relay.secret], ts, body.as_bytes());
    let other = work_available("evt_01JABCDEFGHJKMNPQRSTVWXYZ2", "acme/api");
    let tampered = other.replace("approval_decided", "approval_decidee");
    let good_other = signature_header(&[&relay.secret], ts, other.as_bytes());
    let url = &relay.program.url;
    let evt2 = "evt_01JABCDEFGHJKMNPQRSTVWXYZ2";
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &ts.to_string(),
            &good_other,
            &tampered
        )
        .0,
        401
    );
    let wrong = signature_header(&[&encode_webhook_secret([8; 32])], ts, other.as_bytes());
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &ts.to_string(),
            &wrong,
            &other
        )
        .0,
        401
    );
    let stale = ts - 301;
    let stale_signature = signature_header(&[&relay.secret], stale, other.as_bytes());
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &stale.to_string(),
            &stale_signature,
            &other
        )
        .0,
        401
    );
    assert_eq!(
        deliver(url, evt2, "agent.work_available", "", &good_other, &other).0,
        401
    );
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &ts.to_string(),
            "",
            &other
        )
        .0,
        401
    );
    // A replayed signature over another body does not verify either.
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &ts.to_string(),
            &signature,
            &other
        )
        .0,
        401
    );
    assert_eq!(
        relay.github.seen().len(),
        1,
        "nothing rejected was dispatched"
    );

    // During a rotation overlap the header carries both signatures.
    let rotated = signature_header(
        &[&encode_webhook_secret([9; 32]), &relay.secret],
        ts,
        other.as_bytes(),
    );
    assert_eq!(
        deliver(
            url,
            evt2,
            "agent.work_available",
            &ts.to_string(),
            &rotated,
            &other
        )
        .0,
        200
    );
    assert_eq!(relay.github.seen().len(), 2);

    // Other event types are acknowledged and ignored.
    let started = json!({"id": "evt_01JABCDEFGHJKMNPQRSTVWXYZ3", "type": "execution.started",
        "execution": {"id": EXE}, "data": {}})
    .to_string();
    let (status, _) = relay.send(
        "evt_01JABCDEFGHJKMNPQRSTVWXYZ3",
        "execution.started",
        &started,
    );
    assert_eq!(status, 204);
    assert_eq!(relay.github.seen().len(), 2);
}

#[test]
fn relay_retries_through_github_failures_and_refuses_unlisted_repos() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let relay = Relay::start(&[("RELAY_ALLOWED_REPOS", "acme/api,acme/web".to_owned())]);
    // GitHub fails: the relay answers 5xx so The Runbook retries, and the
    // event is not remembered as handled.
    relay.github.script(500);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZ4";
    let body = work_available(id, "acme/web");
    assert_eq!(relay.send(id, "agent.work_available", &body).0, 502);
    let (status, reply) = relay.send(id, "agent.work_available", &body);
    assert_eq!(status, 200, "{reply}");
    let calls = relay.github.seen();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].path.starts_with("/repos/acme/web/"));
    // A runbook naming a repository outside the allow-list is refused
    // without calling GitHub; so is a malformed workflow name.
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZ5";
    let foreign = work_available(id, "evil/repo");
    assert_eq!(relay.send(id, "agent.work_available", &foreign).0, 403);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZ6";
    let traversal = work_available(id, "acme/api").replace("runbook-resume.yml", "../../x");
    assert_eq!(relay.send(id, "agent.work_available", &traversal).0, 422);
    assert_eq!(relay.github.seen().len(), 2);
}

#[test]
fn relay_remembers_handled_events_across_restarts() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("runbook-relay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = dir.join("seen.txt");
    let _ = std::fs::remove_file(&state);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZ7";
    let body = work_available(id, "acme/api");
    let first = Relay::start(&[("RELAY_DEDUP_FILE", state.display().to_string())]);
    assert_eq!(first.send(id, "agent.work_available", &body).0, 200);
    drop(first);
    let second = Relay::start(&[("RELAY_DEDUP_FILE", state.display().to_string())]);
    let (status, reply) = second.send(id, "agent.work_available", &body);
    assert_eq!(status, 200);
    assert!(reply.contains("duplicate"), "{reply}");
    assert!(second.github.seen().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chat_notifier_formats_human_events_and_ignores_others() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([11; 32]);
    let chat = MockHttp::start(200);
    let program = Program::start(
        "chat-notifier/notifier.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
            (
                "CHAT_WEBHOOK_URL",
                format!("{}/services/T000/B000/x", chat.url()),
            ),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ],
    );
    let send = |id: &str, kind: &str, data: Value| {
        let body = json!({
            "id": id, "type": kind, "version": 1,
            "created_at": "2026-11-10T12:00:00.000Z",
            "org": "org_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "project": "prj_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "execution": {"id": EXE, "title": "Deploy api", "runbook": "rb_01JABCDEFGHJKMNPQRSTVWXYZ0",
                "runbook_version_id": "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0", "runbook_version": 3,
                "url": "https://runbooks.example/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ1"},
            "sequence": 2, "data": data,
        })
        .to_string();
        let ts = now_s();
        let signature = signature_header(&[&secret], ts, body.as_bytes());
        (
            deliver(&program.url, id, kind, &ts.to_string(), &signature, &body).0,
            body,
        )
    };
    let link = "https://runbooks.example/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ1";
    let requested = json!({"step_run_id": "sr_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "recipients": [{"type": "user", "id": "usr_01JABCDEFGHJKMNPQRSTVWXYZ0"}, {"type": "team", "id": "team_01JABCDEFGHJKMNPQRSTVWXYZ0"}],
        "link": link});
    assert_eq!(
        send(
            "evt_01JABCDEFGHJKMNPQRSTVWXYZ8",
            "approval.requested",
            requested.clone()
        )
        .0,
        200
    );
    let blocked = json!({"step_run_id": "sr_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "recipients": [{"type": "user", "id": "usr_01JABCDEFGHJKMNPQRSTVWXYZ0"}],
        "reason": "blocked", "link": link});
    assert_eq!(
        send("evt_01JABCDEFGHJKMNPQRSTVWXYZ9", "step.blocked", blocked).0,
        200
    );
    // Duplicates and other event types post nothing.
    assert_eq!(
        send(
            "evt_01JABCDEFGHJKMNPQRSTVWXYZ8",
            "approval.requested",
            requested
        )
        .0,
        200
    );
    assert_eq!(
        send(
            "evt_01JABCDEFGHJKMNPQRSTVWXYZA",
            "step.completed",
            json!({})
        )
        .0,
        204
    );
    let posts = chat.seen();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(
        posts
            .iter()
            .all(|p| p.method == "POST" && p.path == "/services/T000/B000/x")
    );
    let first = posts[0].json()["text"].as_str().unwrap().to_owned();
    assert!(
        first.contains("Approval requested") && first.contains("Deploy api"),
        "{first}"
    );
    assert!(
        first.contains(link) && first.contains("usr_01JABCDEFGHJKMNPQRSTVWXYZ0"),
        "{first}"
    );
    let second = posts[1].json()["text"].as_str().unwrap().to_owned();
    assert!(
        second.contains("blocked") && second.contains(link),
        "{second}"
    );
    // A forged request is refused before anything is posted.
    let ts = now_s();
    let forged =
        json!({"id": "evt_01JABCDEFGHJKMNPQRSTVWXYZB", "type": "step.blocked"}).to_string();
    let signature = signature_header(&[&encode_webhook_secret([1; 32])], ts, forged.as_bytes());
    assert_eq!(
        deliver(
            &program.url,
            "evt_01JABCDEFGHJKMNPQRSTVWXYZB",
            "step.blocked",
            &ts.to_string(),
            &signature,
            &forged
        )
        .0,
        401
    );
    assert_eq!(chat.seen().len(), 2);
}

#[test]
fn examples_ship_with_readmes_and_a_per_execution_concurrency_group() {
    for dir in [
        "github-actions-relay",
        "kubernetes-job",
        "chat-notifier",
        "",
    ] {
        let readme = example(dir).join("README.md");
        assert!(readme.is_file(), "{}", readme.display());
    }
    let workflow =
        std::fs::read_to_string(example("github-actions-relay/runbook-resume.yml")).unwrap();
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("execution_id:") && workflow.contains("event_id:"));
    assert!(
        workflow.contains("group: runbook-${{ inputs.execution_id }}"),
        "{workflow}"
    );
    assert!(
        workflow.contains("runbook run token \"$EXECUTION_ID\""),
        "{workflow}"
    );
    // Inputs reach shell scripts only through env, never by interpolation,
    // and secrets are never echoed.
    for line in workflow.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("run:") || !trimmed.contains(':') {
            assert!(
                !trimmed.contains("${{"),
                "interpolated into a script: {line}"
            );
        }
        assert!(
            !(trimmed.contains("echo") && trimmed.contains("secrets.")),
            "{line}"
        );
    }
    let job = std::fs::read_to_string(example("kubernetes-job/job.yaml")).unwrap();
    assert!(job.contains("kind: Job") && job.contains("runbook run token"));
    assert!(job.contains("secretKeyRef"));
}

/// Review I1: `workflow` and `ref` also come from the runbook's params, so
/// both are allow-listed like the repository.
#[test]
fn relay_refuses_workflows_and_refs_outside_the_allow_lists() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let relay = Relay::start(&[]);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZC";
    let deploy = work_available(id, "acme/api").replace("runbook-resume.yml", "deploy-prod.yml");
    assert_eq!(relay.send(id, "agent.work_available", &deploy).0, 403);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZD";
    let mut on_ref: Value = serde_json::from_str(&work_available(id, "acme/api")).unwrap();
    on_ref["data"]["params"]["ref"] = json!("release");
    assert_eq!(
        relay
            .send(id, "agent.work_available", &on_ref.to_string())
            .0,
        403
    );
    assert!(relay.github.seen().is_empty(), "GitHub was not called");
    // Owners widen the lists explicitly.
    let wide = Relay::start(&[
        (
            "RELAY_ALLOWED_WORKFLOWS",
            "runbook-resume.yml,deploy-prod.yml".to_owned(),
        ),
        ("RELAY_ALLOWED_REFS", "main,release".to_owned()),
    ]);
    assert_eq!(
        wide.send(id, "agent.work_available", &on_ref.to_string()).0,
        200
    );
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXYZE";
    let deploy = work_available(id, "acme/api").replace("runbook-resume.yml", "deploy-prod.yml");
    assert_eq!(wide.send(id, "agent.work_available", &deploy).0, 200);
    let calls = wide.github.seen();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].json()["ref"], "release");
    assert!(calls[1].path.ends_with("/deploy-prod.yml/dispatches"));
}

/// Review M3: malformed lengths, non-ASCII timestamps and signatures are
/// refused promptly instead of stalling or crashing the handler.
#[test]
fn receivers_reject_malformed_requests_without_stalling() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let relay = Relay::start(&[]);
    let chat = MockHttp::start(200);
    let notifier = Program::start(
        "chat-notifier/notifier.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", encode_webhook_secret([11; 32])),
            ("CHAT_WEBHOOK_URL", chat.url()),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ],
    );
    let body = work_available("evt_01JABCDEFGHJKMNPQRSTVWXYZF", "acme/api");
    let ts = now_s().to_string();
    let good = signature_header(&[&relay.secret], now_s(), body.as_bytes());
    for url in [&relay.program.url, &notifier.url] {
        // A negative length must not read to EOF; garbage is a 400.
        assert_eq!(
            raw(url, &raw_post("-1", ts.as_bytes(), good.as_bytes(), &body)),
            Some(400),
            "{url}"
        );
        assert_eq!(
            raw(url, &raw_post("ten", ts.as_bytes(), good.as_bytes(), &body)),
            Some(400),
            "{url}"
        );
        assert_eq!(
            raw(
                url,
                &raw_post("99999999", ts.as_bytes(), good.as_bytes(), "")
            ),
            Some(413),
            "{url}"
        );
        // Unicode digits (latin-1 superscripts) and non-ASCII signatures.
        assert_eq!(
            raw(
                url,
                &raw_post(&body.len().to_string(), b"\xb2\xb3", good.as_bytes(), &body)
            ),
            Some(401),
            "{url}"
        );
        let mut accented = b"v1=\xe9".to_vec();
        accented.extend_from_slice(&good.as_bytes()[4..]);
        assert_eq!(
            raw(
                url,
                &raw_post(&body.len().to_string(), ts.as_bytes(), &accented, &body)
            ),
            Some(401),
            "{url}"
        );
    }
    assert!(relay.github.seen().is_empty() && chat.seen().is_empty());
    // Both still serve after all that.
    assert_eq!(
        relay
            .send(
                "evt_01JABCDEFGHJKMNPQRSTVWXYZF",
                "agent.work_available",
                &body
            )
            .0,
        200
    );
}

/// Review M4: untrusted text cannot inject chat markup, and events without
/// an ID are refused (they could not be deduplicated).
#[test]
fn chat_notifier_escapes_markup_and_requires_an_event_id() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([12; 32]);
    let chat = MockHttp::start(200);
    let program = Program::start(
        "chat-notifier/notifier.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
            ("CHAT_WEBHOOK_URL", chat.url()),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ],
    );
    let send = |body: Value| {
        let body = body.to_string();
        let ts = now_s();
        let signature = signature_header(&[&secret], ts, body.as_bytes());
        deliver(
            &program.url,
            "evt_x",
            "step.blocked",
            &ts.to_string(),
            &signature,
            &body,
        )
        .0
    };
    let hostile = json!({
        "id": "evt_01JABCDEFGHJKMNPQRSTVWXYZG", "type": "step.blocked",
        "execution": {"id": EXE, "title": "<!channel> <https://evil.example|Approve> & co"},
        "data": {"reason": "<@U999>", "link": "https://runbooks.example/x"},
    });
    assert_eq!(send(hostile), 200);
    let text = chat.seen()[0].json()["text"].as_str().unwrap().to_owned();
    assert!(!text.contains('<') && !text.contains('>'), "{text}");
    assert!(
        text.contains("&lt;!channel&gt;") && text.contains("&amp; co"),
        "{text}"
    );
    assert!(text.contains("&lt;@U999&gt;"), "{text}");
    for id in [json!(""), Value::Null] {
        let anonymous = json!({"id": id, "type": "step.blocked",
            "execution": {"id": EXE, "title": "x"}, "data": {}});
        assert_eq!(send(anonymous), 400);
    }
    assert_eq!(chat.seen().len(), 1);
}

/// Review M5: the resume workflow does not leave credentials where the
/// agent can read them, and pins what it runs.
#[test]
fn resume_workflow_keeps_credentials_from_the_agent_and_pins_tools() {
    let workflow =
        std::fs::read_to_string(example("github-actions-relay/runbook-resume.yml")).unwrap();
    assert!(
        workflow.contains("persist-credentials: false"),
        "{workflow}"
    );
    let checkout = workflow
        .lines()
        .find(|l| l.contains("actions/checkout@"))
        .unwrap();
    let pin = checkout
        .split('@')
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert!(
        pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()),
        "{checkout}"
    );
    assert!(
        !workflow.contains("npx -y @anthropic-ai/claude-code "),
        "unpinned agent"
    );
    assert!(
        workflow.contains("@anthropic-ai/claude-code@"),
        "{workflow}"
    );
    // The credential store lives outside the workspace and the agent may
    // only read inside it.
    assert!(workflow.contains("XDG_CONFIG_HOME: ${{ runner.temp }}/runbook-config"));
    assert!(workflow.contains("Read(./**)"), "{workflow}");
    assert!(
        !workflow.contains("\"Read\""),
        "unrestricted Read: {workflow}"
    );
    let job = std::fs::read_to_string(example("kubernetes-job/job.yaml")).unwrap();
    assert!(
        job.contains("Read(./**)") && !job.contains("\"Read\""),
        "{job}"
    );
    let readme = std::fs::read_to_string(example("kubernetes-job/README.md")).unwrap();
    assert!(readme.contains("two pods"), "{readme}");
}
