//! SLACK1 stage 1: one incoming-webhook URL per project, plain
//! messages with a link into the app. No OAuth app, no buttons (stage
//! 2). Posting is best-effort beside the durable channels (webhooks and
//! email own retries); a Slack outage never blocks a run.
use crate::{StoreError, catalog::TenantScope, notify::NotifyStore};
use runbook_engine::NotificationIntent;
use std::future::Future;

/// The events a project channel hears about.
pub fn slack_worthy(intent: &NotificationIntent) -> bool {
    match intent.kind.as_str() {
        "approval.requested" | "step.blocked" | "step.reminder" | "step.escalated" => true,
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
    let step = step_title.unwrap_or("A step");
    let link = if intent.link.starts_with("http") {
        intent.link.clone()
    } else {
        format!(
            "{}{}",
            origin.trim_end_matches('/'),
            if intent.link.starts_with('/') {
                intent.link.clone()
            } else {
                format!("/x/{}", intent.execution_id)
            }
        )
    };
    let lead = match intent.kind.as_str() {
        "approval.requested" => format!("Approval needed: {step} in {run}"),
        "step.blocked" => format!("Step handed off: {step} in {run} is blocked"),
        "step.reminder" => format!("Reminder: {step} in {run} is still open"),
        "step.escalated" => format!("Step escalated: {step} in {run}"),
        "execution.completed" => format!("Run failed: {run}"),
        other => format!("{other}: {run}"),
    };
    format!("<{link}|{}>", escape_mrkdwn(&lead))
}

/// POST one Slack message. The error is a short code, never content.
pub trait SlackTransport {
    fn post_slack(&self, url: &str, text: &str) -> impl Future<Output = Result<(), String>>;
}

/// Post this intent to the project's channel when one is configured.
/// Returns whether a message was posted. Best-effort by design: callers
/// log and move on (email and webhooks carry the durable record).
pub async fn fan_out_slack<N: NotifyStore, T: SlackTransport>(
    notify: &N,
    transport: &T,
    scope: &TenantScope,
    intent: &NotificationIntent,
    run_title: &str,
    step_title: Option<&str>,
    origin: &str,
) -> Result<bool, StoreError> {
    if !slack_worthy(intent) {
        return Ok(false);
    }
    let Some(url) = notify.slack_url(scope).await? else {
        return Ok(false);
    };
    let text = slack_text(intent, run_title, step_title, origin);
    transport
        .post_slack(&url, &text)
        .await
        .map_err(|_| StoreError::Backend("slack.post_failed"))?;
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
