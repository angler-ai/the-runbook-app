# Creating and changing runbooks

Use this when asked to create a runbook or change one. A runbook is a YAML
spec: `metadata.id`, then `spec` with its inputs and steps. Its
`metadata.id` is its slug and never changes once the runbook exists.

## The smallest runbook

Only `metadata.id` and an `id` on every step are required. This is a
complete runbook:

```yaml
metadata:
  id: check-health
spec:
  steps:
    - id: ping_api
    - id: confirm_dashboard
```

- `apiVersion` defaults to `therunbook.app/v1` and `kind` to `Runbook`.
  Writing them is fine; any other value is refused.
- A missing `metadata.title` or step `title` is derived from the ID: `-` and
  `_` read as spaces and the first letter is capitalized, so `check-health`
  shows as "Check health" and `ping_api` as "Ping api". The derived title is
  what the API, the outline, reports and the web show. Write a `title` when
  you want different wording.
- IDs are stable keys for references (`steps.<id>`) and history: never
  rename one to change how it reads; add a `title` instead.
- A step's `actor` defaults to `any` (a person or an agent may do it);
  `defaults.actor` changes that for every step. Set `actor: human` or
  `actor: agent` only when the work needs one or the other.

## The flow

1. **Pull the draft.** `runbook pull <runbook> --project <p> -o <file>`
   (MCP: `get_runbook_source`). Note the draft revision it reports: it is the
   expected revision for your save. For a new runbook, start a file whose
   `metadata.id` is the new slug.
2. **Edit the YAML.** Change only what was asked.
3. **Check it.** `runbook spec validate <file> --json` (MCP:
   `validate_runbook`) compiles the spec and reports its diagnostics and the
   worst-case step budget; review that budget as the plan. Fix every
   diagnostic (path, code, message) about the spec itself. The CLI checks
   offline, so it cannot resolve the project's teams, users, skills or
   called runbooks (`spec.unresolved_*`, `spec.call_unresolved`) and skips
   the checks made at publish. Never remove an assignee, approver,
   `escalate_to`, skill or call just to clear one of those: get the
   server's verdict from `validate_runbook` with `project`, or by saving
   the draft (step 4), which reports them.
4. **Save the draft.** `runbook push <file> --draft --project <p> --revision
   <n>` (MCP: `save_runbook_draft` with `expected_revision`). For a new
   runbook, leave `--revision` out on the CLI; over MCP pass
   `expected_revision: 0`. The expected revision protects other people's edits: if the
   draft moved since you pulled it, the save fails with
   `catalog.draft_conflict` and nothing is written. Pull again, reapply your
   change and save with the new revision. Never overwrite blindly.
5. **Publish.** Publish only when the person explicitly asked you to and
   the credential has `runbooks:publish`:
   `runbook publish <runbook> --project <p> --revision <n>` (MCP:
   `publish_runbook`). Otherwise report the draft revision and ask a human to
   publish it. A `403 auth.scope_missing` or `next_action:
   ask_human_to_publish` means exactly that: stop, do not retry, and tell the
   person which runbook and draft revision to publish.

`runbook push <file> --project <p>` without `--draft` does steps 4 and 5
together, so always pass `--draft` unless you were asked to publish.

## Control flow worth knowing

The server's validation is the authority on what it accepts. Beyond tasks,
groups, `switch`, `wait`, `approval`, `set` and `end`:

- `loop` takes `max_iterations` (at most 50) and either `until` or `while`.
  Both are checked after each pass, so the body runs at least once; `while`
  stops the loop when it is false.
- A `parallel` step lists its branches under `parallel:`, and takes
  `join:` and `fail_fast:` beside it (not inside the list): `join: all`
  (the default; `fail_fast: true` cancels the rest on a failure),
  `join: any`, or `join: {at_least: n}`. With `any` or
  `at_least`, the parallel finishes as soon as enough branches succeeded, or
  fails as soon as that can no longer happen, and the other branches are
  cancelled.
