# Chat notifier

Posts The Runbook's human events to a chat incoming webhook as
`{"text": "..."}` (Slack, Mattermost, Rocket.Chat and Google Chat accept
this shape):

| Event | Message |
| --- | --- |
| `approval.requested` | Approval requested: *run title* for *recipients* - *link* |
| `approval.escalated` | Approval escalated: ... |
| `approval.expired` | Approval expired: ... |
| `step.blocked` | Step blocked, needs a person: ... (reported *reason*) |

Other events are acknowledged with `204`. Requests must carry a valid
`Runbook-Signature` with a fresh timestamp (`401` otherwise), and events are
deduplicated by ID so a redelivery never posts twice; an event without an
ID is refused (`400`). Handled IDs are kept in memory, so after a restart a
redelivered event can post once more; persist them if that matters. Event
text (titles, reasons, unmapped recipient IDs) is escaped for the chat's
markup, so it cannot inject mentions or links. Set `CHAT_MARKUP`:

- `slack` (default; Slack and Google Chat): escapes `&`, `<`, `>`, so no
  `<!channel>` or disguised `<https://...|link>`;
- `markdown` (Mattermost, Rocket.Chat): backslash-escapes Markdown (no
  `[text](url)` links or emphasis) and breaks `@channel`, `@all`, `@here`
  and other @-mentions with a zero-width space.

Any other value refuses to start. If the chat webhook fails the notifier
answers `502` and The Runbook retries.

Recipients are Runbook user and team IDs. Set `MENTIONS` to a JSON object
mapping IDs to chat handles to turn them into mentions; configured handles
are inserted as given (they are yours, not event data), unmapped IDs are
escaped text.

```sh
export RUNBOOK_WEBHOOK_SECRET=whsec_...
export CHAT_WEBHOOK_URL=https://hooks.slack.com/services/...
export MENTIONS='{"usr_...": "<@U012345>"}'
python3 notifier.py
runbook webhook create --name chat --url https://notifier.example.com/ \
  --event approval.requested --event approval.escalated \
  --event approval.expired --event step.blocked
```

A human event that reaches no subscriber, or whose deliveries all failed, is
shown on the execution as "notification not delivered": an event-log entry
alone never counts as notifying anyone.
