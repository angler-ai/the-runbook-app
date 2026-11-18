// NOTIFY1: turning one committed notification intent into the email a
// person should get, and whether their preference wants it now, in the
// daily summary, or not at all. Pure planning and rendering: resolving
// recipients to verified addresses, storing the outbox rows and sending
// are hosts' work (the native server and the Worker), like webhooks.
// Templates render both parts from the same facts; every value that came
// from a runbook or a run is escaped in HTML and never logged here.
use runbook_engine::NotificationIntent;
use runbook_model::RecipientRef;
use serde::{Deserialize, Serialize};

pub mod deliver;
pub mod fanout;
pub mod memory;
pub mod store;
pub use store::*;

/// How much email a person wants (PROGRESS NOTIFY1). The default is
/// `All`: people who never set a preference get the mail the feature
/// exists to send, and every email carries the unsubscribe link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailPreference {
    #[default]
    All,
    ApprovalsOnly,
    DailySummary,
    None,
}

/// What the planner decided for one (intent, person) pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailPlan {
    /// Send an individual email now.
    SendNow,
    /// Hold it for the daily summary.
    Digest,
    /// The preference (or the event kind) says no email.
    Drop,
}

/// Intent kinds that mean "your approval is needed or its state changed".
const APPROVAL_KINDS: &[&str] = &[
    "approval.requested",
    "approval.escalated",
    "approval.expired",
];

/// Intent kinds worth an email at all. Everything else in the catalog
/// (step.completed, execution.started, memory.*, agent work) stays in the
/// app and on webhooks; emailing it would train people to ignore mail.
pub fn emailable(kind: &str) -> bool {
    APPROVAL_KINDS.contains(&kind)
        || matches!(
            kind,
            "step.blocked" | "step.reminder" | "step.escalated" | "execution.completed"
        )
}

pub fn plan(preference: EmailPreference, kind: &str) -> EmailPlan {
    if !emailable(kind) {
        return EmailPlan::Drop;
    }
    match preference {
        EmailPreference::All => EmailPlan::SendNow,
        EmailPreference::ApprovalsOnly => {
            if APPROVAL_KINDS.contains(&kind) {
                EmailPlan::SendNow
            } else {
                EmailPlan::Drop
            }
        }
        EmailPreference::DailySummary => EmailPlan::Digest,
        EmailPreference::None => EmailPlan::Drop,
    }
}

/// The facts one email is rendered from. The host resolves names and the
/// run link; nothing here can carry a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailFacts {
    pub recipient_name: String,
    /// e.g. "Acme / Production"; empty when unknown.
    pub place: String,
    pub run_title: String,
    /// The step the email is about, when there is one.
    pub step_title: Option<String>,
    /// Absolute link into the app (the run, usually its step).
    pub link: String,
    /// The one-click unsubscribe URL for this recipient (signed by host).
    pub unsubscribe_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