- `execution.concurrency` takes `key`, `limit` and `on_conflict`: `queue`
  (the default) waits for a slot, `reject` refuses the new run
  (`execution.concurrency_conflict`), `cancel_previous` cancels the runs
  waiting for the key and the one holding it (the oldest one when `limit` is
  above 1); the new run starts once that run has finished its `finally`.
  Treat `cancel_previous` like a destructive change and call it out in your
  summary.
- `call` runs another runbook of the same project as a child execution:
  `call: {runbook: <slug>, version: <n>, with: {<input>: <value>},
  outputs: {<name>: {type: …}}}`. The child must be published; without
  `version` the latest one is pinned when you publish. `with` supplies the
  child's inputs (every required one), `outputs` declares which of the
  child's outputs the step exposes (`steps.<id>.outputs.<name>`). The step
  waits for the child and fails if the child fails, is cancelled, is
  refused (for example `limit.executions_monthly`: each child counts as a
  run) or returns outputs that do not match. Calls nest at most 3 deep and
  a runbook can never call itself, directly or through another. Cancelling
  the run cancels its children. A child that can reach a destructive task
  counts as destructive for approval policies.

## Who does a step

- `actor: human | agent | any` (default `any`) says who may claim a task.
  Agents never claim `actor: human` tasks. A person may take over an
  `actor: agent` task while it is ready (never claimed, or its agent claim
  expired or was released): `runbook step claim <sr> --session <id>
  --take-over`. The run records who took it over.
- `assignee: user:<name> | team:<slug> | "${{ … }}"` routes the work: the
  Inbox, notifications, and the target of a handoff. By default it is
  guidance: anyone with run access in the project may still do the step.
- `assignment: strict` makes it binding: only the assignee (that user, or an
  active member of that team) with operator access or above may claim and
  complete the step; others get `403 claim.not_assignee`. It needs an
  `assignee`. What it means for agents depends on `actor`:
  - `actor: any` or `actor: human`: only the assignee, who is a person, may
    claim it. Agents are refused (`claim.not_assignee`) and are not offered
    the step.
  - `actor: agent`: agents still do the step, since `actor` governs agents.
    The assignee binds only a person taking it over: only the assignee may
    take it over.
- A strict assignee that resolves to nobody (for example a user without a
  role in the project, or an expression naming an unknown user) leaves the
  step blocked with `handoff.no_recipient` and notifies the project admins;
  it never falls back to anyone else. Once the assignee is fixed in the
  directory (the user is given a role, or someone joins the team), the step
  is resolved again when that person claims it.

```yaml
    - id: page_oncall
      actor: human
      assignee: team:oncall
      assignment: strict
```

## Skills on a step

A task may declare skills that the agent loads when doing the step:
- `skill: <name>`: shorthand for a single skill.
- `skills: [<name1>, <name2>]`: several skills on one step (at most 8, no duplicates). Mutually exclusive with `skill:`.
- Skills can be named by their alias declared under `spec.skills` or by their catalog reference.

## Signals with a payload

A `wait: {signal: …}` step can declare the payload it expects. The sender
(your app, a webhook relay) posts
`POST /v1/executions/{exe}/signals/{name}` with `{"payload": {...}}`. A
payload that doesn't match is refused with field errors (`payload.store_id`)
and the step keeps waiting. A matching one becomes the step's outputs:

```yaml
    - id: connect_shopify
      wait:
        signal: shopify_connected
        timeout: 14d
        outputs:
          store_id: { type: string, required: true }
    - id: welcome
      instructions: "Store ${{ steps.connect_shopify.outputs.store_id }} is connected."
```

Fields the schema doesn't declare are ignored.

## Retry and catch by failure type

Failures have a type: `timeout` (ran out of time), `failed` (reported as
failed), or `call_failed` (a child runbook failed).

```yaml
    - id: guarded
      steps:
        - id: query_metrics
          timeout: 5m
          retry: { max_attempts: 3, backoff: 30s, on: [timeout] }  # don't retry a real failure
      catch:
        - id: page_oncall
          actor: human
      catch_on: [timeout]   # other failures go to an outer catch, or fail the run
```

