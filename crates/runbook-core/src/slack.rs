//! SLACK1. Stage 1: one incoming-webhook URL per project, plain messages
//! with a link into the app. Stage 2 (2026-11-17): the operator creates a
//! Slack app from `deploy/slack/manifest.yaml` and gives the server its
//! bot token and signing secret; a project then names a channel, and an
//! approval request arrives with one button per option. A click is
//! verified with the signing secret, the clicking Slack user is matched to
//! a Runbook user by verified email, and the vote is cast with that
//! person's own permissions (see `runbook_api::slack_app`). Posting is
//! best-effort beside the durable channels (webhooks and email own
//! retries); a Slack outage never blocks a run.
use crate::{StoreError, catalog::TenantScope, directory::DirectoryStore, notify::NotifyStore};
use runbook_engine::NotificationIntent;
use std::future::Future;

/// The events a project channel hears about.
pub fn slack_worthy(intent: &NotificationIntent) -> bool {
    match intent.kind.as_str() {
        "approval.requested" | "step.blocked" | "step.reminder" | "step.overdue"
        | "step.escalated" => true,
        "execution.completed" => {
            intent
                .detail
                .as_ref()
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                == Some("failed")
        }
        _ => false,
    }
}

fn run_starter(intent: &NotificationIntent) -> String {
    let detail = intent.detail.as_ref();
    if let Some(parent) = detail.and_then(|d| d["parent_execution_id"].as_str()) {
        return format!("call from {parent}");
    }
    if let Some(name) = detail.and_then(|d| d["starter_name"].as_str()) {
        return name.to_owned();
    }
    match detail
        .and_then(|d| d["created_by"].as_str())
        .and_then(|s| s.split_once(':'))
    {
        Some(("webhook", id)) => format!("webhook {id}"),
        Some(("schedule", id)) => format!("schedule {id}"),
        Some(("service", _)) => "a service token".into(),
        Some(("human" | "user", id)) => format!("user {id}"),
        Some(("agent", id)) => format!("agent sponsored by {id}"),
        _ => "the system".into(),
    }
}

fn escape_mrkdwn(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The Slack `text` for one intent. Titles come from the host (the
/// intent itself carries ids, not words).
pub fn slack_text(
    intent: &NotificationIntent,
    run_title: &str,
    step_title: Option<&str>,
    origin: &str,
) -> String {
    let run = if run_title.is_empty() {
        "a run"
    } else {
        run_title
    };
    let step = step_title
        .or_else(|| {
            intent
                .detail
                .as_ref()
                .and_then(|d| d["step_title"].as_str())
        })
        .or_else(|| intent.detail.as_ref().and_then(|d| d["path"].as_str()))
        .unwrap_or("A step");
    let link = crate::notify::fanout::link_absolute(origin, intent);
    let lead = match intent.kind.as_str() {
        "approval.requested" => format!("Approval needed: {step} in {run}"),
        "step.blocked" => format!("Step handed off: {step} in {run} is blocked"),
        "step.reminder" => format!("Reminder: {step} in {run} is still open"),
        "step.escalated" => format!("Step escalated: {step} in {run}"),
        "step.overdue" if intent.reason.as_deref() == Some("not_open") => {
            format!("Step overdue: {step} in {run} has yet to open")
        }
        "step.overdue" => format!("Step overdue: {step} in {run} is still open"),
        "execution.started" => format!("Run started: {run} (by {})", run_starter(intent)),
        "execution.completed" => format!("Run failed: {run}"),
        other => format!("{other}: {run}"),
    };
    format!("<{link}|{}>", escape_mrkdwn(&lead))
}

/// The server's Slack app (stage 2), from `RUNBOOK_SLACK_BOT_TOKEN`,
/// `RUNBOOK_SLACK_SIGNING_SECRET` and optionally
/// `RUNBOOK_SLACK_DEFAULT_CHANNEL` (for projects that name none).
#[derive(Clone, PartialEq, Eq)]
pub struct SlackApp {
    pub bot_token: String,
    pub signing_secret: String,
    pub default_channel: Option<String>,
}
impl std::fmt::Debug for SlackApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackApp")
            .field("default_channel", &self.default_channel)
            .finish_non_exhaustive()
    }
}
impl SlackApp {
    /// Both secrets set (and the token a bot token), or no app.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let value = |name: &str| {
            get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let bot_token = value("RUNBOOK_SLACK_BOT_TOKEN")?;
        let signing_secret = value("RUNBOOK_SLACK_SIGNING_SECRET")?;
        if !bot_token.starts_with("xoxb-") {
            return None;
        }
        Some(Self {
            bot_token,
            signing_secret,
            default_channel: value("RUNBOOK_SLACK_DEFAULT_CHANNEL").filter(|c| valid_channel(c)),
        })
    }
}

