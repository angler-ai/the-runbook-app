# Pilot profile

This page states what the native pilot supports: the runbook spec and
language profile, the runtime limits that apply, the editions and stores, and
what the pilot does **not** claim. It is the profile named
`token-authenticated-pilot` (version 1) in
[`conformance/spec/pilot-capabilities.json`](../conformance/spec/pilot-capabilities.json).

The key tables at the end are checked against that file by
`crates/runbook-spec/tests/pilot_profile.rs`, and every rejected key is
compiled through the publish path by
`crates/runbook-spec/tests/reserved_keys.rs`. When the capability file
changes, update the tables in the same commit; the test names every
difference.

## Scope of the pilot

- **Edition.** One self-hosted native server (`runbook-server`) and the
  `runbook` CLI, with one organization, authenticated by tokens only:
  personal access tokens (`trb_pat_`), project service tokens (`trb_svc_`)
  and execution-scoped access/refresh pairs (`trb_exe_` / `trb_exr_`). See
  [install.md](install.md).
- **Store.** PostgreSQL 14 or newer is the only supported store
  (`runbook-store-postgres`), reached through `DATABASE_URL` (plus optional
  `DATABASE_URL_DIRECT` for the live-event listener behind a transaction
  pooler). Blobs live in Postgres. The in-memory store exists for tests only.
- **Cloudflare is deferred.** The Workers / Durable Objects / D1 / R2 adapter
  (B2c) is not part of the pilot. The model, spec, engine and core crates
  pass a `wasm32-unknown-unknown` compile check (`scripts/check.sh`); a
  compile check is not a Workers runtime verification.
- **Spec.** `apiVersion: therunbook.app/v1`, `kind: Runbook` only. Unknown
  non-extension keys are rejected (`spec.unknown_key`); `x-*` keys are
  preserved verbatim and never executed.
- **Expressions.** The bounded CEL profile `runbook-cel/1` with the
  `runbook-cost/1` cost model ([CEL decision](architecture/cel-decision.md)),
  not full CEL. Expression limits (source 4,096 bytes, 256 AST nodes,
  depth 32, fuel at most 100,000) are listed there.
- **Interfaces.** REST under `/v1` (OpenAPI generated from the Rust types),
  the CLI (contexts, `--json`, and the PRD section 10 exit codes 0 ok, 1
  error, 2 usage or invalid spec, 3 execution failed, 4 limit reached, with
  known exceptions: `runbook spec validate` exits 1 for an invalid spec, and
  `--help`/`--version` exit 2) and an MCP server, over stdio
  (`runbook mcp serve`) and remote at `/mcp` (Streamable HTTP, stateless,
  bearer tokens only; MCP1), with runbook resources, the `run_runbook`
  prompt and the [`therunbook` skill](../skills/therunbook/SKILL.md).
- **Webhooks.** Outbound, signed (`Runbook-Signature: v1=…`), at-least-once
  from a durable outbox, with the recipes in
  [`examples/webhooks/`](../examples/webhooks/) and the recording receiver in
  [`examples/pilot/`](../examples/pilot/).

## Not claimed

Gate D's evidence for the pilot is the crate tests, strict lint and
format, the Postgres suite on owned databases, wasm compile checks for the
portable crates (`scripts/check.sh`), the container smoke tests and the
unmodified flagship run end to end. From that evidence alone the pilot makes
**no** further claims:

- It is **not full v1**: only the keys in "Supported keys" below are
  promised, and several v1 features are rejected or deferred.
- There is **no production security certification**: no external audit,
  penetration test or compliance attestation has been done.
- There is **no public Docker release**: images are built and run locally;
  nothing is pushed to a registry, signed or licensed for redistribution.
- There are **no performance targets**: no latency, throughput or scale
  figures are claimed (the `/v1/me/tasks` inbox needs one indexed query
  before large deployments, R27).

### Deferred (not in the pilot)