Without `on` or `catch_on`, any failure is retried or caught.

## Starting runs on a schedule or from a webhook

`spec.triggers` starts runs of the latest published version on its own:

```yaml
spec:
  inputs:
    service: { type: string, required: true }
  triggers:
    schedule:
      - id: nightly
        cron: "0 2 * * *"          # minute hour day-of-month month day-of-week
        timezone: America/Los_Angeles
        inputs: { service: api }   # literal values; every required input
    webhook:
      - id: on_deploy
        payload:                   # the fields expressions may read
          status: { type: string, required: true }
          repository: { type: object, properties: { name: { type: string } } }
          deployment: { type: object, properties: { id: { type: integer } } }
        when: "${{ payload.status == 'success' }}"
        inputs: { service: "${{ payload.repository.name }}" }
        dedupe: "${{ payload.deployment.id }}"   # a retried call returns the same run
```

Call a webhook trigger with
`POST /v1/projects/{project}/runbooks/{runbook}/triggers/{id}` and the
sender's JSON as the body, authorized like any start (or with `?token=` when
the sender can't set headers; use a service token limited to the project).
Fields the payload schema doesn't declare are ignored. Runs show who started
them as "Schedule nightly" or "Webhook on_deploy".

## Reminders and escalation

A step left open can nudge its people and then escalate. Both count from
when the step opened, and a paused run sends neither:

```yaml
spec:
  defaults:
    remind_after: 4h            # every task: remind once it's been open 4h
  steps:
    - id: review_config
      assignee: team:onboarding
      remind_after: 1d          # overrides the default
      remind_every: 1d          # then daily
      escalate_after: 3d        # once, after 3 days
      escalate_to: [team:leads] # they're notified and can pick it up
    - id: connect_shopify
      wait: { signal: shopify_connected, timeout: 14d }
      remind_after: 2d
```

`remind_every` needs `remind_after`, and `escalate_after` and `escalate_to`
go together. Approvals take only the reminder keys; they escalate with
`approval.escalate_to`.

## Labels on a step

`labels` is a small map of your own tags on any step. The engine ignores
them; the API returns them with the step, and the run page shows them. Use
them to tell other systems about a step, for example which steps belong to a
customer in an onboarding run:

```yaml
    - id: connect_shopify
      wait: { signal: shopify_connected, timeout: 14d }
      labels: { audience: customer, surface: settings/sources }
```

You can set at most 16 labels. Keys use lowercase letters, digits, `_`, `.`,
`/` or `-`, up to 63 characters; values are strings of up to 256 characters.

## Hints for the agent

`agent.hints` holds preferences passed to the agent doing the step. The
server never enforces them, because it can't see which tools an agent uses:

```yaml
    - id: compare_error_rate
      agent:
        hints:
          tools: [query-metrics, query-logs]
          commands: [{command: "kubectl get pods -n api"}]
```

The older spellings `agent.tools` and `agent.commands` still work; don't give
the same hint both ways. Never rely on hints for safety: limit what an agent
can do through the permissions of the access it is given, and gate risky
steps with `destructive: true` and approvals.

## Rules

- Never remove or weaken an approval, a destructive flag or an auto-decision
  policy unless explicitly asked. That includes lowering a quorum, removing
  approvers, adding an auto-decision, shortening an escalation, or dropping
  `destructive: true`. If you were asked to make such a change, call out any
  such change in your summary, naming the step and the old and new values.
- Never edit a runbook to get past a step of an execution you are running.
  If a step blocks you, report it honestly (`failed`, a `fail` verdict, or
  `request_human`); a runbook change only affects new executions anyway.
- Keep the change minimal and reviewable; do not reformat unrelated parts.
- Git-managed runbooks (origin `git`) are changed through a pull request to
  their repository, not the API; the API answers `409 runbook.git_managed`.
- Never put secrets, tokens or personal data in a runbook.
- In your summary, give the runbook, the saved draft revision, whether it was
  published (and the version), and every policy-relevant change.