/// How a project's Slack setting is stored: a webhook URL as itself, a
/// channel with this prefix (one column serves both).
pub const CHANNEL_PREFIX: &str = "slack-channel:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlackTarget {
    Webhook(String),
    Channel(String),
}
impl SlackTarget {
    pub fn parse(stored: &str) -> Self {
        match stored.strip_prefix(CHANNEL_PREFIX) {
            Some(channel) => Self::Channel(channel.to_owned()),
            None => Self::Webhook(stored.to_owned()),
        }
    }
    pub fn stored(&self) -> String {
        match self {
            Self::Webhook(url) => url.clone(),
            Self::Channel(channel) => format!("{CHANNEL_PREFIX}{channel}"),
        }
    }
}

/// A Slack channel ID: `C…` (public) or `G…` (private), upper-case
/// letters and digits.
pub fn valid_channel(id: &str) -> bool {
    (id.starts_with('C') || id.starts_with('G'))
        && (9..=24).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// What a button carries back (Slack returns it verbatim, and only on a
/// request signed with the app's secret).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecisionButton {
    pub org: String,
    pub sr: String,
    pub option: String,
}
pub const DECIDE_ACTION: &str = "runbook.decide";

/// The Block Kit message for one intent: the stage-1 line, and for an
/// approval request its prompt and one button per option.
pub fn slack_blocks(
    intent: &NotificationIntent,
    org: &str,
    run_title: &str,
    step_title: Option<&str>,
    origin: &str,
) -> serde_json::Value {
    let detail = intent.detail.as_ref();
    let step_title = step_title.or_else(|| {
        detail
            .and_then(|d| d.get("step_title"))
            .and_then(|t| t.as_str())
    });
    let text = slack_text(intent, run_title, step_title, origin);
    let mut blocks = vec![serde_json::json!({
        "type": "section",
        "text": {"type": "mrkdwn", "text": text},
    })];
    if intent.kind == "approval.requested"
        && let (Some(sr), Some(options)) = (
            intent.step_run_ids.first(),
            detail
                .and_then(|d| d.get("options"))
                .and_then(|o| o.as_array()),
        )
    {
        if let Some(prompt) = detail
            .and_then(|d| d.get("prompt"))
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty())
        {
            blocks.push(serde_json::json!({
                "type": "context",
                "elements": [{"type": "mrkdwn", "text": escape_mrkdwn(prompt)}],
            }));
        }
        let buttons: Vec<serde_json::Value> = options
            .iter()
            .take(5)
            .filter_map(|o| {
                let id = o.get("id")?.as_str()?;
                let title = o.get("title").and_then(|t| t.as_str()).unwrap_or(id);
                let value = serde_json::to_string(&DecisionButton {
                    org: org.to_owned(),
                    sr: sr.to_string(),
                    option: id.to_owned(),
                })
                .ok()?;
                Some(serde_json::json!({
                    "type": "button",
                    "action_id": format!("{DECIDE_ACTION}.{id}"),
                    "text": {"type": "plain_text", "text": title.chars().take(70).collect::<String>()},
                    "value": value,
                }))
            })
            .collect();
        if !buttons.is_empty() {
            blocks.push(serde_json::json!({"type": "actions", "elements": buttons}));
        }
    }
    serde_json::json!({"text": text, "blocks": blocks})
}

