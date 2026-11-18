# Webhook recipes

The Runbook ships one generic outbound webhook system and no vendor
integrations (PRD section 9). These are small, owner-maintained examples
you copy, adapt and run yourself:

| Folder | Event | What it does |
| --- | --- | --- |
| [`github-actions-relay`](github-actions-relay/) | `agent.work_available` | Verifies the delivery and starts `runbook-resume.yml` with `workflow_dispatch` (`execution_id`, `event_id`); the workflow mints execution-scoped credentials and resumes an agent. |
| [`kubernetes-job`](kubernetes-job/) | `agent.work_available` | A Job manifest (and launcher notes) that runs one agent session per event. |
| [`chat-notifier`](chat-notifier/) | `approval.*`, `step.blocked` | Posts human events to a chat incoming webhook. |

Every receiver must:

1. **Verify the signature.** `Runbook-Signature: v1=<hex HMAC-SHA256 of
   "{Runbook-Timestamp}.{raw body}">`, keyed by the subscription's `whsec_`
   secret. During a 24-hour secret rotation the header carries one
   comma-separated `v1=` entry per secret; accept a match on any.
2. **Reject stale timestamps** more than five minutes from your clock.
3. **Deduplicate on the event ID** (`Runbook-Event-Id`, also the body's
   `id`). Delivery is at least once and unordered; `sequence` orders one
   execution's events.
4. **Answer quickly.** A 2xx within 10 seconds counts as delivered;
   timeouts, 5xx, 408 and 429 are retried with backoff for 24 hours; other
   4xx fail at once. Answer 5xx when your own downstream call failed so the
   event comes back.

Payloads never contain credentials. A launcher mints execution-scoped
credentials with its own service token (`runbook run token <exe>`) and gives
the agent only those.

The Python examples use only the standard library (Python 3.9+). They are
exercised by `crates/runbook-server/tests/webhook_examples.rs` against
loopback mocks (skipped when `python3` is missing).