| Area | State |
| --- | --- |
| Runbook memory (`search_memory`, `save_memory`, `memory.proposed`, memory CLI) | Deferred with B10 |
| Daily archives, retention, legal hold and the archives API/CLI | Later plan (PRD section 14); the pilot keeps the run-as-a-unit storage shape |
| `runbook login` device/browser grant, browser sessions, Better Auth sign-in | Later; the CLI takes a token with `login --token-stdin`, and a person's first token is seeded by the operator ([install.md](install.md#4-people-teams-and-their-own-tokens)) |
| External-operation reconciliation (report fields for operation IDs) | Deferred; lease fencing refuses stale reports but cannot prevent a duplicate real-world action |
| `skill install`, `skill push/pull/list` CLI, plugin bundle | Not built; skills are uploaded with `POST /v1/projects/{p}/skills` |
| Control flow: `call` (LANG4) | Supported: runs a published version of another runbook in the same project as a child execution. The version is pinned at publish; `with` maps inputs and `outputs` declares the child outputs the step exposes. The step waits for the child and takes its outcome; cancelling the run cancels the child. Children count toward the monthly run quota |
| `loop.while` (LANG1) | Supported: tested after each iteration like `until`, and the loop stops when it is false |
| `join: any` / `{at_least: n}` (LANG2) | Supported: the parallel succeeds once `n` branches succeeded and fails once that can no longer happen; either way its open branches are cancelled. `fail_fast` is for `join: all` only |
| `on_conflict: reject` / `cancel_previous` (LANG3) | Supported: when the key's pool has no free slot, `reject` refuses the start (409 `execution.concurrency_conflict`, nothing written) and `cancel_previous` cancels the waiters and the holder (the oldest one when `limit > 1`), recording the replacing run on them, and queues the new run first in line; it starts when the cancelled holder closes, so mutual exclusion holds while it cleans up |
| GitHub sync | Later |
| `pattern` constraints and input types (LANG5) | Supported: linear-time regex dialect, whole-value matching; `multi_enum`, `date`, `datetime`, `duration`, `url`, `email`, `user`. `secret_ref` remains rejected |
| Runner agent keys (R0) | Supported: `agent.name` (with `external` reserved for external agents), `agent.model`, `agent.max_cost_usd`, `agent.max_tokens`, `agent.network`, step `connections` and `spec.defaults.agent`, checked at publish against the project's agent directory; `agent.runner` is retired (`spec.retired_key`) |
| `file` and `screenshot` evidence (UP1) | Supported: a report names finalized uploads by `evd_` ID (`POST /v1/step-runs/{sr}/evidence`, then `.../finalize`); `screenshot` needs an image, `file` any upload. Servers advertise `capabilities.uploads: true` once `RUNBOOK_UPLOAD_SIGNING_KEY` and a separate `RUNBOOK_USERCONTENT_ORIGIN` are set (UP2: the user-content listener serves signed file URLs; images are re-encoded without EXIF and get thumbnails); otherwise they refuse slots |
| PDF and JUnit reports | Later; reports render as JSON and Markdown |
| Web UI, hosted runners, vaults, model billing, SSO, remote MCP OAuth | Later |

Known flagship limits (from the C4 gate): the unmodified flagship's approval
has no `auto` option, so automatic rollback is shown only in the PRD's
companion variant, and that automatic decision emits no `approval.decided`
webhook.

## How keys are classified

- **Supported** is a promise for the finished pilot by the named plan task
  owners: the key is accepted at publish and has runtime behaviour.
- **Rejected** keys fail compilation with the listed diagnostic code at the
  key's source location (a rejected container is reported once, not per
  child): `runbook spec validate` and `POST /v1/validate` report it, and a
  draft that contains one cannot be published (`runbook push` prints the
  diagnostics and stops). Nothing rejected can be published and then
  silently ignored.
- **Preserved** keys (`x-*` extensions and agent hints) are kept verbatim
  and exposed as data; the server never executes or enforces them. Agent
  hints live under `agent.hints` (`agent.hints.tools`,
  `agent.hints.commands`), passed to the agent as preferences only. The
  older spellings `agent.tools` and `agent.commands` are still accepted and
  mean the same; giving one hint both ways is refused
  (`spec.duplicate_agent_hint`).

**Enforced or hint (LANG9).** Every key in a runbook is either enforced by
the server or is a hint. The server enforces who may claim and complete a
step, approvals and destructive-step gates, leases, timeouts and retries,
and output and evidence validation. It cannot see which tools or commands an
agent uses, so those preferences are hints, under `agent.hints`. Limit what
an agent can do through the permissions of the access it is given, not
through hints.

**Who does a step (LANG7).**

- `actor`: `human`, `agent` or `any`. The default is `any`: a person or an
  agent may claim it.
- `assignee` is guidance by default: it puts the step in the named
  person's or team's inbox and notifications, and anyone allowed by `actor`
  may still claim it.
- `assignment: strict` makes the assignee binding.
  - On a `human` or `any` step, only the assignees may claim it, and agents
    can't.
  - On an `agent` step, it limits who may take over.
