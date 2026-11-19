<!--
Every webhook event, with an example delivery body.

scripts/generate.mjs combines this file with the event catalog in
crates/runbook-core/src/webhooks.rs (through `runbook __docs-reference`) and
writes src/content/docs/reference/webhook-events.mdx. Generation fails when
the catalog has an event with no entry here or an entry names an event the
catalog doesn't have. scripts/tests checks every example body: valid JSON,
the envelope's keys, and `type` equal to its heading.

The `data` shapes follow `envelope()` in
crates/runbook-core/src/webhooks/delivery.rs and the intents in
crates/runbook-engine/src/notification.rs.

Format: `# Section`, then `## <event type>`, Markdown, and one ```json body.
-->

# Agents

## agent.work_available
Steps an agent may claim became ready: at the start of a run, after an
approval was decided, after a wait ended, or when an agent's lease lapsed.
Subscribe a launcher to it to start (or wake) an agent. `data.reason` says
why (`initial`, `approval_decided`, `timer`, `signal`, `auto_decision`,
`lease_expired` or `cleanup`); `data.params` is the runbook's
`spec.agents.params`, passed through untouched.

```json
{
  "id": "evt_01JD2X6Q2C4V7T9M3K8B5N1R0S",
  "type": "agent.work_available",
  "version": 1,
  "created_at": "2026-11-18T09:30:12.418Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 4,
  "data": {
    "reason": "initial",
    "ready_steps": [
      { "step_run_id": "sr_01JD2X6Q1B3N5P7R9T1V3X5Z7A", "path": "check_health", "title": "Check health" }
    ],
    "params": { "repo": "acme/api", "workflow": "runbook-resume.yml" }
  }
}
```

# Approvals

## approval.requested
An approval opened and is waiting for its approvers. `recipients` are the
people it was sent to (teams are expanded to their members).

```json
{
  "id": "evt_01JD2X9A7F0H2K4M6P8R0T2V4X",
  "type": "approval.requested",
  "version": 1,
  "created_at": "2026-11-18T09:41:03.077Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 11,
  "data": {
    "step_run_id": "sr_01JD2X9A6E9G1J315N7Q9S1V3W",
    "recipients": [
      { "type": "user", "id": "usr_01JC0000000000000000000A1C" },
      { "type": "user", "id": "usr_01JC0000000000000000000B0B" }
    ],
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  }
}
```

## approval.escalated
An approval reached its `timeout` undecided and escalated. `recipients`
are the people it now goes to; `targets` the configured `escalate_to`
(teams and users).

```json
{
  "id": "evt_01JD2YB4C6E8G0J214N6Q8S0V2",
  "type": "approval.escalated",
  "version": 1,
  "created_at": "2026-11-18T10:11:03.512Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 14,
  "data": {
    "step_run_id": "sr_01JD2X9A6E9G1J315N7Q9S1V3W",
    "recipients": [{ "type": "user", "id": "usr_01JC0000000000000000000CAR" }],
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "targets": [{ "type": "team", "id": "team_01JC00000000000000000001DS" }]
  }
}
```

## approval.expired
An escalated approval reached its `escalation_timeout` undecided. The run
pauses or the step fails, by `on_expired`.

```json
{
  "id": "evt_01JD30C1D3F5H7K9M1P3R5T7V9",
  "type": "approval.expired",
  "version": 1,
  "created_at": "2026-11-18T12:11:03.640Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 17,
  "data": {
    "step_run_id": "sr_01JD2X9A6E9G1J315N7Q9S1V3W",
    "recipients": [{ "type": "user", "id": "usr_01JC0000000000000000000CAR" }],
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "targets": [{ "type": "team", "id": "team_01JC00000000000000000001DS" }]
  }
}
```

## approval.decided
An approval was decided by its approvers. `option` is the chosen option's
ID; `signers` the people whose decisions counted (as many as `min`).
Automatic timeout decisions don't send this event.

```json
{
  "id": "evt_01JD2XA2B4D6F8H0K2M4P6R8T0",
  "type": "approval.decided",
  "version": 1,
  "created_at": "2026-11-18T09:52:40.233Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 12,
  "data": {
    "option": "keep",
    "decided_by": "usr_01JC0000000000000000000B0B",
    "signers": ["usr_01JC0000000000000000000B0B"],
    "automatic": false,
    "step_run_id": "sr_01JD2X9A6E9G1J315N7Q9S1V3W"
  }
}
```

# Steps

## step.blocked
A step needs a person: it was handed off (`on_failure: handoff`, or
`runbook step request-human`) or reported `blocked`. `reason` is the
report's status (`failed`, `blocked`, …).

```json
{
  "id": "evt_01JD2XC9E1G3J517N9Q1S3V5W7",
  "type": "step.blocked",
  "version": 1,
  "created_at": "2026-11-18T09:47:55.901Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 9,
  "data": {
    "step_run_id": "sr_01JD2XC8D0F2H4K6M8P0R2T4V6",
    "recipients": [{ "type": "user", "id": "usr_01JC0000000000000000000A1C" }],
    "reason": "failed",
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  }
}
```

## step.completed
A step reached a final status. `path` is its place in the runbook
(`regions[2].error_rate` inside a `foreach`); `verdict` is set on checks.

```json
{
  "id": "evt_01JD2X7K3M5P7R9T1V3X5Z7B9D",
  "type": "step.completed",
  "version": 1,
  "created_at": "2026-11-18T09:33:48.120Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 6,
  "data": {
    "status": "succeeded",
    "verdict": "pass",
    "path": "check_health",
    "step_run_id": "sr_01JD2X6Q1B3N5P7R9T1V3X5Z7A"
  }
}
```

## step.reminder
A step has been open for `remind_after` (and every `remind_every` after).
`recipients` are the step's current people.

```json
{
  "id": "evt_01JD31D2F4H6K8M0P2R4T6V8X0",
  "type": "step.reminder",
  "version": 1,
  "created_at": "2026-11-18T11:47:55.004Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 15,
  "data": {
    "step_run_id": "sr_01JD2XC8D0F2H4K6M8P0R2T4V6",
    "recipients": [{ "type": "user", "id": "usr_01JC0000000000000000000A1C" }],
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  }
}
```

## step.escalated
A step has been open for `escalate_after` and escalated to `escalate_to`.
`recipients` are the expanded people; `targets` the configured teams and
users.

```json
{
  "id": "evt_01JD33E3G5J719N1Q3S5V7W9Y1",
  "type": "step.escalated",
  "version": 1,
  "created_at": "2026-11-19T09:47:55.010Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 21,
  "data": {
    "step_run_id": "sr_01JD2XC8D0F2H4K6M8P0R2T4V6",
    "recipients": [{ "type": "user", "id": "usr_01JC0000000000000000000CAR" }],
    "link": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "targets": [{ "type": "team", "id": "team_01JC00000000000000000001DS" }]
  }
}
```

# Runs

## execution.queued
A start was queued behind the runbook's concurrency limit. `sequence` is
0: the run has no events yet.

```json
{
  "id": "evt_01JD2X6PZ6S9F3X5Z7B1D4G6J8",
  "type": "execution.queued",
  "version": 1,
  "created_at": "2026-11-18T09:30:11.902Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 0,
  "data": {
    "status": "queued",
    "concurrency_key": "deploy-api"
  }
}
```

## execution.started
A run started (or a queued run got its slot).

```json
{
  "id": "evt_01JD2X6Q0A2C4E6G8J012N4Q6S",
  "type": "execution.started",
  "version": 1,
  "created_at": "2026-11-18T09:30:12.401Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 1,
  "data": {
    "status": "running"
  }
}
```

## execution.completed
A run ended. `status` is `succeeded`, `failed` or `cancelled`; `result`
holds the run's outputs.

```json
{
  "id": "evt_01JD2XF0H2K4M6P8R0T2V4X6Z8",
  "type": "execution.completed",
  "version": 1,
  "created_at": "2026-11-18T09:58:31.776Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 19,
  "data": {
    "status": "succeeded",
    "result": { "result": "kept" }
  }
}
```

# Memory

## memory.proposed
An agent proposed a memory entry for people to accept or reject. Only
identifiers: read the entry (untrusted text) through the API. From a run,
`data` names the step run; from the memory API, it also carries
`revision`, `runbook`, `actor_kind` and `actor`, and `execution` is
`null`.

```json
{
  "id": "evt_01JD2XD5F7H9K1M3P5R7T9V1X3",
  "type": "memory.proposed",
  "version": 1,
  "created_at": "2026-11-18T09:49:02.318Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 10,
  "data": {
    "memory_id": "mem_01JD2XD5E6G8J012N4Q6S8V0W2",
    "step_run_id": "sr_01JD2XC8D0F2H4K6M8P0R2T4V6"
  }
}
```

## memory.saved
An agent saved a memory entry directly, so people can follow (and revert)
what agents write. The same two shapes as `memory.proposed`.

```json
{
  "id": "evt_01JD2XE6G8J012N4Q6S8V0W2Y4",
  "type": "memory.saved",
  "version": 1,
  "created_at": "2026-11-18T09:50:13.007Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": null,
  "sequence": 0,
  "data": {
    "memory_id": "mem_01JD2XE6F7H9K1M3P5R7T9V1X3",
    "revision": 1,
    "runbook": "rb_01JC9Z0000000000000000000A",
    "actor_kind": "agent",
    "actor": "tok_01JC0000000000000000000AGT"
  }
}
```

## memory.edited
An agent edited an existing memory entry. The same two shapes as
`memory.proposed`.

```json
{
  "id": "evt_01JD2XF7H9K1M3P5R7T9V1X3Z5",
  "type": "memory.edited",
  "version": 1,
  "created_at": "2026-11-18T09:55:44.621Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": {
    "id": "exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7",
    "title": "Verify api",
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3,
    "url": "https://runbooks.example.com/runs/exe_01JD2X6PZ5R8E2W4Y6A0C3D5F7"
  },
  "sequence": 18,
  "data": {
    "memory_id": "mem_01JD2XD5E6G8J012N4Q6S8V0W2",
    "step_run_id": "sr_01JD2XC8D0F2H4K6M8P0R2T4V6"
  }
}
```

# Runbooks

## runbook.published
A runbook version was published. Identifiers only, never the source;
`execution` is `null`. Subscriptions with a runbook filter receive it for
that runbook; tag-filtered subscriptions don't.

```json
{
  "id": "evt_01JD2W0A2C4E6G8J012N4Q6S8V",
  "type": "runbook.published",
  "version": 1,
  "created_at": "2026-11-18T09:12:00.050Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": null,
  "sequence": 0,
  "data": {
    "runbook": "rb_01JC9Z0000000000000000000A",
    "runbook_slug": "post-deploy",
    "runbook_version_id": "rbv_01JD0000000000000000000003",
    "runbook_version": 3
  }
}
```

# Testing

## webhook.test
Sent by `runbook webhook test` (or the API's test route) to one
subscription, to check the receiver end to end.

```json
{
  "id": "evt_01JD2W1B3D5F7H9K1M3P5R7T9V",
  "type": "webhook.test",
  "version": 1,
  "created_at": "2026-11-18T09:14:27.388Z",
  "org": "org_01JC0000000000000000000001",
  "project": "prj_01JC0000000000000000000001",
  "execution": null,
  "sequence": 0,
  "data": {
    "webhook_id": "whk_01JD2W1A2C4E6G8J012N4Q6S8V",
    "message": "Test delivery from The Runbook."
  }
}
```