/// Slack's request signature (`X-Slack-Signature`, `v0=` and hex) over
/// `v0:<timestamp>:<body>`, within five minutes of `now_s`.
pub fn verify_signature(
    secret: &str,
    timestamp: &str,
    body: &str,
    signature: &str,
    now_s: i64,
) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else {
        return false;
    };
    if (now_s - ts).abs() > 300 {
        return false;
    }
    let mac = crate::webhooks::hmac_sha256(
        secret.as_bytes(),
        &[b"v0:", timestamp.as_bytes(), b":", body.as_bytes()],
    );
    let expected: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    let Some(given) = signature.strip_prefix("v0=") else {
        return false;
    };
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Slack's HTTP calls. Errors are short codes, never content.
pub trait SlackTransport {
    /// POST one stage-1 message to an incoming webhook.
    fn post_slack(&self, url: &str, text: &str) -> impl Future<Output = Result<(), String>>;
    /// A Web API call with the bot token: JSON POST to
    /// `https://slack.com/api/<method>`, or GET with `query` when `body`
    /// is None. Returns the JSON reply when Slack says `ok`.
    fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> impl Future<Output = Result<serde_json::Value, String>>;
}

/// Post this intent to the project's Slack: its webhook (stage 1), or its
/// channel (or the app's default channel) through the app (stage 2).
/// Returns whether a message was posted. Best-effort by design: callers
/// log and move on (email and webhooks carry the durable record).
#[allow(clippy::too_many_arguments)]
pub async fn fan_out_slack<N: NotifyStore, D: DirectoryStore, T: SlackTransport>(
    notify: &N,
    directory: &D,
    transport: &T,
    app: Option<&SlackApp>,
    scope: &TenantScope,
    intent: &NotificationIntent,
    run_title: &str,
    step_title: Option<&str>,
    origin: &str,
) -> Result<bool, StoreError> {
    let run_started = intent.kind == "execution.started";
    if !run_started && !slack_worthy(intent) {
        return Ok(false);
    }
    let settings = notify.slack_settings(scope).await?;
    if run_started
        && !settings
            .as_ref()
            .is_some_and(|settings| settings.notify_run_started)
    {
        return Ok(false);
    }
    let target = match settings {
        Some(settings) => SlackTarget::parse(&settings.target),
        None => match app.and_then(|a| a.default_channel.clone()) {
            Some(channel) => SlackTarget::Channel(channel),
            None => return Ok(false),
        },
    };
    let mut named_intent = None;
    if run_started
        && let Some(detail) = intent
            .detail
            .as_ref()
            .filter(|d| d["parent_execution_id"].is_null())
        && let Some(("human" | "user", id)) = detail["created_by"]
            .as_str()
            .and_then(|s| s.split_once(':'))
        && let Ok(id) = id.parse::<runbook_model::UserId>()
        && let Ok(snapshot) = directory.snapshot(&scope.org_id).await
        && let Some(user) = snapshot.users.get(&id)
    {
        let mut named = intent.clone();
        let mut detail = detail.clone();
        detail["starter_name"] = serde_json::json!(user.name);
        named.detail = Some(detail);
        named_intent = Some(named);
    }
    let intent = named_intent.as_ref().unwrap_or(intent);
    match target {
        SlackTarget::Webhook(url) => {
            let text = slack_text(intent, run_title, step_title, origin);
            transport
                .post_slack(&url, &text)
                .await
                .map_err(|_| StoreError::Backend("slack.post_failed"))?;
        }
        SlackTarget::Channel(channel) => {
            let Some(app) = app else {
                return Ok(false);
            };
            let mut message =
                slack_blocks(intent, scope.org_id.as_str(), run_title, step_title, origin);
            message["channel"] = serde_json::json!(channel);
            message["unfurl_links"] = serde_json::json!(false);
            transport
                .slack_api(&app.bot_token, "chat.postMessage", &[], Some(&message))
                .await
                .map_err(|_| StoreError::Backend("slack.post_failed"))?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(kind: &str) -> NotificationIntent {
        NotificationIntent {
            event_id: "evt_1".into(),
            kind: kind.into(),
            execution_id: "exe_00000000000000000000000001".parse().unwrap(),
            sequence: 1,
            at_ms: 0,
            step_run_ids: vec![],
            recipients: vec![],
            reason: None,
            link: "/x/exe_00000000000000000000000001".into(),
            ready_steps: vec![],
            params: None,
            detail: None,
        }
    }

    #[test]
    fn renders_a_link_and_escapes_titles() {
        let text = slack_text(
            &intent("approval.requested"),
            "Verify <api> & go",
            Some("Sign off"),
            "https://runs.example.test",
        );
        assert!(text.starts_with("<https://runs.example.test/x/exe_"));
        assert!(text.contains("Verify &lt;api&gt; &amp; go"));
    }

    /// A run's stored URL is its API address; people are sent to its page.
    #[test]
    fn links_to_the_run_page_never_the_api() {
        let exe = "exe_00000000000000000000000001";
        let page = format!("<https://runs.example.test/x/{exe}|");
        for link in [
            format!("https://runs.example.test/v1/executions/{exe}"),
            format!("/v1/executions/{exe}"),
            String::new(),
        ] {
            let mut i = intent("approval.requested");
            i.link = link.clone();
            let text = slack_text(&i, "Release", Some("Ship"), "https://runs.example.test/");
            assert!(text.starts_with(&page), "{link}: {text}");
        }
        let mut i = intent("approval.requested");
        i.link = "https://elsewhere.example/doc".into();
        let text = slack_text(&i, "Release", Some("Ship"), "https://runs.example.test");
        assert!(
            text.starts_with("<https://elsewhere.example/doc|"),
            "{text}"
        );
        i.link = format!("/x/{exe}?step=signoff");
        let text = slack_text(&i, "Release", Some("Ship"), "https://runs.example.test");
        assert!(
            text.starts_with(&format!("<https://runs.example.test/x/{exe}?step=signoff|")),
            "{text}"
        );
    }

    #[test]
    fn approval_requests_get_one_button_per_option() {
        let mut i = intent("approval.requested");
        i.step_run_ids = vec!["sr_00000000000000000000000009".parse().unwrap()];
        i.detail = Some(serde_json::json!({
            "step_title": "Ship it",
            "prompt": "Deploy <prod>?",
            "options": [{"id": "yes", "title": "Approve"}, {"id": "no", "title": "Reject"}],
        }));
        let m = slack_blocks(&i, "org_1", "Release", None, "https://r.test");
        assert!(
            m["text"]
                .as_str()
                .unwrap()
                .contains("Approval needed: Ship it in Release")
        );
        assert_eq!(
            m["blocks"][1]["elements"][0]["text"],
            "Deploy &lt;prod&gt;?"
        );
        let buttons = m["blocks"][2]["elements"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[1]["action_id"], "runbook.decide.no");
        let value: DecisionButton =
            serde_json::from_str(buttons[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value.option, "yes");
        assert_eq!(value.org, "org_1");
        // Other kinds: the line only.
        let m = slack_blocks(
            &intent("step.blocked"),
            "org_1",
            "Release",
            None,
            "https://r.test",
        );
        assert_eq!(m["blocks"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn depicts_a_run_started_by_a_webhook() {
        let mut start = intent("execution.started");
        start.detail = Some(serde_json::json!({"created_by": "webhook:deployed"}));
        let text = slack_text(
            &start,
            "Onboard & verify",
            None,
            "https://runs.example.test",
        );
        assert!(text.contains("Run started: Onboard &amp; verify"));
        assert!(text.contains("webhook deployed"));
        assert!(text.starts_with("<https://runs.example.test/x/exe_"));
    }

    #[derive(Default)]
    struct RecordedSlack(std::cell::RefCell<Vec<(String, String)>>);

    impl SlackTransport for RecordedSlack {
        async fn post_slack(&self, url: &str, text: &str) -> Result<(), String> {
            self.0.borrow_mut().push((url.into(), text.into()));
            Ok(())
        }

        async fn slack_api(
            &self,
            _: &str,
            method: &str,
            _: &[(&str, &str)],
            body: Option<&serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            assert_eq!(method, "chat.postMessage");
            let message = body.unwrap();
            self.0.borrow_mut().push((
                message["channel"].as_str().unwrap().into(),
                message["text"].as_str().unwrap().into(),
            ));
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[test]
    fn run_start_opt_in_posts_through_both_slack_destinations() {
        let test = async {
            let scope = TenantScope {
                org_id: "org_00000000000000000000000001".parse().unwrap(),
                project_id: "prj_00000000000000000000000001".parse().unwrap(),
            };
            let notify = crate::notify::memory::MemoryNotify::new(0);
            let directory = crate::directory::memory::MemoryDirectory::default();
            let transport = RecordedSlack::default();
            let app = SlackApp {
                bot_token: "xoxb-test".into(),
                signing_secret: "test".into(),
                default_channel: None,
            };
            let mut started = intent("execution.started");
            started.detail = Some(serde_json::json!({"created_by": "schedule:daily"}));
            let post = async || {
                fan_out_slack(
                    &notify,
                    &directory,
                    &transport,
                    Some(&app),
                    &scope,
                    &started,
                    "Customer onboarding",
                    None,
                    "https://runs.example.test",
                )
                .await
                .unwrap()
            };
            let webhook = "https://hooks.slack.com/services/test";
            let channel = "C0123ABCD";
            notify
                .save_slack_settings(&scope, Some(webhook.into()), None)
                .await
                .unwrap();
            assert!(!post().await);
            notify
                .save_slack_settings(&scope, None, Some(true))
                .await
                .unwrap();
            assert!(post().await);
            notify
                .save_slack_settings(
                    &scope,
                    Some(SlackTarget::Channel(channel.into()).stored()),
                    None,
                )
                .await
                .unwrap();
            assert!(post().await);
            let messages = transport.0.borrow();
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].0, webhook);
            assert_eq!(messages[1].0, channel);
            assert_eq!(messages[0].1, messages[1].1);
            assert!(
                messages[0]
                    .1
                    .contains("Customer onboarding (by schedule daily)")
            );
        };
        assert!(
            std::pin::pin!(test)
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_ready()
        );
    }

    #[test]
    fn signatures_are_checked_with_the_secret_and_the_clock() {
        // Slack's documented example.
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = "token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";
        let sig = "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503";
        assert!(verify_signature(
            secret,
            "1531420618",
            body,
            sig,
            1531420618 + 60
        ));
        assert!(!verify_signature(
            secret,
            "1531420618",
            body,
            sig,
            1531420618 + 301
        ));
        assert!(!verify_signature(
            "other",
            "1531420618",
            body,
            sig,
            1531420618
        ));
        assert!(!verify_signature(
            secret,
            "1531420618",
            &format!("{body}x"),
            sig,
            1531420618
        ));
    }

    #[test]
    fn targets_round_trip_and_channels_are_checked() {
        let channel = SlackTarget::Channel("C0123ABCD".into());
        assert_eq!(SlackTarget::parse(&channel.stored()), channel);
        let hook = SlackTarget::Webhook("https://hooks.slack.com/services/x".into());
        assert_eq!(SlackTarget::parse(&hook.stored()), hook);
        assert!(valid_channel("C0123ABCD"));
        assert!(valid_channel("G01ABCDEFGH"));
        assert!(!valid_channel("#general"));
        assert!(!valid_channel("U0123ABCD"));
        let vars = |name: &str| match name {
            "RUNBOOK_SLACK_BOT_TOKEN" => Some("xoxb-1".to_owned()),
            "RUNBOOK_SLACK_SIGNING_SECRET" => Some("s".to_owned()),
            _ => None,
        };
        assert!(SlackApp::from_vars(vars).is_some());
        assert!(SlackApp::from_vars(|_| None).is_none());
    }

    #[test]
    fn only_the_stage_one_kinds_post() {
        assert!(slack_worthy(&intent("approval.requested")));
        assert!(slack_worthy(&intent("step.blocked")));
        assert!(!slack_worthy(&intent("step.completed")));
        assert!(!slack_worthy(&intent("execution.completed")));
        let mut failed = intent("execution.completed");
        failed.detail = Some(serde_json::json!({"status": "failed"}));
        assert!(slack_worthy(&failed));
    }
}