- A person may take over an `actor: agent` step while it is ready (never
  claimed, or its agent's claim expired or was released). The take-over is
  recorded on the run (`StepTakenOver`).

**Skills (LANG8).** `skill: name` attaches one skill; `skills: [a, b]` attaches
up to 8, with no duplicates. Each must be declared in `spec.skills`. A step
may not use both keys (`spec.both_skill_and_skills`).

**Signal payloads (LANG9).** A signal wait may declare `outputs`, a schema in
the same form as a task's outputs: `wait: {signal: shopify_connected, outputs:
{store_id: {type: string, required: true}}}`. The signal's payload must match
it: an invalid payload is refused with `422 signal.invalid_payload` and field
errors named `payload.<field>`, nothing is recorded, and the step keeps
waiting. A valid payload becomes the step's outputs (fields the schema
doesn't declare are ignored), so later steps read
`steps.<id>.outputs.<field>`. Only signal waits take `outputs`
(`spec.wait_outputs_requires_signal`). Without `outputs`, a signal accepts
any payload and sets no outputs, as before.

**Labels (LANG9).** `labels` on any step is a map of your own tags (at most
16; keys of lowercase letters, digits, `_`, `.`, `/` or `-`; string values of
at most 256 characters). The engine ignores them; the run document returns
them with each step.

**Failure types, `retry.on` and `catch_on` (LANG9).** Every failure has a
type, derived from what the engine records:

| Type | When |
| --- | --- |
| `timeout` | The step timed out or expired, or a task's attempt ran past its `timeout` |
| `failed` | A person or agent reported failure |
| `call_failed` | A `call` step's child runbook failed |

A container (group, parallel, foreach, loop or switch) that fails takes the
type of its earliest failed step.

- `retry: {max_attempts: 3, on: [timeout]}` retries only those types.
  Without `on`, it retries any failure, as before. It also works under
  `spec.defaults.retry`.
- `catch_on: [timeout]` on a group makes its `catch` handle only those
  types. Other failures propagate as if the group had no catch, so an outer
  group's catch may handle them. To handle types differently, nest groups.
  `catch_on` needs a `catch` (`spec.catch_on_without_catch`).
- A lapsed lease is not a failure: the step simply becomes ready again for
  another agent. Outputs that don't match their schema are refused before
  anything is recorded.

The run document returns each failed step's `failure_type`, and the run
page shows it.

**Triggers (LANG9b).** `spec.triggers` starts runs on its own. Only the
latest published version's triggers act, and an archived runbook's never do.

- `schedule`: entries `{id, cron, timezone, inputs, catch_up}`. `cron` has the
  standard five fields (minute, hour, day of month, month, day of week, with
  lists, ranges, steps and names), matched in `timezone` (an IANA name,
  default `UTC`). `inputs` are literal values, checked against the runbook's
  inputs, and must give every required input without a default. A tick
  missed while the server was down starts late if it's still inside a few
  minutes, or inside `catch_up` (at most 24 hours); otherwise it's skipped.
  Each tick starts one run, however many servers or passes see it. Runs
  record `schedule:<id>` as who started them.
- `webhook`: entries `{id, payload, when, inputs, dedupe}`, called at
  `POST /v1/projects/{project}/runbooks/{runbook}/triggers/{id}` with a token
  that may start runs in the project (in `Authorization`, or `?token=` for
  senders that can't set headers). The body is the sender's own JSON.
  `payload` declares the fields expressions may read (others are ignored);
  `when` (true or false), `inputs` and `dedupe` are expressions over
  `payload` only. A field the call leaves out makes `when` false, leaves an
  input to its default and `dedupe` unset. A call with the same `dedupe`
  value as an earlier one returns that run. Runs record `webhook:<id>`.
- The runbook page lists the triggers in the source it is showing. For a
  published runbook it gives each webhook's full `POST` URL and the token
  forms (`Authorization: Bearer …`, or `?token=` for senders that cannot set
  headers); unpublished draft triggers are labelled inactive.
- At most 16 of each, IDs unique across both. Refusals:
  `spec.invalid_cron`, `spec.invalid_timezone`, `spec.trigger_input_invalid`,
  `spec.trigger_when_type`, `spec.duplicate_trigger`.

**Reminders and escalation (LANG9).** A step left open (ready, claimed or
waiting, an unresolved handoff included) can remind its people and escalate:

- `remind_after: 2h` sends a `step.reminder` notification once the step has
  been open that long; `remind_every: 4h` repeats it (it needs
  `remind_after`). Reminders go to the step's current people: approvers, the
  assignee, whoever claimed it, a handed-off assignee and its escalation;
  with none, the runbook's owners. Reminders that fell due together (after
  a pause, say) send once.
- `escalate_after: 1d` with `escalate_to: [team:leads]` (both or neither)
  escalates once: a `step.escalated` notification goes to the escalation,
  the task's inbox entry adds them, and they may claim a strict task too.
- Both count from when the step opened. A paused run sends neither; a step
  that opens again (a retry, say) starts over. A failed attempt waiting to
  be retried isn't open.
- Tasks inherit `spec.defaults.remind_after`, `remind_every`,
  `escalate_after` and `escalate_to`; a step's own keys override them one
  by one. Waits take all four; approvals take only the reminder keys (they
  escalate with `approval.escalate_to`). Invalid combinations are refused
  with `spec.followup_invalid`.
- Run views carry `followup` once a reminder has been sent or the step has
  escalated in its current attempt, including the escalation's resolved
  recipients. The run page shows those facts beside the step in its graph,
  document, steps and checklist views and in Markdown exports.

- **Notification email (NOTIFY1)**. Approvals, assignments and handoffs,
  reminders, escalations and failed runs also go out by email to people
  with a verified address, rendered as text and HTML with a link into the
  app and a one-click unsubscribe. Each person chooses `all`,
  `approvals_only`, `daily_summary` or `none` in account settings.
  Invitations mail a sign-in link when they are created. Mail is a
  durable outbox beside webhook deliveries; a failed send retries and
  never blocks the run.
- **Slack (SLACK1 stage 1)**. A project may name one Slack incoming
  webhook in project settings. Step assignments, approvals needed,
  handoffs/stalls and failed runs post a one-line message linking to the
  run. The URL is a credential: it can be replaced or removed, never read
  back. There is no Slack app, OAuth or buttons in this stage.
- Paths are dotted; `*` stands for any map entry (an input, field or output
  name) or any array index. An example value of `"example"` means any
  well-formed value of that key's documented shape; other values are the
  exact value classified (for instance `on_conflict: "queue"` is supported,
  `"reject"` is rejected).
- Shape checks are separate passes: declaration shapes, recursive type
  constraints and the field count (at most 10 indexed fields) (A5);
  `foreach.max_items` and `loop.max_iterations` are required bounds (A5);
  the expression surface and cost profile (A0/A4).

Publication constraints that apply on top of the tables:

- `timeout` on a node applies to tasks only (`spec.timeout_requires_task` on
  controls); `defaults.timeout` applies only to tasks; nested wait/approval
  timeouts and the execution timeout remain supported.
- `approval.allow_agents` requires `min: 1`
  (`spec.agent_approval_requires_min_one`); an agent decision is recorded
  separately from human signatures, and the destructive-path prohibition
  remains.
- `end` is rejected anywhere inside `finally` (`spec.end_in_finally`);
  normal and `catch` sections remain supported.
- `spec.agents.params` is passed through untouched to `agent.work_available`
  subscribers and must encode to at most 4,096 bytes
  (`spec.agents_params_too_large`); `spec.agents.dispatch` was removed
  (`spec.agents_dispatch_removed`): use webhook subscriptions instead.

## Runtime limits

| Limit | Value | Where |
| --- | --- | --- |
| Runbook source (PRD §17 proposes 256 KB as a cloud default) | 1 MiB (lower with `RUNBOOK_MAX_SOURCE_BYTES`, for example to 256 KiB): the compiler plan fixes 1 MiB as the native safety ceiling and keeps the PRD's numbers as configurable cloud defaults | compiler, authoring routes |
| Source values / control nodes / control depth | 262,144 / 4,096 / 32 (implementation ceilings) | compiler |
| Identified nodes per runbook (PRD §17 proposes 500) | 256: every node with an `id` is a field of the `steps` expression record, which admits 256 fields (`expression.schema_limit`); this is lower than the PRD number and is the effective limit | compiler |
| Nesting depth (PRD §17 proposes 8) | 32 nested containers; at most 4 `foreach`/`loop` levels, because each level nests the `steps` record type (expression type depth 16, `expression.schema_limit`) | compiler |
| `foreach` items / `loop` iterations / `parallel` branches | **Enforced** (PRD §17): `max_items` at most 100, `max_iterations` at most 50, at most 20 branches (`spec.limit_exceeded`) | compiler |
| Single `wait` (PRD §17: 7 days) | **Enforced**: `wait.for` at most 7 days at publish (`spec.limit_exceeded`); an `until` target more than 7 days after the wait starts fails the wait step and the run (`limit.wait_exceeded`, engine version 4; runs started under older engine versions keep their targets). Signal waits and `timeout` are bounded by the execution's own timeout | compiler, engine |
| `call` depth (PRD §17: 3) | 3, and no runbook can reach itself through `call` (`spec.call_depth_exceeded`, `spec.call_cycle`); the start also refuses a deeper child | compiler, core |
| `metadata.id` | kebab case, at most 63 bytes (`spec.invalid_id`), like node IDs and directory slugs | compiler |
| Integer negation | `-(-9223372036854775808)` is `expression.integer_overflow` at publish, never a runtime error | compiler |
| Data depth / schema declarations | 64 / 4,096 | compiler |
| Indexed custom fields | 10 per runbook | compiler, run search |
| Worst-case step runs | computed and reported by `runbook spec validate`; overflow is `spec.step_budget_overflow` | compiler |
| Step runs per run | No cap by default. `RUNBOOK_LIMIT_STEP_RUNS` sets one (both editions): a run that would open one more step run fails with `limit.step_runs_exceeded` and keeps what it did; the run view's `terminal_reason` says so. `/v1/meta` reports it (`limits.step_runs`); the editor warns at publish time when the worst case is over it | engine, server |
| `spec.agents.params` | 4,096 encoded bytes | compiler |
| Request body | about 2 MiB on every route; skill uploads keep 64 MiB (`RUNBOOK_MAX_REQUEST_BYTES`); a tokenless request with a body is 401 before it is read | server (F45) |
| Step outputs (PRD §17: 64 KB per step) | **Enforced** (F9): 64 KiB encoded JSON, `422 step.outputs_too_large` | claims |
| Step log (PRD §17: 5 MB per step) | **Enforced** (F9): 5 MiB per step across appended lines (report logs alone also at most 5 MiB), `422 step.logs_too_large` | claims |
| A run's outputs together (CF13 review round 1) | **Enforced**: at most 256 declared outputs (`expression.schema_limit`), and 900 KiB for all of a run's output values (and their reads) together as stored; a run that would succeed with more fails with `execution.outputs_too_large`, and a failed or cancelled run closes with the outputs past it null (engine version 5; runs started under older engine versions keep theirs) | compiler, engine |
| A run's stored size (CF13 review round 1) | **Enforced**: a run's event log is held to 8 MiB by default (`RUNBOOK_LIMIT_RUN_BYTES`); a request whose own event carries more than 4 KiB and would pass it is refused (422 `execution.too_large`); claims, cancels and short reports always pass, so such a run can still be finished or cancelled. A hard ceiling of one and a half times the budget (CF13b, engine version 6) holds the engine's own events and small writes too, from the run's activation on: past it the run is faulted with `limit.run_size_exceeded`, every open step is cancelled, no `catch`, group `finally` or top-level `finally` runs, and it closes in the same commit, failed with that reason (a cancelled run stays cancelled; a failing run keeps a reason it recorded, and one failing from a plain step failure, which records none, gets this one) | core (both editions) |
| Evidence files (PRD §17, §14) | The configured upload limits (none by default when self-hosted): type (PNG, JPEG, WebP, GIF, PDF, text, JSON; HEIC and SVG refused), bytes per image and per file, images per step run, files per execution, storage per organization; size and magic bytes are checked at finalize | uploads (UP1) |
| Skill upload | bounded by the 64 MiB request body (base64 content, so less than that of files), lower with `RUNBOOK_MAX_SKILL_BYTES`; file paths at most 255 bytes | server |
| Personal access token lifetime | default 90 days, at most 1 year, never past the issuing PAT | token routes |
| Service token lifetime | until revoked (optional expiry) | token routes |
| Execution access token / refresh family | 1 hour / at most 30 days, only while the run is open; reuse revokes the family | execution credentials |
| Agent claim lease | 10 minutes after the last heartbeat | claims |
| `Idempotency-Key` replay window | 24 hours | every POST |
| Webhook subscriptions | 20 per project; URL at most 2,048 bytes | webhook routes |
| Webhook delivery | 2xx within 10 s; retries with backoff for 24 h; disabled after 3 days of failures; log kept 90 days; rotation overlap 24 h | delivery worker |
| Postgres lock wait / idle transaction | 10 s / 30 s | store |

## Key classification

### Supported keys

| Path | Example value | Owner (plan tasks) |
| --- | --- | --- |
| `apiVersion` | `"therunbook.app/v1"` | `A1/A5/B1/B3` |
| `kind` | `"Runbook"` | `A1/A5/B1/B3` |
| `metadata.id` | `"example"` | `A1/A5/B1/B3` |
| `metadata.title` | `"example"` | `A1/A5/B1/B3` |
| `metadata.description` | `"example"` | `A1/A5/B1/B3` |
| `metadata.tags` | `"example"` | `A1/A5/B1/B3` |
| `metadata.owners` | `"example"` | `A1/A5/B1/B3` |
| `spec.execution.title` | `"example"` | `A4/A18/A19/B5` |
| `spec.execution.tags` | `"example"` | `A4/A18/A19/B5` |
| `spec.execution.timeout` | `"example"` | `A4/A18/A19/B5` |
| `spec.execution.cleanup_timeout` | `"example"` | `A4/A18/A19/B5` |
| `spec.execution.concurrency.key` | `"example"` | `B5` |
| `spec.execution.concurrency.limit` | `"example"` | `B5` |
| `spec.execution.concurrency.on_conflict` | `"queue"` | `B5` |
| `spec.execution.concurrency.on_conflict` | `"reject"` | `B5/LANG3` |
| `spec.execution.concurrency.on_conflict` | `"cancel_previous"` | `B5/LANG3` |
| `spec.inputs.*.type` | `"string"` | `A4/A5` |
| `spec.inputs.*.type` | `"text"` | `A4/A5` |
| `spec.inputs.*.type` | `"number"` | `A4/A5` |
| `spec.inputs.*.type` | `"integer"` | `A4/A5` |
| `spec.inputs.*.type` | `"boolean"` | `A4/A5` |
| `spec.inputs.*.type` | `"enum"` | `A4/A5` |
| `spec.inputs.*.type` | `"list"` | `A4/A5` |
| `spec.inputs.*.type` | `"object"` | `A4/A5` |
| `spec.inputs.*.type` | `"multi_enum"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"date"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"datetime"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"duration"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"url"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"email"` | `A4/A5/LANG5` |
| `spec.inputs.*.type` | `"user"` | `A4/A5/LANG5` |
| `spec.inputs.*.title` | `"example"` | `A4/A5` |
| `spec.inputs.*.description` | `"example"` | `A4/A5` |
| `spec.inputs.*.required` | `"example"` | `A4/A5` |
| `spec.inputs.*.default` | `"example"` | `A4/A5` |
| `spec.inputs.*.options` | `"example"` | `A4/A5` |
| `spec.inputs.*.min` | `"example"` | `A4/A5` |
| `spec.inputs.*.max` | `"example"` | `A4/A5` |
| `spec.inputs.*.items` | `{}` | `A4/A5` |
| `spec.inputs.*.properties` | `{}` | `A4/A5` |
| `spec.inputs.*.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.fields.*.type` | `"string"` | `A4/A5` |
| `spec.fields.*.type` | `"text"` | `A4/A5` |
| `spec.fields.*.type` | `"number"` | `A4/A5` |
| `spec.fields.*.type` | `"integer"` | `A4/A5` |
| `spec.fields.*.type` | `"boolean"` | `A4/A5` |
| `spec.fields.*.type` | `"enum"` | `A4/A5` |
| `spec.fields.*.type` | `"list"` | `A4/A5` |
| `spec.fields.*.type` | `"object"` | `A4/A5` |
| `spec.fields.*.title` | `"example"` | `A4/A5` |
| `spec.fields.*.description` | `"example"` | `A4/A5` |
| `spec.fields.*.required` | `"example"` | `A4/A5` |
| `spec.fields.*.default` | `"example"` | `A4/A5` |
| `spec.fields.*.options` | `"example"` | `A4/A5` |
| `spec.fields.*.min` | `"example"` | `A4/A5` |
| `spec.fields.*.max` | `"example"` | `A4/A5` |
| `spec.fields.*.items` | `{}` | `A4/A5` |
| `spec.fields.*.properties` | `{}` | `A4/A5` |
| `spec.fields.*.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.steps.*.outputs.*.type` | `"string"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"text"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"number"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"integer"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"boolean"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"enum"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"list"` | `A4/A5` |
| `spec.steps.*.outputs.*.type` | `"object"` | `A4/A5` |
| `spec.steps.*.outputs.*.title` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.description` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.required` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.default` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.options` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.min` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.max` | `"example"` | `A4/A5` |
| `spec.steps.*.outputs.*.items` | `{}` | `A4/A5` |
| `spec.steps.*.outputs.*.properties` | `{}` | `A4/A5` |
| `spec.steps.*.outputs.*.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.inputs.*.items.type` | `"string"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"text"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"number"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"integer"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"boolean"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"enum"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"list"` | `A4/A5` |
| `spec.inputs.*.items.type` | `"object"` | `A4/A5` |
| `spec.inputs.*.items.title` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.description` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.required` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.default` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.options` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.min` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.max` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.items` | `{}` | `A4/A5` |
| `spec.inputs.*.items.properties` | `{}` | `A4/A5` |
| `spec.inputs.*.items.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.inputs.*.properties.*.type` | `"string"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"text"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"number"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"integer"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"boolean"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"enum"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"list"` | `A4/A5` |
| `spec.inputs.*.properties.*.type` | `"object"` | `A4/A5` |
| `spec.inputs.*.properties.*.title` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.description` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.required` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.default` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.options` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.min` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.max` | `"example"` | `A4/A5` |
| `spec.inputs.*.properties.*.items` | `{}` | `A4/A5` |
| `spec.inputs.*.properties.*.properties` | `{}` | `A4/A5` |
| `spec.inputs.*.properties.*.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.inputs.*.items.properties.*.items.type` | `"string"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"text"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"number"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"integer"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"boolean"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"enum"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"list"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.type` | `"object"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.title` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.description` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.required` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.default` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.options` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.min` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.max` | `"example"` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.items` | `{}` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.properties` | `{}` | `A4/A5` |
| `spec.inputs.*.items.properties.*.items.pattern` | `".*"` | `A4/A5/LANG5` |
| `spec.fields.*.indexed` | `true` | `B3/B8` |
| `spec.fields.*.locked` | `true` | `B3/B8` |
| `spec.inputs.*` | `{}` | `A4/A5` |
| `spec.fields.*` | `{}` | `A4/A5` |
| `spec.vars.*` | `"example"` | `A4/A5` |
| `spec.vars.*.arbitrary` | `"example"` | `A4/A5` |
| `spec.inputs.*.default.arbitrary` | `"example"` | `A4/A5` |
| `spec.inputs.*.options.0.arbitrary` | `"example"` | `A4/A5` |
| `spec.defaults.actor` | `"example"` | `A5/A15/A18` |
| `spec.defaults.timeout` | `"example"` | `A5/A15/A18` |
| `spec.defaults.on_failure` | `"example"` | `A5/A15/A18` |
| `spec.defaults.retry.max_attempts` | `"example"` | `A5/A15/A18` |
| `spec.defaults.retry.backoff` | `"example"` | `A5/A15/A18` |
| `spec.skills.*` | `"example"` | `A4/A5/A20/B3` |
| `spec.steps` | `[]` | `A7-A20/B8` |
| `spec.finally` | `[]` | `A7-A20/B8` |
| `spec.outputs.*` | `"example"` | `A7-A20/B8` |
| `spec.report` | `"example"` | `A20/B8` |
| `spec.steps.*.id` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.title` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.if` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.actor` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.assignee` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.check` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.skippable` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.timeout` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.on_failure` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.destructive` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.allow_auto_decision` | `"example"` | `A5/A8/A15-A20/B1/B6` |
| `spec.steps.*.instructions` | `"example"` | `A20` |
| `spec.steps.*.instructions.default` | `"example"` | `A20` |
| `spec.steps.*.instructions.human` | `"example"` | `A20` |
| `spec.steps.*.instructions.agent` | `"example"` | `A20` |
| `spec.steps.*.skill` | `"example"` | `A4/A5/A20/B3` |
| `spec.steps.*.skills` | `["example"]` | `LANG8` |
| `spec.steps.*.assignment` | `"strict"` | `LANG7` |
| `spec.steps.*.labels` | `{"audience": "customer"}` | `LANG9` |
| `spec.steps.*.retry.on` | `["timeout"]` | `LANG9` |
| `spec.steps.*.catch_on` | `["timeout"]` | `LANG9` |
| `spec.steps.*.remind_after` | `"2h"` | `LANG9` |
| `spec.steps.*.remind_every` | `"4h"` | `LANG9` |
| `spec.steps.*.escalate_after` | `"1d"` | `LANG9` |
| `spec.steps.*.escalate_to` | `["team:leads"]` | `LANG9` |
| `spec.defaults.remind_after` | `"2h"` | `LANG9` |
| `spec.defaults.escalate_to` | `["team:leads"]` | `LANG9` |
| `spec.triggers.schedule.*.id` | `"nightly"` | `LANG9b` |
| `spec.triggers.schedule.*.cron` | `"0 2 * * *"` | `LANG9b` |
| `spec.triggers.schedule.*.timezone` | `"America/Los_Angeles"` | `LANG9b` |
| `spec.triggers.schedule.*.inputs.*` | `"api"` | `LANG9b` |
| `spec.triggers.schedule.*.catch_up` | `"1h"` | `LANG9b` |
| `spec.triggers.webhook.*.id` | `"on_deploy"` | `LANG9b` |
| `spec.triggers.webhook.*.payload.*.type` | `"string"` | `A4/A5` |
| `spec.triggers.webhook.*.when` | `"${{ payload.status == 'success' }}"` | `LANG9b` |
| `spec.triggers.webhook.*.inputs.*` | `"${{ payload.service }}"` | `LANG9b` |
| `spec.triggers.webhook.*.dedupe` | `"${{ payload.id }}"` | `LANG9b` |
| `spec.steps.*.with.*` | `"example"` | `A4/A5/A20/B3` |
| `spec.steps.*.checklist` | `[]` | `A8` |
| `spec.steps.*.checklist.0` | `"example"` | `A8` |
| `spec.steps.*.retry.max_attempts` | `"example"` | `A5/A15/A18` |
| `spec.steps.*.retry.backoff` | `"example"` | `A5/A15/A18` |
| `spec.steps.*.evidence.required.0` | `"link"` | `A8/B8` |
| `spec.steps.*.evidence.required.0` | `"log"` | `A8/B8` |
| `spec.steps.*.evidence.accepted.0` | `"link"` | `A8/B8` |
| `spec.steps.*.evidence.accepted.0` | `"log"` | `A8/B8` |
| `spec.steps.*.evidence.required.0` | `"file"` | `A8/B8/UP1` |
| `spec.steps.*.evidence.required.0` | `"screenshot"` | `A8/B8/UP1` |
| `spec.steps.*.evidence.accepted.0` | `"file"` | `A8/B8/UP1` |
| `spec.steps.*.evidence.accepted.0` | `"screenshot"` | `A8/B8/UP1` |
| `spec.steps.*.steps` | `[]` | `A7/A14/A19` |
| `spec.steps.*.catch` | `[]` | `A7/A14/A19` |
| `spec.steps.*.finally` | `[]` | `A7/A14/A19` |
| `spec.steps.*.parallel` | `[]` | `A11` |
| `spec.steps.*.join` | `"all"` | `A11` |
| `spec.steps.*.join` | `"any"` | `A11/LANG2` |
| `spec.steps.*.join` | `{"at_least": 1}` | `A11/LANG2` |
| `spec.steps.*.join.at_least` | `1` | `A11/LANG2` |
| `spec.steps.*.fail_fast` | `true` | `A11` |
| `spec.steps.*.foreach.items` | `"example"` | `A12` |
| `spec.steps.*.foreach.as` | `"example"` | `A12` |
| `spec.steps.*.foreach.key` | `"example"` | `A12` |
| `spec.steps.*.foreach.concurrency` | `"example"` | `A12` |
| `spec.steps.*.foreach.max_items` | `"example"` | `A12` |
| `spec.steps.*.foreach.steps` | `[]` | `A12` |
| `spec.steps.*.loop.until` | `"example"` | `A13` |
| `spec.steps.*.loop.while` | `"example"` | `A13/LANG1` |
| `spec.steps.*.loop.max_iterations` | `"example"` | `A13` |
| `spec.steps.*.loop.delay` | `"example"` | `A13` |
| `spec.steps.*.loop.steps` | `[]` | `A13` |
| `spec.steps.*.switch.cases.*.when` | `"example"` | `A10` |
| `spec.steps.*.switch.cases.*.steps` | `[]` | `A10` |
| `spec.steps.*.switch.default.steps` | `[]` | `A10` |
| `spec.steps.*.wait.for` | `"example"` | `A17` |
| `spec.steps.*.call` | `{}` | `LANG4` |
| `spec.steps.*.call.runbook` | `"other"` | `LANG4` |
| `spec.steps.*.call.version` | `3` | `LANG4` |
| `spec.steps.*.call.with.env` | `"prod"` | `LANG4` |
| `spec.steps.*.call.outputs.*.type` | `"integer"` | `A4/A5` |
| `spec.steps.*.wait.until` | `"example"` | `A17` |
| `spec.steps.*.wait.signal` | `"example"` | `A17` |
| `spec.steps.*.wait.timeout` | `"example"` | `A17` |
| `spec.steps.*.wait.outputs.*.type` | `"string"` | `A4/A5` |
| `spec.steps.*.approval.prompt` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.min` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.timeout` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.escalation_timeout` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.on_expired` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.allow_agents` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.escalate_to.0` | `"example"` | `A6/A16/A18/B1/B6` |
| `spec.steps.*.approval.options.0` | `"keep"` | `A6/A16` |
| `spec.steps.*.approval.options.*.id` | `"example"` | `A6/A16` |
| `spec.steps.*.approval.options.*.title` | `"example"` | `A6/A16` |
| `spec.steps.*.approval.options.*.auto` | `"example"` | `A6/A16` |
| `spec.steps.*.approval.approvers.users` | `[]` | `A6/A16` |
| `spec.steps.*.approval.approvers.teams` | `[]` | `A6/A16` |
| `spec.steps.*.approval.on_timeout` | `"escalate"` | `A6/A16` |
| `spec.steps.*.approval.on_timeout.decide` | `"rollback"` | `A6/A16` |
| `spec.steps.*.set.vars.*` | `"example"` | `A9` |
| `spec.steps.*.set.fields.*` | `"example"` | `A9` |
| `spec.steps.*.end.status` | `"example"` | `A9/A19` |
| `spec.steps.*.end.message` | `"example"` | `A9/A19` |
| `spec.agents.params` | `{"repo": "acme/api", "workflow": "runbook-resume.yml"}` | `C1` |
| `spec.agents.params.x-opaque.nested` | `null` | `C1` |
| `spec.steps.*.agent.name` | `"deploy-checker"` | `R0` |
| `spec.steps.*.agent.model` | `"fast"` | `R0` |
| `spec.steps.*.agent.max_cost_usd` | `0.5` | `R0` |
| `spec.steps.*.agent.max_tokens` | `1000` | `R0` |
| `spec.steps.*.agent.network` | `["api.example.com"]` | `R0` |
| `spec.steps.*.connections` | `["datadog-readonly"]` | `R0` |
| `spec.defaults.agent.name` | `"deploy-checker"` | `R0` |