pub(crate) fn escape_html_pub(value: &str) -> String {
    escape_html(value)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn place_prefix(facts: &EmailFacts) -> String {
    if facts.place.is_empty() {
        String::new()
    } else {
        format!("[{}] ", facts.place)
    }
}

fn frame(
    facts: &EmailFacts,
    subject: String,
    lead: &str,
    detail: Option<&str>,
) -> (String, String, String) {
    let detail_text = detail.map(|d| format!("\n\n{d}")).unwrap_or_default();
    let text = format!(
        "Hi {name},\n\n{lead}{detail}\n\nOpen the run: {link}\n\nYou are getting this because run activity is assigned to you. Unsubscribe: {unsub}",
        name = facts.recipient_name,
        detail = detail_text,
        link = facts.link,
        unsub = facts.unsubscribe_url,
    );
    let detail_html = detail
        .map(|d| format!("<p>{}</p>", escape_html(d)))
        .unwrap_or_default();
    let html = format!(
        "<div style=\"font-family:system-ui,sans-serif;max-width:560px;margin:auto;padding:24px\">\
<p>Hi {name},</p><p>{lead}</p>{detail}\
<p><a href=\"{link}\">Open the run</a></p>\
<p style=\"color:#555;font-size:13px\">You are getting this because run activity is assigned to you. <a href=\"{unsub}\">Unsubscribe</a></p></div>",
        name = escape_html(&facts.recipient_name),
        lead = escape_html(lead),
        detail = detail_html,
        link = escape_html(&facts.link),
        unsub = escape_html(&facts.unsubscribe_url),
    );
    (subject, text, html)
}

/// Render the email for one intent and recipient, or None when the kind
/// is not emailable or a completed run did not fail (a success is not mail).
pub fn render(intent: &NotificationIntent, to: &str, facts: &EmailFacts) -> Option<EmailMessage> {
    let step = facts.step_title.as_deref().unwrap_or("A step");
    let run = if facts.run_title.is_empty() {
        "a run"
    } else {
        facts.run_title.as_str()
    };
    let (subject, text, html) = match intent.kind.as_str() {
        "approval.requested" => frame(
            facts,
            format!("{}Approval needed: {step} in {run}", place_prefix(facts)),
            &format!("Your approval is needed for “{step}” in {run}."),
            Some("Approve or reject it from the run page; the run is waiting on the decision."),
        ),
        "approval.escalated" => frame(
            facts,
            format!(
                "{}Approval escalated to you: {step} in {run}",
                place_prefix(facts)
            ),
            &format!("An approval for “{step}” in {run} was escalated and now needs you."),
            None,
        ),
        "approval.expired" => frame(
            facts,
            format!("{}Approval expired: {step} in {run}", place_prefix(facts)),
            &format!("The approval for “{step}” in {run} expired without a decision."),
            None,
        ),
        "step.blocked" => frame(
            facts,
            format!(
                "{}Step handed off to you: {step} in {run}",
                place_prefix(facts)
            ),
            &format!("“{step}” in {run} is blocked and was handed off to you."),
            intent
                .reason
                .as_deref()
                .map(|_| "Its summary on the run page says what it is waiting on."),
        ),
        "step.reminder" => frame(
            facts,
            format!(
                "{}Reminder: {step} in {run} is still open",
                place_prefix(facts)
            ),
            &format!("“{step}” in {run} is still open and waiting."),
            None,
        ),
        "step.escalated" => frame(
            facts,
            format!(
                "{}Step escalated to you: {step} in {run}",
                place_prefix(facts)
            ),
            &format!("“{step}” in {run} stayed open past its escalation time and now needs you."),
            None,
        ),
        "execution.completed" => {
            let failed = intent
                .detail
                .as_ref()
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                == Some("failed");
            if !failed {
                return None;
            }
            frame(
                facts,
                format!("{}Run failed: {run}", place_prefix(facts)),
                &format!("The run “{run}” failed."),
                Some("The run page shows which step failed and what it recorded."),
            )
        }
        _ => return None,
    };
    Some(EmailMessage {
        to: to.to_owned(),
        subject,
        text,
        html,
    })
}

/// The invitation email (sent at invitation creation, not from an intent).
pub fn render_invitation(
    to: &str,
    organization: &str,
    invited_by: &str,
    sign_in_url: &str,
) -> EmailMessage {
    let subject = format!("You are invited to {organization} on The Runbook");
    let text = format!(
        "Hi there,\n\n{invited_by} invited you to {organization} on The Runbook. Sign in with this email address to accept:\n\n{sign_in_url}\n\nIf you were not expecting this invitation, you can ignore this email."
    );
    let html = format!(
        "<div style=\"font-family:system-ui,sans-serif;max-width:560px;margin:auto;padding:24px\">\
<p>Hi there,</p><p>{} invited you to {} on The Runbook. Sign in with this email address to accept.</p>\
<p><a href=\"{}\">Accept the invitation</a></p>\
<p style=\"color:#555;font-size:13px\">If you were not expecting this invitation, you can ignore this email.</p></div>",
        escape_html(invited_by),
        escape_html(organization),
        escape_html(sign_in_url),
    );
    EmailMessage {
        to: to.to_owned(),
        subject,
        text,
        html,
    }
}

/// One line of the daily summary: what happened, in the person's words.
pub fn digest_line(intent: &NotificationIntent, facts: &EmailFacts) -> Option<String> {
    let message = render(intent, "", facts)?;
    Some(message.subject)
}

/// Whether an intent names this recipient (user ref, or a team the host
/// has already expanded to the user). Hosts expand teams before planning.
pub fn names_recipient(intent: &NotificationIntent, user: &runbook_model::UserId) -> bool {
    intent
        .recipients
        .iter()
        .any(|r| matches!(r, RecipientRef::User(id) if id == user))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(kind: &str) -> NotificationIntent {
        NotificationIntent {
            event_id: "evt_test".into(),
            kind: kind.into(),
            execution_id: "exe_00000000000000000000000001".parse().unwrap(),
            sequence: 1,
            at_ms: 0,
            step_run_ids: vec![],
            recipients: vec![],
            reason: None,
            link: "https://runs.example.test/x/exe_1".into(),
            ready_steps: vec![],
            params: None,
            detail: None,
        }
    }

    fn facts() -> EmailFacts {
        EmailFacts {
            recipient_name: "Ada <script>".into(),
            place: "Acme / Production".into(),
            run_title: "Verify <api>".into(),
            step_title: Some("Sign off".into()),
            link: "https://runs.example.test/x/exe_1?step=signoff".into(),
            unsubscribe_url: "https://runs.example.test/unsubscribe?token=t".into(),
        }
    }

    #[test]
    fn preferences_gate_by_kind() {
        assert_eq!(
            plan(EmailPreference::All, "step.reminder"),
            EmailPlan::SendNow
        );
        assert_eq!(
            plan(EmailPreference::All, "step.completed"),
            EmailPlan::Drop
        );
        assert_eq!(
            plan(EmailPreference::ApprovalsOnly, "approval.requested"),
            EmailPlan::SendNow
        );
        assert_eq!(
            plan(EmailPreference::ApprovalsOnly, "step.blocked"),
            EmailPlan::Drop
        );
        assert_eq!(
            plan(EmailPreference::DailySummary, "execution.completed"),
            EmailPlan::Digest
        );
        assert_eq!(
            plan(EmailPreference::None, "approval.requested"),
            EmailPlan::Drop
        );
    }

    #[test]
    fn approval_mail_names_the_step_and_escapes_html() {
        let message = render(&intent("approval.requested"), "ada@example.test", &facts()).unwrap();
        assert_eq!(
            message.subject,
            "[Acme / Production] Approval needed: Sign off in Verify <api>"
        );
        assert!(
            message
                .text
                .contains("Open the run: https://runs.example.test/x/exe_1?step=signoff")
        );
        assert!(
            message
                .text
                .contains("Unsubscribe: https://runs.example.test/unsubscribe?token=t")
        );
        assert!(message.html.contains("Ada &lt;script&gt;"));
        assert!(!message.html.contains("Verify <api>"));
        assert!(message.html.contains("Verify &lt;api&gt;"));
    }

    #[test]
    fn only_failed_runs_are_mailed() {
        let ok = intent("execution.completed");
        assert!(render(&ok, "a@b.test", &facts()).is_none());
        let mut failed = intent("execution.completed");
        failed.detail = Some(serde_json::json!({"status": "failed"}));
        let message = render(&failed, "a@b.test", &facts()).unwrap();
        assert!(message.subject.contains("Run failed"));
        assert_eq!(
            digest_line(&failed, &facts()).as_deref(),
            Some(message.subject.as_str())
        );
    }

    #[test]
    fn invitations_carry_the_sign_in_link() {
        let message = render_invitation(
            "new@example.test",
            "Acme",
            "Alice",
            "https://runs.example.test/sign-in",
        );
        assert!(message.subject.contains("invited to Acme"));
        assert!(message.text.contains("https://runs.example.test/sign-in"));
    }
}