### Rejected keys

| Path | Example value | Diagnostic code |
| --- | --- | --- |
| `spec.steps.*.evidence.required.0` | `"video"` | `spec.unsupported_value` |
| `spec.steps.*.agent.runner` | `true` | `spec.retired_key` |
| `kind` | `"StepLibrary"` | `spec.unsupported_kind` |
| `apiVersion` | `"therunbook.app/v2"` | `spec.unsupported_api_version` |
| `spec.magic` | `true` | `spec.unknown_key` |
| `spec.inputs.*.future` | `true` | `spec.unknown_key` |
| `spec.inputs.*.type` | `"secret_ref"` | `spec.unsupported_type` |
| `spec.agents.dispatch` | `{"github": {}}` | `spec.agents_dispatch_removed` |
| `spec.agents.dispatch.github.repo` | `"acme/api"` | `spec.agents_dispatch_removed` |

### Preserved keys

| Path | Example value |
| --- | --- |
| `spec.steps.*.agent.tools` | `[]` |
| `spec.steps.*.agent.commands` | `[]` |
| `spec.steps.*.agent.hints.tools` | `[]` |
| `spec.steps.*.agent.hints.commands` | `[]` |
| `x-extension` | `{"nested": "retained"}` |
| `metadata.x-extension` | `{"nested": "retained"}` |
| `spec.x-extension` | `{"nested": "retained"}` |
| `spec.inputs.x-extension` | `{"nested": "retained"}` |
| `spec.inputs.*.x-extension` | `{"nested": "retained"}` |
| `spec.inputs.*.items.x-extension` | `{"nested": "retained"}` |
| `spec.inputs.*.properties.*.x-extension` | `{"nested": "retained"}` |
| `spec.vars.x-extension` | `{"nested": "retained"}` |
| `spec.execution.concurrency.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.foreach.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.loop.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.switch.cases.*.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.approval.options.*.x-extension` | `{"nested": "retained"}` |
| `spec.steps.*.agent.x-extension` | `{"nested": "retained"}` |

