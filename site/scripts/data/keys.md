<!--
The runbook key reference: one entry per key the compiler accepts.

scripts/generate.mjs reads this file and the key tree that `runbook
__docs-reference` probes from crates/runbook-spec (the capability matrix),
and writes src/content/docs/reference/keys.mdx. Generation fails when a key
in the tree has no entry here, or an entry names a key the tree doesn't
have, so the page can't drift from the compiler.

Format:
  # Section title           starts a section; text under it is the intro
  ## <path>                 one key, as the tree prints it
  type: ...                 metadata lines, then a blank line
  as: step | doc | inputs   how check:samples wraps the example
  ...description (Markdown)
  ```yaml                   the first yaml block is the example
-->

# Document

A runbook is one YAML document with `metadata` and `spec`. Keys starting
with `x-` are allowed at every level: they are kept verbatim and never
executed. Every other unknown key is refused with `spec.unknown_key`.

## apiVersion
type: string
default: therunbook.app/v1
as: doc

The language version. Optional; when present it must be
`therunbook.app/v1` (`spec.unsupported_api_version`).

```yaml
apiVersion: therunbook.app/v1
kind: Runbook
metadata: { id: hello }
spec:
  steps:
    - id: say_hello
```

## kind
type: string
default: Runbook
as: doc

The document kind. Optional; when present it must be `Runbook`
(`spec.unsupported_kind`).

```yaml
kind: Runbook
metadata: { id: hello }
spec:
  steps:
    - id: say_hello
```

## metadata
type: object
required: yes
as: doc

Who the runbook is and who owns it. Only `id` is required.

```yaml
metadata:
  id: post-deploy
  title: Post-deploy verification
spec:
  steps:
    - id: check
```

## metadata.id
type: string
required: yes
as: doc

The runbook's ID within its project: kebab case (lowercase letters and
digits, separated by single hyphens), at most 63 bytes
(`spec.invalid_id`). It names the runbook in the CLI, the API and `call`.

```yaml
metadata: { id: cert-expiry-sweep }
spec:
  steps:
    - id: list_certificates
```

## metadata.title
type: string
as: doc

A human title. Without one, the title is derived from the ID
(`cert-expiry-sweep` becomes "Cert expiry sweep").

```yaml
metadata:
  id: cert-expiry-sweep
  title: Certificate expiry sweep
spec:
  steps:
    - id: list_certificates
```

## metadata.description
type: string
as: doc

A longer description, shown on the runbook's page.

```yaml
metadata:
  id: cert-expiry-sweep
  description: Finds certificates that expire in the next 30 days and renews them.
spec:
  steps:
    - id: list_certificates
```

## metadata.tags
type: list of strings
as: doc

Tags for finding and grouping the runbook.

```yaml
metadata:
  id: cert-expiry-sweep
  tags: [security, weekly]
spec:
  steps:
    - id: list_certificates
```

## metadata.owners
type: list of recipients
as: doc

The people or teams who own the runbook: `team:<slug>`, `user:<name>` or a
bare user name. Owners get reminders when a step has no one else to
remind, and an approval's timeout escalates to them when it names no
`escalate_to`. They resolve against your server when you publish.

```yaml
metadata:
  id: cert-expiry-sweep
  owners: [team:platform]
spec:
  steps:
    - id: list_certificates
```

## spec
type: object
required: yes
as: doc

The procedure: its inputs, steps and outputs. Only `steps` is required.

```yaml
metadata: { id: hello }
spec:
  steps:
    - id: say_hello
```

# Runs: spec.execution

Settings for each run (execution) of the runbook.

## spec.execution
type: object
as: doc

Run-level settings: the run's title and tags, its timeout, and a
concurrency limit.

```yaml
metadata: { id: nightly-backup-check }
spec:
  execution:
    title: Backup check
    timeout: 2h
  steps:
    - id: check
```

## spec.execution.title
type: string or template
as: doc

The title each run gets. It may interpolate expressions, so runs are told
apart in lists.

```yaml
metadata: { id: post-deploy }
spec:
  inputs:
    service: { type: string, required: true }
  execution:
    title: "Verify ${{ inputs.service }}"
  steps:
    - id: check
```

## spec.execution.tags
type: list of strings or templates
as: doc

Tags on every run. Runs can be listed by tag, and webhook subscriptions
with a tag filter receive only matching runs' events.

```yaml
metadata: { id: post-deploy }
spec:
  inputs:
    service: { type: string, required: true }
  execution:
    tags: [deploy, "${{ inputs.service }}"]
  steps:
    - id: check
```

## spec.execution.timeout
type: duration
as: doc

The most a whole run may take. A run past it times out and its cleanup
(`finally`) runs. It also bounds signal waits, which have no limit of
their own.

```yaml
metadata: { id: post-deploy }
spec:
  execution: { timeout: 4h }
  steps:
    - id: check
```

## spec.execution.cleanup_timeout
type: duration
default: 30m
as: doc

How long the run's cleanup (its `finally` steps after a timeout, a
failure or a cancel) may take before the run is closed anyway.

```yaml
metadata: { id: db-migration }
spec:
  execution: { timeout: 2h, cleanup_timeout: 15m }
  steps:
    - id: migrate
  finally:
    - id: release_lock
```

## spec.execution.concurrency
type: object
as: doc

Limits how many runs with the same key run at once. Runs beyond the limit
are queued, refused or replace the running one, by `on_conflict`.

```yaml
metadata: { id: deploy }
spec:
  inputs:
    service: { type: string, required: true }
  execution:
    concurrency:
      key: "deploy-${{ inputs.service }}"
      limit: 1
  steps:
    - id: deploy
```

## spec.execution.concurrency.key
type: string or template
required: yes
as: doc

The pool a run joins. Runs share a limit only when their keys are equal,
so a key built from an input limits per value (per service, per region).

```yaml
metadata: { id: deploy }
spec:
  inputs:
    region: { type: string, required: true }
  execution:
    concurrency: { key: "region-${{ inputs.region }}", limit: 1 }
  steps:
    - id: deploy
```

## spec.execution.concurrency.limit
type: positive integer
required: yes
as: doc

How many runs with the same key may be open at once.

```yaml
metadata: { id: load-test }
spec:
  execution:
    concurrency: { key: load-test, limit: 2 }
  steps:
    - id: run
```

## spec.execution.concurrency.on_conflict
type: queue | reject | cancel_previous
default: queue
as: doc

What a start does when the pool is full:

- `queue` waits for a slot (the run is `queued` and a
  `execution.queued` webhook fires);
- `reject` refuses the start (`409 execution.concurrency_conflict`) and
  writes nothing;
- `cancel_previous` cancels the waiting runs and the running one (the
  oldest, when `limit` is more than 1) and queues the new run first in
  line; it starts once the cancelled run has finished its cleanup.

```yaml
metadata: { id: preview-env }
spec:
  inputs:
    branch: { type: string, required: true }
  execution:
    concurrency:
      key: "preview-${{ inputs.branch }}"
      limit: 1
      on_conflict: cancel_previous
  steps:
    - id: build
```

# Data: inputs, fields, vars and outputs

## spec.inputs
type: map of declarations
as: doc

The values a run is started with, each a typed declaration (see
[Declarations](#declarations)). A start with a missing required input, or
a value of the wrong type, is refused before the run exists.

```yaml
metadata: { id: post-deploy }
spec:
  inputs:
    service: { type: enum, options: [api, web], required: true }
    version: { type: string, pattern: "v[0-9]+\\.[0-9]+\\.[0-9]+" }
  steps:
    - id: check
      instructions: Check ${{ inputs.service }} at ${{ inputs.version }}.
```

## spec.fields
type: map of declarations
as: doc

Typed values on the run that steps can `set`, such as a severity or a
ticket link. They show on the run page, and up to 10 can be `indexed` to
search runs by them. Fields take every [declaration](#declarations) key,
plus `indexed` and `locked`.

```yaml
metadata: { id: incident-triage }
spec:
  fields:
    severity: { type: enum, options: [sev1, sev2, sev3], default: sev3, indexed: true }
  steps:
    - id: classify
      set: { fields: { severity: sev2 } }
```

## spec.fields.<name>.indexed
type: boolean
default: false
as: doc

Makes a field searchable: `runbook run list --field severity=sev1`. At
most 10 indexed fields per runbook (`spec.indexed_field_limit`).

```yaml
metadata: { id: incident-triage }
spec:
  fields:
    severity: { type: string, indexed: true }
  steps:
    - id: classify
```

## spec.fields.<name>.locked
type: boolean
default: false
as: doc

Refuses edits to the field once the run has started
(`data.locked_field`), for values that must not change during a run.

```yaml
metadata: { id: incident-triage }
spec:
  fields:
    ticket_url: { type: url, locked: true }
  steps:
    - id: open_ticket
```

## spec.vars
type: map of values
as: doc

The run's working variables, with their starting values. Steps change
them with `set`, and expressions read them as `vars.<name>`. A variable's
starting value may be an expression over inputs and other variables
(cycles are refused with `spec.variable_cycle`).

```yaml
metadata: { id: release }
spec:
  inputs:
    version: { type: string, required: true }
  vars:
    result: undetermined
    tag: "release-${{ inputs.version }}"
  steps:
    - id: record
      set: { vars: { result: shipped } }
```

## spec.outputs
type: map of expressions
as: doc

The run's results, computed when it ends. They show on the run page and in
the run report, and a parent's `call` step reads them.

```yaml
metadata: { id: canary }
spec:
  steps:
    - id: measure
      check: true
      outputs:
        error_rate: { type: number, required: true }
  outputs:
    error_rate: "${{ steps.measure.outputs.error_rate }}"
    verdict: "${{ steps.measure.verdict }}"
```

## spec.report
type: template
as: doc

A Markdown summary rendered when the run finishes, with expressions
interpolated. It shows at the top of the run report.

```yaml
metadata: { id: canary }
spec:
  inputs:
    service: { type: string, required: true }
  steps:
    - id: measure
      check: true
  report: |
    ## Canary for ${{ inputs.service }}
    Verdict: ${{ steps.measure.verdict }}
```

# Declarations

A declaration types one value: an input, a field, a step's or signal's
outputs, a `call`'s outputs or a webhook trigger's payload. The same keys
apply everywhere. Declarations are closed: a value with an undeclared
property is refused, and no type accepts `null`.

## spec.inputs.<name>
type: object
as: inputs

One declaration. Only `type` is needed; `{}` alone is not a valid
declaration.

```yaml
service: { type: string, required: true }
```

## spec.inputs.<name>.type
type: string
required: yes
as: inputs

The value's type:

| Type | Values |
| --- | --- |
| `string` | One line of text |
| `text` | Text that may span lines |
| `number` | Any JSON number |
| `integer` | A whole number that fits in 64 bits |
| `boolean` | `true` or `false` |
| `enum` | One of `options` |
| `multi_enum` | A list of distinct `options` |
| `list` | A list of `items` |
| `object` | An object with `properties` |
| `date` | `YYYY-MM-DD` |
| `datetime` | An RFC 3339 timestamp |
| `duration` | A duration such as `90s`, `15m` or `1h30m` |
| `url` | An absolute `http` or `https` URL |
| `email` | An email address |
| `user` | A person in your directory |

Anything else is refused with `spec.unsupported_type`.

```yaml
deadline: { type: datetime, required: true }
```

## spec.inputs.<name>.title
type: string
as: inputs

The label shown on the start form and run page.

```yaml
service: { type: string, title: Service name }
```

## spec.inputs.<name>.description
type: string
as: inputs

Help text shown under the field.

```yaml
service: { type: string, description: The Kubernetes deployment to check. }
```

## spec.inputs.<name>.required
type: boolean
default: false
as: inputs

The value must be given. For an input that means at start; for step
outputs, in the step's report.

```yaml
service: { type: string, required: true }
```

## spec.inputs.<name>.default
type: value
as: inputs

The value used when none is given. It must match the declaration.

```yaml
environment: { type: enum, options: [staging, production], default: staging }
```

## spec.inputs.<name>.options
type: list
as: inputs

The allowed values of an `enum` or `multi_enum`.

```yaml
region: { type: enum, options: [us-east, us-west, eu-west] }
```

## spec.inputs.<name>.min
type: number
as: inputs

The least allowed value: a number's value, a string's length in
characters, or a list's item count.

```yaml
replicas: { type: integer, min: 1, max: 20 }
```

## spec.inputs.<name>.max
type: number
as: inputs

The greatest allowed value, measured like `min`.

```yaml
reason: { type: text, max: 2000 }
```

## spec.inputs.<name>.pattern
type: regular expression
as: inputs

A pattern the whole string must match (it is anchored at both ends). The
dialect runs in linear time: no backreferences or lookaround
(`spec.invalid_pattern`, `spec.pattern_limit`).

```yaml
ticket: { type: string, pattern: "OPS-[0-9]+" }
```

## spec.inputs.<name>.items
type: declaration
as: inputs

The declaration of each item of a `list`.

```yaml
regions: { type: list, items: { type: string }, default: [us-west, eu-west] }
```

## spec.inputs.<name>.properties
type: map of declarations
as: inputs

The properties of an `object`. Objects are closed: undeclared properties
are refused.

```yaml
owner:
  type: object
  properties:
    name: { type: string, required: true }
    slack: { type: string }
```

# Defaults: spec.defaults

Settings every task inherits unless it sets its own.

## spec.defaults
type: object
as: doc

Task defaults. A task's own key replaces the default one by one.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { actor: agent, timeout: 15m, on_failure: handoff }
  steps:
    - id: check_health
    - id: check_errors
```

## spec.defaults.actor
type: human | agent | any
default: any
as: doc

The default `actor` of every task.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { actor: agent }
  steps:
    - id: check_health
```

## spec.defaults.agent
type: object
as: doc

The runner agent of every agent task that names none itself. Publish
checks the name against the project's agent directory.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { actor: agent, agent: { name: external } }
  steps:
    - id: check_health
```

## spec.defaults.agent.name
type: agent name
as: doc

The agent's name, as in `spec.steps[].agent.name`. `external` keeps agent
tasks open to external agents.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { actor: agent, agent: { name: external } }
  steps:
    - id: check_health
```

## spec.defaults.timeout
type: duration
as: doc

The default `timeout` of every task. It applies to tasks only, not to
containers, waits or approvals.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { timeout: 10m }
  steps:
    - id: check_health
```

## spec.defaults.on_failure
type: fail | continue | handoff
default: fail
as: doc

The default `on_failure` of every task. `handoff` here is a common way to
make every agent failure a person's job instead of the run's end.

```yaml
metadata: { id: agent-checks }
spec:
  defaults: { actor: agent, on_failure: handoff }
  steps:
    - id: check_health
```

## spec.defaults.retry
type: object
as: doc

How a failed task is retried. Tasks inherit it; a task's own `retry`
replaces it key by key.

```yaml
metadata: { id: flaky-checks }
spec:
  defaults:
    retry: { max_attempts: 3, backoff: 30s, on: [timeout] }
  steps:
    - id: check_health
```

## spec.defaults.retry.max_attempts
type: positive integer
default: 1
as: doc

The most attempts a task gets, the first included. `1` means no retry.

```yaml
metadata: { id: flaky-checks }
spec:
  defaults:
    retry: { max_attempts: 3 }
  steps:
    - id: check_health
```

## spec.defaults.retry.backoff
type: duration
as: doc

How long to wait before each retry.

```yaml
metadata: { id: flaky-checks }
spec:
  defaults:
    retry: { max_attempts: 3, backoff: 1m }
  steps:
    - id: check_health
```

## spec.defaults.retry.on
type: list of failure types
as: doc

Retry only failures of these types: `timeout`, `failed` or `call_failed`.
Without `on`, any failure is retried. A lapsed lease isn't a failure and
never uses an attempt.

```yaml
metadata: { id: flaky-checks }
spec:
  defaults:
    retry: { max_attempts: 2, on: [timeout] }
  steps:
    - id: check_health
```

## spec.defaults.remind_after
type: duration
as: doc

The default `remind_after` of every task.

```yaml
metadata: { id: weekly-review }
spec:
  defaults: { remind_after: 4h }
  steps:
    - id: review
      actor: human
```

## spec.defaults.remind_every
type: duration
as: doc

The default `remind_every` of every task. Needs `remind_after`.

```yaml
metadata: { id: weekly-review }
spec:
  defaults: { remind_after: 4h, remind_every: 1d }
  steps:
    - id: review
      actor: human
```

## spec.defaults.escalate_after
type: duration
as: doc

The default `escalate_after` of every task. Comes with `escalate_to`.

```yaml
metadata: { id: weekly-review }
spec:
  defaults: { escalate_after: 2d, escalate_to: [team:leads] }
  steps:
    - id: review
      actor: human
```

## spec.defaults.escalate_to
type: list of recipients
as: doc

The default `escalate_to` of every task. Comes with `escalate_after`.

```yaml
metadata: { id: weekly-review }
spec:
  defaults: { escalate_after: 2d, escalate_to: [team:leads, user:dana] }
  steps:
    - id: review
      actor: human
```

# Skills and agents

## spec.skills
type: map of paths
as: doc

The skills the runbook uses, by alias. Each value is a path whose last
segment names a skill uploaded to the project (`runbook skill push`).
Publishing pins each alias to the latest uploaded version of that skill.

```yaml
metadata: { id: error-check }
spec:
  skills:
    error_rates: ./skills/check-error-rates
  steps:
    - id: check
      skill: error_rates
```

## spec.agents
type: object
as: doc

Settings for agents that work this runbook's runs.

```yaml
metadata: { id: post-deploy }
spec:
  agents:
    params: { repo: acme/api, workflow: runbook-resume.yml }
  steps:
    - id: check
      actor: agent
```

## spec.agents.params
type: object
as: doc

Opaque parameters passed untouched to `agent.work_available` webhook
subscribers, so a launcher knows where to start the agent. At most 4,096
bytes encoded (`spec.agents_params_too_large`). The older
`spec.agents.dispatch` was removed (`spec.agents_dispatch_removed`).

```yaml
metadata: { id: post-deploy }
spec:
  agents:
    params: { repo: acme/api, workflow: runbook-resume.yml, ref: main }
  steps:
    - id: check
      actor: agent
```

# Triggers: spec.triggers

Triggers start runs on their own. Only the latest published version's
triggers act, and an archived runbook's never do. Each kind takes at most
16 entries, with IDs unique across both.

## spec.triggers
type: object
as: doc

Schedules and webhook triggers.

```yaml
metadata: { id: nightly-sweep }
spec:
  triggers:
    schedule:
      - { id: nightly, cron: "0 2 * * *", timezone: Europe/London }
  steps:
    - id: sweep
```

## spec.triggers.schedule
type: list
as: doc

Runs on a cron schedule. Each tick starts one run, however many servers
see it, recorded as started by `schedule:<id>`.

```yaml
metadata: { id: weekly-audit }
spec:
  triggers:
    schedule:
      - { id: monday_morning, cron: "0 9 * * MON", timezone: America/New_York }
  steps:
    - id: audit
```

## spec.triggers.schedule[].id
type: string
required: yes
as: doc

The schedule's ID: lowercase letters, digits and `_`, starting with a
letter.

```yaml
metadata: { id: nightly-sweep }
spec:
  triggers:
    schedule: [{ id: nightly, cron: "0 2 * * *" }]
  steps:
    - id: sweep
```

## spec.triggers.schedule[].cron
type: string
required: yes
as: doc

Five fields: minute, hour, day of month, month and day of week, with
lists, ranges, steps and names (`*/15`, `1-5`, `MON,THU`). Refused with
`spec.invalid_cron`.

```yaml
metadata: { id: business-hours-check }
spec:
  triggers:
    schedule: [{ id: every_15m, cron: "*/15 9-17 * * MON-FRI" }]
  steps:
    - id: check
```

## spec.triggers.schedule[].timezone
type: IANA time zone
default: UTC
as: doc

The zone the cron fields are matched in (`spec.invalid_timezone`).

```yaml
metadata: { id: nightly-sweep }
spec:
  triggers:
    schedule: [{ id: nightly, cron: "0 2 * * *", timezone: America/Los_Angeles }]
  steps:
    - id: sweep
```

## spec.triggers.schedule[].inputs
type: map of values
as: doc

Literal inputs for each scheduled run. They must match the runbook's
inputs and give every required input that has no default
(`spec.trigger_input_invalid`).

```yaml
metadata: { id: nightly-sweep }
spec:
  inputs:
    environment: { type: enum, options: [staging, production], required: true }
  triggers:
    schedule:
      - { id: nightly, cron: "0 2 * * *", inputs: { environment: production } }
  steps:
    - id: sweep
```

## spec.triggers.schedule[].catch_up
type: duration
as: doc

How late a tick missed while the server was down may still start, at most
24 hours. Without it, a missed tick starts only if it's a few minutes
late; otherwise it's skipped.

```yaml
metadata: { id: nightly-sweep }
spec:
  triggers:
    schedule: [{ id: nightly, cron: "0 2 * * *", catch_up: 6h }]
  steps:
    - id: sweep
```

## spec.triggers.webhook
type: list
as: doc

Runs started by an HTTP call to
`POST /v1/projects/{project}/runbooks/{runbook}/triggers/{id}`, with a
token that may start runs in the project, in `Authorization` or as
`?token=` for senders that can't set headers. The body is the sender's
own JSON. Runs record `webhook:<id>` as who started them.

```yaml
metadata: { id: post-deploy }
spec:
  inputs:
    service: { type: string, required: true }
  triggers:
    webhook:
      - id: on_deploy
        payload:
          service: { type: string }
          status: { type: string }
        when: "${{ payload.status == 'success' }}"
        inputs: { service: "${{ payload.service }}" }
  steps:
    - id: check
```

## spec.triggers.webhook[].id
type: string
required: yes
as: doc

The trigger's ID, the last segment of its URL.

```yaml
metadata: { id: on-alert }
spec:
  triggers:
    webhook: [{ id: pagerduty }]
  steps:
    - id: triage
```

## spec.triggers.webhook[].payload
type: map of declarations
as: doc

The payload fields expressions may read, as declarations. Fields the
sender adds beyond these are ignored.

```yaml
metadata: { id: on-alert }
spec:
  inputs:
    service: { type: string }
  triggers:
    webhook:
      - id: alertmanager
        payload:
          service: { type: string }
        inputs: { service: "${{ payload.service }}" }
  steps:
    - id: triage
```

## spec.triggers.webhook[].when
type: expression → boolean
as: doc

Start a run only when this is true. It reads `payload` only; a field the
call leaves out makes it false (`spec.trigger_when_type`).

```yaml
metadata: { id: on-deploy }
spec:
  triggers:
    webhook:
      - id: github
        payload:
          environment: { type: string }
        when: "${{ payload.environment == 'production' }}"
  steps:
    - id: verify
```

## spec.triggers.webhook[].inputs
type: map of values or expressions
as: doc

The run's inputs, literal or computed from `payload`. A field the call
leaves out leaves that input to its default.

```yaml
metadata: { id: on-deploy }
spec:
  inputs:
    service: { type: string, default: api }
  triggers:
    webhook:
      - id: github
        payload:
          repository: { type: string }
        inputs: { service: "${{ payload.repository }}" }
  steps:
    - id: verify
```

## spec.triggers.webhook[].dedupe
type: expression → string
as: doc

A key for repeated calls: a call whose `dedupe` equals an earlier one's
returns that run instead of starting another.

```yaml
metadata: { id: on-deploy }
spec:
  triggers:
    webhook:
      - id: github
        payload:
          deployment_id: { type: string }
        dedupe: "${{ payload.deployment_id }}"
  steps:
    - id: verify
```

# Steps: common keys

`spec.steps` is the list of steps. A step's kind is set by the key it
carries: none of the keys below makes it a **task**; `steps`, `parallel`,
`foreach`, `loop`, `switch`, `wait`, `approval`, `set`, `end` or `call`
make it that kind (at most one: `spec.control_conflict`). These keys work
on every kind.

## spec.steps
type: list of steps
required: yes
as: doc

The steps, run in order unless control flow says otherwise. Every step
has an `id`. At most 256 steps with IDs per runbook.

```yaml
metadata: { id: two-steps }
spec:
  steps:
    - id: first
    - id: second
```

## spec.finally
type: list of steps
as: doc

Steps that always run when the run ends: succeeded, failed, timed out or
cancelled. `end` isn't allowed inside (`spec.end_in_finally`).

```yaml
metadata: { id: guarded }
spec:
  steps:
    - id: change
  finally:
    - id: notify
      instructions: Post the outcome to the channel.
```

## spec.steps[].id
type: string
required: yes
as: step

The step's ID: lowercase letters, digits and `_`, starting with a letter,
at most 63 bytes, unique in the runbook. Expressions read the step as
`steps.<id>`.

```yaml
- id: check_error_rate
```

## spec.steps[].title
type: string or template
as: step

The step's title. A task without one gets a title from its ID
(`check_error_rate` becomes "Check error rate").

```yaml
- id: check
  title: Check the 5xx rate
```

## spec.steps[].if
type: expression → boolean
as: doc

Runs the step only when this is true; otherwise the step is `skipped`.

```yaml
metadata: { id: maybe-notify }
spec:
  inputs:
    notify: { type: boolean, default: true }
  steps:
    - id: post_update
      if: "${{ inputs.notify }}"
```

## spec.steps[].on_failure
type: fail | continue | handoff
default: fail
as: step

What a failure of this step does:

- `fail` fails the enclosing group, and with it the run unless a `catch`
  handles it;
- `continue` records the failure and moves on;
- `handoff` (tasks only, `spec.handoff_requires_task`) hands the step to
  a person instead of failing.

```yaml
- id: optional_check
  on_failure: continue
```

## spec.steps[].timeout
type: duration
as: step

The most one attempt of a task may take. Tasks only
(`spec.timeout_requires_task`); waits and approvals have their own
`timeout`.

```yaml
- id: smoke_test
  actor: agent
  timeout: 10m
```

## spec.steps[].labels
type: map of strings
as: step

Your own tags on the step: at most 16, keys of lowercase letters, digits,
`_`, `.`, `/` or `-`, values at most 256 characters
(`spec.invalid_labels`). The engine ignores them; the run returns them with
each step.

```yaml
- id: notify_customer
  labels: { audience: customer, channel: email }
```

# Tasks

A task is work for a person or an agent: a step without a control key.

## spec.steps[].actor
type: human | agent | any
default: any
as: step

Who may claim the task. The server tells people from agents by their
credential, not by what they say.

```yaml
- id: run_smoke_tests
  actor: agent
```

## spec.steps[].assignee
type: recipient or template
as: step

Who the task is for: `team:<slug>`, `user:<name>`, a bare user name, or an
expression. It puts the task in their inbox and notifications. By itself
it's guidance: anyone `actor` allows may still claim it.

```yaml
- id: review_change
  actor: human
  assignee: team:oncall
```

## spec.steps[].assignment
type: guidance | strict
default: guidance
as: step

`strict` makes the `assignee` binding: on a `human` or `any` task only the
assignees may claim it, and on an `agent` task it limits who may take
over. Needs an `assignee` (`spec.strict_assignment_without_assignee`).

```yaml
- id: approve_spend
  actor: human
  assignee: team:finance
  assignment: strict
```

## spec.steps[].instructions
type: Markdown, or an object
as: step

What to do, in Markdown, with expressions interpolated. Give an object to
write different instructions for people and agents.

```yaml
- id: check_dashboard
  instructions: |
    Open the **errors** dashboard and compare the last hour with the
    hour before the deploy.
```

## spec.steps[].instructions.default
type: Markdown
as: step

Instructions for whoever does the step, unless a more specific one is
given.

```yaml
- id: rotate_key
  instructions:
    default: Rotate the API key in the vault.
    agent: Run ./scripts/rotate-key.sh and attach its output.
```

## spec.steps[].instructions.human
type: Markdown
as: step

Instructions shown to a person.

```yaml
- id: rotate_key
  instructions:
    human: Rotate the key in the vault UI and paste the new key's ID.
    agent: Run ./scripts/rotate-key.sh and attach its output.
```

## spec.steps[].instructions.agent
type: Markdown
as: step

Instructions in an agent's work brief.

```yaml
- id: rotate_key
  actor: agent
  instructions:
    agent: Run ./scripts/rotate-key.sh and report the new key ID as an output.
```

## spec.steps[].check
type: boolean
default: false
as: step

Makes the task a check: its report also records a verdict, `pass`,
`warn` or `fail`, independent of its status. Later steps read it as
`steps.<id>.verdict`.

```yaml
- id: error_rate_ok
  check: true
  instructions: Is the 5xx rate under 1%?
```

## spec.steps[].checklist
type: list of strings
as: step

Items to tick while doing the task (`runbook step checklist --tick`).

```yaml
- id: smoke_test
  actor: human
  checklist:
    - Login page loads
    - Dashboard shows data
```

## spec.steps[].outputs
type: map of declarations
as: step

What the task reports, as declarations. A report whose outputs don't
match is refused before anything is recorded. Later steps read
`steps.<id>.outputs.<name>`. At most 64 KiB per step.

```yaml
- id: measure
  outputs:
    error_rate: { type: number, required: true }
    sample_url: { type: url }
```

## spec.steps[].evidence
type: object
as: step

Evidence the report must or may carry. Kinds are `link`, `log`, `file`
and `screenshot`.

```yaml
- id: check_dashboard
  evidence: { required: [link], accepted: [screenshot] }
```

## spec.steps[].evidence.required
type: list of evidence kinds
as: step

Kinds the report must include; a report without them is refused.

```yaml
- id: check_dashboard
  evidence: { required: [link] }
```

## spec.steps[].evidence.accepted
type: list of evidence kinds
as: step

Kinds the report may include. Without `accepted`, any kind is accepted.

```yaml
- id: check_dashboard
  evidence: { accepted: [link, screenshot] }
```

## spec.steps[].skill
type: skill alias
as: doc

Attaches one skill declared in `spec.skills`. The agent fetches its files
with `runbook step skill-file`.

```yaml
metadata: { id: error-check }
spec:
  skills:
    error_rates: ./skills/check-error-rates
  steps:
    - id: check
      actor: agent
      skill: error_rates
```

## spec.steps[].skills
type: list of skill aliases
as: doc

Attaches up to 8 skills (`spec.too_many_skills`), with no duplicates
(`spec.duplicate_skill`). A step can't use both `skill` and `skills`
(`spec.both_skill_and_skills`).

```yaml
metadata: { id: rollback }
spec:
  skills:
    error_rates: ./skills/check-error-rates
    rollback: ./skills/rollback-service
  steps:
    - id: roll_back
      actor: agent
      skills: [error_rates, rollback]
```

## spec.steps[].with
type: map of values
as: step

Parameters for the task, passed to whoever does it (often to its skill).
Values may be expressions.

```yaml
- id: scale
  actor: agent
  with: { replicas: 6, region: eu-west }
```

## spec.steps[].agent
type: object
as: step

Hints for an agent. The server can't see which tools an agent uses, so
these are preferences, never enforced. Limit what an agent can do through
the access you give it.

```yaml
- id: query_logs
  actor: agent
  agent:
    hints: { tools: [grafana], commands: [kubectl logs] }
```

## spec.steps[].agent.name
type: agent name
as: step

The runner agent that works this task (the task needs `actor: agent`). It
is checked at publish against the project's agent directory and recorded
by ID, so renaming or recreating an agent never moves a published runbook
(`spec.agent_unknown`). `external` keeps the task open to external agents.
Without a name, the task follows `spec.defaults.agent`, then the project's
default agent, then stays open to external agents.

```yaml
- id: compare_error_rate
  actor: agent
  agent: { name: external }
```

## spec.steps[].agent.model
type: string
as: step

The model profile the runner agent uses for this task; one the agent
allows (`spec.agent_outside_ceiling`).

```yaml
- id: compare_error_rate
  actor: agent
  agent: { model: deep }
```

## spec.steps[].agent.max_cost_usd
type: number
as: step

The most this task may cost, at most the agent's own limit
(`spec.agent_outside_ceiling`).

```yaml
- id: compare_error_rate
  actor: agent
  agent: { max_cost_usd: 0.5 }
```

## spec.steps[].agent.max_tokens
type: integer
as: step

The most tokens this task may use, at most the agent's own limit.

```yaml
- id: compare_error_rate
  actor: agent
  agent: { max_tokens: 100000 }
```

## spec.steps[].agent.network
type: list of hosts
as: step

The hosts this task may reach: a narrowing of the agent's egress list.

```yaml
- id: compare_error_rate
  actor: agent
  agent: { network: [api.datadoghq.com] }
```


## spec.steps[].connections
type: list of credential names
as: step

The credentials this task may use: a narrowing of its runner agent's,
checked at publish and recorded by ID. Whether a task is destructive
follows from what it can reach: a task that can use a destructive tool or
credential must be marked `destructive` (`spec.destructive_reach`), and a
destructive runner task must come after a person's approval on every path
to it (`spec.destructive_needs_approval`).

```yaml
- id: compare_error_rate
  actor: agent
  connections: [datadog-readonly]
```

## spec.steps[].agent.hints
type: object
as: step

The explicit home for hints, passed to the agent as data.

```yaml
- id: query_logs
  actor: agent
  agent:
    hints: { tools: [grafana] }
```

## spec.steps[].agent.hints.tools
type: list
as: step

Tools the agent should prefer.

```yaml
- id: query_logs
  actor: agent
  agent:
    hints: { tools: [grafana, sentry] }
```

## spec.steps[].agent.hints.commands
type: list
as: step

Commands the agent should prefer.

```yaml
- id: query_logs
  actor: agent
  agent:
    hints: { commands: ["kubectl logs deploy/api --since=1h"] }
```

## spec.steps[].agent.tools
type: list
as: step

The older spelling of `agent.hints.tools`, still accepted. Giving a hint
both ways is refused (`spec.duplicate_agent_hint`).

```yaml
- id: query_logs
  actor: agent
  agent: { tools: [grafana] }
```

## spec.steps[].agent.commands
type: list
as: step

The older spelling of `agent.hints.commands`, still accepted.

```yaml
- id: query_logs
  actor: agent
  agent: { commands: ["kubectl get pods"] }
```

## spec.steps[].destructive
type: boolean
default: false
as: step

Marks a task that changes something you can't easily undo. An approval
that agents may decide, or that decides on its own at a timeout, can't
lead to a destructive task (`spec.agent_approval_destructive`,
`spec.auto_decision_destructive`) unless the task opts in with
`allow_auto_decision`.

```yaml
- id: drop_old_tables
  actor: agent
  destructive: true
```

## spec.steps[].allow_auto_decision
type: boolean
default: false
as: step

Lets an approval's automatic decision (`on_timeout: { decide: … }`) lead
to this destructive task. Agent decisions never may.

```yaml
- id: roll_back
  actor: agent
  destructive: true
  allow_auto_decision: true
```

## spec.steps[].skippable
type: boolean
default: false
as: step

Lets the person or agent doing the task report it `skipped`
(`step.not_skippable` otherwise).

```yaml
- id: update_status_page
  actor: human
  skippable: true
```

## spec.steps[].retry
type: object
as: step

How a failed attempt is retried; the same keys as
[`spec.defaults.retry`](#specdefaultsretry), which it replaces key by key.

```yaml
- id: smoke_test
  actor: agent
  retry: { max_attempts: 3, backoff: 30s, on: [timeout, failed] }
```

## spec.steps[].remind_after
type: duration
as: step

Sends a `step.reminder` once the step has been open this long. Reminders
go to the step's people: approvers, the assignee, whoever claimed it, a
handoff's assignee; with none, the runbook's owners. Tasks, waits and
approvals take it.

```yaml
- id: sign_off
  actor: human
  remind_after: 2h
```

## spec.steps[].remind_every
type: duration
as: step

Repeats the reminder this often. Needs `remind_after`
(`spec.followup_invalid`).

```yaml
- id: sign_off
  actor: human
  remind_after: 2h
  remind_every: 4h
```

## spec.steps[].escalate_after
type: duration
as: step

Escalates once the step has been open this long: a `step.escalated`
notification goes to `escalate_to`, the task's inbox entry adds them, and
they may claim it even under `assignment: strict`. Comes with
`escalate_to`. Tasks and waits take it; approvals escalate with
`approval.escalate_to`.

```yaml
- id: sign_off
  actor: human
  escalate_after: 1d
  escalate_to: [team:eng-leads]
```

## spec.steps[].escalate_to
type: list of recipients
as: step

Who the step escalates to. Comes with `escalate_after`.

```yaml
- id: sign_off
  actor: human
  escalate_after: 1d
  escalate_to: [team:eng-leads, user:dana]
```

# Groups: steps, catch and finally

## spec.steps[].steps
type: list of steps
as: step

Makes the step a group: its steps run in order. A group can `catch` its
failures and run `finally` steps.

```yaml
- id: migrate
  steps:
    - id: back_up
    - id: apply
```

## spec.steps[].catch
type: list of steps
as: step

Steps that run when one of the group's steps fails. When they succeed,
the group succeeds.

```yaml
- id: migrate
  steps:
    - id: apply
  catch:
    - id: restore
      instructions: Restore from the backup.
```

## spec.steps[].catch_on
type: list of failure types
as: step

Makes `catch` handle only these failure types: `timeout`, `failed` or
`call_failed`. Others propagate as if the group had no catch. Needs a
`catch` (`spec.catch_on_without_catch`).

```yaml
- id: migrate
  steps:
    - id: apply
  catch_on: [timeout]
  catch:
    - id: kill_migration
```

## spec.steps[].finally
type: list of steps
as: step

Steps that always run after the group, whether it succeeded or failed.

```yaml
- id: maintenance
  steps:
    - id: drain_traffic
    - id: patch
  finally:
    - id: restore_traffic
```

# Parallel

## spec.steps[].parallel
type: list of steps
as: step

Branches that run at the same time, at most 20 (`spec.limit_exceeded`).
Each branch is one step, often a group.

```yaml
- id: canaries
  parallel:
    - id: canary_us
      check: true
    - id: canary_eu
      check: true
```

## spec.steps[].join
type: all | any | object
default: all
as: step

When the parallel succeeds: `all` branches, `any` one, or
`{ at_least: n }`. Once it has succeeded under `any` or `at_least`, or can
no longer succeed, its open branches are cancelled.

```yaml
- id: mirrors
  join: any
  parallel:
    - id: mirror_a
    - id: mirror_b
```

## spec.steps[].join.at_least
type: positive integer
as: step

The number of branches that must succeed.

```yaml
- id: quorum
  join: { at_least: 2 }
  parallel:
    - id: replica_a
    - id: replica_b
    - id: replica_c
```

## spec.steps[].fail_fast
type: boolean
default: false
as: step

With `join: all`, cancels the other branches as soon as one fails.
(`any` and `at_least` already cancel once decided.)

```yaml
- id: preflight
  fail_fast: true
  parallel:
    - id: disk_space
    - id: replication_lag
```

# foreach

## spec.steps[].foreach
type: object
as: doc

Runs its steps once per item of a list. Inside, the item is a variable
(`item`, or the name in `as`) and `index` is its position. Later steps
read the iterations as `steps.<id>.items`.

```yaml
metadata: { id: per-region }
spec:
  inputs:
    regions: { type: list, items: { type: string }, default: [us-west, eu-west] }
  steps:
    - id: regions
      foreach:
        items: "${{ inputs.regions }}"
        as: region
        max_items: 10
        steps:
          - id: deploy
            instructions: Roll out to ${{ region }}.
```

## spec.steps[].foreach.items
type: expression → list
required: yes
as: doc

The list to iterate. A run whose list is longer than `max_items` fails
the step.

```yaml
metadata: { id: per-host }
spec:
  inputs:
    hosts: { type: list, items: { type: string }, required: true }
  steps:
    - id: hosts
      foreach:
        items: "${{ inputs.hosts }}"
        max_items: 50
        steps:
          - id: patch
            instructions: Patch ${{ item }}.
```

## spec.steps[].foreach.as
type: name
default: item
as: doc

The item variable's name. It can't shadow `inputs`, `vars`, `fields`,
`steps`, `outputs`, `index` or `iteration` (`spec.alias_collision`).

```yaml
metadata: { id: per-region }
spec:
  inputs:
    regions: { type: list, items: { type: string }, required: true }
  steps:
    - id: regions
      foreach:
        items: "${{ inputs.regions }}"
        as: region
        max_items: 10
        steps:
          - id: verify
            instructions: Verify ${{ region }}.
```

## spec.steps[].foreach.key
type: expression → string
as: doc

A stable key per item, shown instead of the index. Keys must be unique.

```yaml
metadata: { id: per-service }
spec:
  inputs:
    services:
      type: list
      items:
        type: object
        properties:
          name: { type: string, required: true }
      required: true
  steps:
    - id: services
      foreach:
        items: "${{ inputs.services }}"
        key: "${{ item.name }}"
        max_items: 20
        steps:
          - id: restart
```

## spec.steps[].foreach.concurrency
type: positive integer
default: 1
as: doc

How many items run at once.

```yaml
metadata: { id: per-host }
spec:
  inputs:
    hosts: { type: list, items: { type: string }, required: true }
  steps:
    - id: hosts
      foreach:
        items: "${{ inputs.hosts }}"
        concurrency: 5
        max_items: 100
        steps:
          - id: patch
```

## spec.steps[].foreach.max_items
type: positive integer
required: yes
as: doc

The most items the step accepts, at most 100. It bounds the run's
worst-case size, which `runbook spec validate` reports.

```yaml
metadata: { id: per-host }
spec:
  inputs:
    hosts: { type: list, items: { type: string }, required: true }
  steps:
    - id: hosts
      foreach:
        items: "${{ inputs.hosts }}"
        max_items: 25
        steps:
          - id: patch
```

## spec.steps[].foreach.steps
type: list of steps
required: yes
as: doc

The steps run for each item.

```yaml
metadata: { id: per-host }
spec:
  inputs:
    hosts: { type: list, items: { type: string }, required: true }
  steps:
    - id: hosts
      foreach:
        items: "${{ inputs.hosts }}"
        max_items: 25
        steps:
          - id: drain
          - id: patch
          - id: undrain
```

# loop

## spec.steps[].loop
type: object
as: doc

Repeats its steps until a condition holds (or while one does), tested
after each iteration. Inside, `iteration` counts from 0. Later steps read
`steps.<id>.iterations`, `.satisfied` and `.exhausted`.

```yaml
metadata: { id: settle }
spec:
  steps:
    - id: stabilize
      loop:
        until: "${{ steps.recheck.outputs.rate <= 0.01 }}"
        max_iterations: 3
        delay: 5m
        steps:
          - id: recheck
            check: true
            outputs:
              rate: { type: number, required: true }
```

## spec.steps[].loop.until
type: expression → boolean
as: doc

Stop once this is true. A loop takes `until` or `while`, not both
(`spec.invalid_shape`).

```yaml
metadata: { id: wait-healthy }
spec:
  steps:
    - id: poll
      loop:
        until: "${{ steps.probe.verdict == 'pass' }}"
        max_iterations: 10
        steps:
          - id: probe
            check: true
```

## spec.steps[].loop.while
type: expression → boolean
as: doc

Keep going while this is true.

```yaml
metadata: { id: drain-queue }
spec:
  steps:
    - id: drain
      loop:
        while: "${{ steps.batch.outputs.remaining > 0 }}"
        max_iterations: 50
        steps:
          - id: batch
            outputs:
              remaining: { type: integer, required: true }
```

## spec.steps[].loop.max_iterations
type: positive integer
required: yes
as: doc

The most iterations, at most 50. A loop that reaches it without its
condition is `exhausted`.

```yaml
metadata: { id: retry-until-green }
spec:
  steps:
    - id: attempts
      loop:
        until: "${{ steps.ci.verdict == 'pass' }}"
        max_iterations: 5
        steps:
          - id: ci
            check: true
```

## spec.steps[].loop.delay
type: duration
as: doc

How long to wait between iterations.

```yaml
metadata: { id: wait-healthy }
spec:
  steps:
    - id: poll
      loop:
        until: "${{ steps.probe.verdict == 'pass' }}"
        max_iterations: 12
        delay: 5m
        steps:
          - id: probe
            check: true
```

## spec.steps[].loop.steps
type: list of steps
required: yes
as: doc

The steps of one iteration.

```yaml
metadata: { id: wait-healthy }
spec:
  steps:
    - id: poll
      loop:
        until: "${{ steps.probe.verdict == 'pass' }}"
        max_iterations: 3
        steps:
          - id: probe
            check: true
```

# switch

## spec.steps[].switch
type: object
as: doc

Runs the steps of the first case whose `when` is true, or the `default`
steps when none is.

```yaml
metadata: { id: route-by-severity }
spec:
  inputs:
    severity: { type: enum, options: [sev1, sev2, sev3], required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.severity == 'sev1' }}"
            steps:
              - id: page_oncall
        default:
          steps:
            - id: open_ticket
```

## spec.steps[].switch.cases
type: list
required: yes
as: doc

The cases, tested in order.

```yaml
metadata: { id: route }
spec:
  inputs:
    env: { type: enum, options: [staging, production], required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.env == 'production' }}"
            steps: [{ id: careful_rollout }]
          - when: "${{ inputs.env == 'staging' }}"
            steps: [{ id: fast_rollout }]
```

## spec.steps[].switch.cases[].when
type: expression → boolean
required: yes
as: doc

The case's condition.

```yaml
metadata: { id: route }
spec:
  inputs:
    error_rate: { type: number, required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.error_rate > 0.05 }}"
            steps: [{ id: roll_back }]
```

## spec.steps[].switch.cases[].steps
type: list of steps
required: yes
as: doc

The steps the case runs.

```yaml
metadata: { id: route }
spec:
  inputs:
    error_rate: { type: number, required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.error_rate > 0.05 }}"
            steps:
              - id: page_oncall
              - id: roll_back
```

## spec.steps[].switch.default
type: object
as: doc

The steps to run when no case matches. Without it, nothing runs.

```yaml
metadata: { id: route }
spec:
  inputs:
    error_rate: { type: number, required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.error_rate > 0.05 }}"
            steps: [{ id: roll_back }]
        default:
          steps: [{ id: keep }]
```

## spec.steps[].switch.default.steps
type: list of steps
required: yes
as: doc

The default steps.

```yaml
metadata: { id: route }
spec:
  inputs:
    ok: { type: boolean, required: true }
  steps:
    - id: route
      switch:
        cases:
          - when: "${{ inputs.ok }}"
            steps: [{ id: celebrate }]
        default:
          steps:
            - id: investigate
```

# wait

## spec.steps[].wait
type: object
as: step

Pauses its branch for a duration (`for`), until a time (`until`) or until
a signal arrives (`signal`): exactly one of the three
(`spec.invalid_shape`).

```yaml
- id: soak
  wait: { for: 30m }
```

## spec.steps[].wait.for
type: duration
as: step

Wait this long, at most 7 days (`spec.limit_exceeded`).

```yaml
- id: soak
  wait: { for: 2h }
```

## spec.steps[].wait.until
type: RFC 3339 time
as: step

Wait until this time, written as an RFC 3339 literal (it is checked as a
timestamp, so a `datetime` input, which is a string, can't be used here).
A target more than 7 days after the wait starts fails the step
(`limit.wait_exceeded`).

```yaml
- id: wait_for_window
  wait: { until: "2026-12-01T09:00:00Z" }
```

## spec.steps[].wait.signal
type: name
as: step

Wait for a named signal, sent with `runbook run signal` or
`POST /v1/executions/{id}/signals/{name}`. Bounded by the run's own
timeout or the wait's `timeout`.

```yaml
- id: connect_store
  wait: { signal: store_connected, timeout: 14d }
```

## spec.steps[].wait.timeout
type: duration
as: step

The most the wait may take; past it the step times out.

```yaml
- id: customer_reply
  wait: { signal: customer_replied, timeout: 3d }
```

## spec.steps[].wait.outputs
type: map of declarations
as: step

The signal payload's schema. A payload that doesn't match is refused
(`422 signal.invalid_payload`) and the step keeps waiting; a valid one
becomes the step's outputs. Signal waits only
(`spec.wait_outputs_requires_signal`).

```yaml
- id: connect_store
  wait:
    signal: store_connected
    outputs:
      store_id: { type: string, required: true }
```

# approval

## spec.steps[].approval
type: object
as: step

A decision between options by named approvers. Later steps read
`steps.<id>.outputs.decision`, `.comment` and `.decided_by`. Agents can't
decide it unless `allow_agents` says so.

```yaml
- id: go_no_go
  approval:
    prompt: Ship to every region?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
```

## spec.steps[].approval.prompt
type: string or template
required: yes
as: step

The question the approvers answer.

```yaml
- id: go_no_go
  approval:
    prompt: Canaries look clean. Continue the rollout?
    options: [continue, roll_back]
    approvers: { teams: [oncall] }
```

## spec.steps[].approval.options
type: list
required: yes
as: step

The choices: strings, or objects with `id`, `title` and `auto`. IDs are
unique and non-empty.

```yaml
- id: go_no_go
  approval:
    prompt: Continue?
    options:
      - { id: continue, title: Continue the rollout }
      - { id: roll_back, title: Roll back now }
    approvers: { teams: [oncall] }
```

## spec.steps[].approval.options[].id
type: string
required: yes
as: step

The option's ID, the value of `outputs.decision`.

```yaml
- id: go_no_go
  approval:
    prompt: Continue?
    options: [{ id: continue }, { id: stop }]
    approvers: { users: [dana] }
```

## spec.steps[].approval.options[].title
type: string
as: step

The option's label. Defaults to its ID.

```yaml
- id: go_no_go
  approval:
    prompt: Continue?
    options: [{ id: continue, title: "Yes, continue" }, { id: stop, title: Stop here }]
    approvers: { users: [dana] }
```

## spec.steps[].approval.options[].auto
type: boolean
default: false
as: step

Marks the option an automatic decision (`on_timeout: { decide: … }`) may
choose (`spec.auto_option_required`).

```yaml
- id: go_no_go
  approval:
    prompt: Errors are up. Roll back?
    options: [keep, { id: roll_back, auto: true }]
    approvers: { teams: [oncall] }
    timeout: 30m
    on_timeout: { decide: roll_back }
```

## spec.steps[].approval.approves
type: list of option IDs
as: step

List the option IDs that can authorize a downstream destructive runner
task. A human decision for one of these options can satisfy the task's
approval requirement. Use a task guard to skip it on other decisions.
New publications require this list on the gate
(`spec.destructive_approval_options`) unless the task's `if` already
requires the gate's decision to equal a specific option.

```yaml
- id: go_no_go
  approval:
    prompt: Drop the old table?
    options: [drop, keep]
    approves: [drop]
    approvers: { teams: [dba] }
```

## spec.steps[].approval.approvers
type: object
as: step

Who may decide: `users` and `teams`. Publishing requires approvers
(`spec.approvers_required`); they resolve against your directory.

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { users: [dana], teams: [release-managers] }
```

## spec.steps[].approval.approvers.users
type: list of user names
as: step

People who may decide.

```yaml
- id: sign_off
  approval:
    prompt: Sign off on the migration?
    options: [approve, reject]
    approvers: { users: [dana, lee] }
```

## spec.steps[].approval.approvers.teams
type: list of team slugs
as: step

Teams whose members may decide.

```yaml
- id: sign_off
  approval:
    prompt: Sign off on the migration?
    options: [approve, reject]
    approvers: { teams: [dba] }
```

## spec.steps[].approval.min
type: positive integer
default: 1
as: step

How many approvers must choose the same option.

```yaml
- id: two_person_rule
  approval:
    prompt: Delete the production bucket?
    options: [delete, keep]
    approvers: { teams: [security] }
    min: 2
```

## spec.steps[].approval.timeout
type: duration
as: step

How long the approval waits before `on_timeout` applies.

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [oncall] }
    timeout: 1h
    escalate_to: [team:eng-leads]
```

## spec.steps[].approval.on_timeout
type: escalate | object
default: escalate
as: step

What happens at `timeout`: `escalate` (to `escalate_to`, or the runbook's
owners) or `{ decide: <option> }`, an automatic decision for an `auto`
option. Needs `timeout` (`spec.on_timeout_requires_timeout`).

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [oncall] }
    timeout: 2h
    on_timeout: escalate
    escalate_to: [team:eng-leads]
```

## spec.steps[].approval.on_timeout.decide
type: option ID
as: step

The option chosen automatically at the timeout. It must be an `auto`
option, and it can't lead to a destructive task unless that task sets
`allow_auto_decision`.

```yaml
- id: go_no_go
  approval:
    prompt: Errors are up. Roll back?
    options: [keep, { id: roll_back, auto: true }]
    approvers: { teams: [oncall] }
    timeout: 15m
    on_timeout: { decide: roll_back }
```

## spec.steps[].approval.escalate_to
type: list of recipients
as: step

Who gets the approval when it escalates at its timeout. Without it, the
runbook's `metadata.owners` (`spec.escalation_recipients_required` when
there are none).

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [oncall] }
    timeout: 30m
    escalate_to: [team:eng-leads]
```

## spec.steps[].approval.escalation_timeout
type: duration
default: the approval's timeout
as: step

How long the escalated approval waits before it expires.

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [oncall] }
    timeout: 30m
    escalate_to: [team:eng-leads]
    escalation_timeout: 2h
```

## spec.steps[].approval.on_expired
type: pause | fail
default: pause
as: step

What an approval that expires undecided does: `pause` the run for a
person to pick up, or `fail` the step.

```yaml
- id: go_no_go
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [oncall] }
    timeout: 30m
    escalate_to: [team:eng-leads]
    on_expired: fail
```

## spec.steps[].approval.allow_agents
type: boolean
default: false
as: step

Lets an agent decide. Needs `min: 1`
(`spec.agent_approval_requires_min_one`), and no destructive task may
follow from the decision (`spec.agent_approval_destructive`). An agent's
decision is recorded apart from human signatures. Add `agent` to have a
runner agent decide.

```yaml
- id: triage_choice
  approval:
    prompt: Which runbook fits this alert?
    options: [restart, scale_up, escalate]
    approvers: { teams: [oncall] }
    allow_agents: true
```

## spec.steps[].approval.agent
type: object
as: step

A runner agent that decides this approval when no person has. Needs
`allow_agents: true` (`spec.agent_requires_allow_agents`). The agent's
decision run is read-only, people can decide at any time and the first
decision wins, and the approval still can't lead to a destructive task
(`spec.agent_approval_destructive`).

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer, after: 15m }
```

## spec.steps[].approval.agent.name
type: agent name
required: yes
as: step

The runner agent that decides. It is checked at publish against the
project's agent directory and recorded by ID (`spec.agent_unknown`).

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer }
```

## spec.steps[].approval.agent.model
type: string
as: step

The model profile the agent decides with; one the agent allows
(`spec.agent_outside_ceiling`).

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer, model: deep }
```

## spec.steps[].approval.agent.max_cost_usd
type: number
as: step

The most the decision may cost, at most the agent's own limit
(`spec.agent_outside_ceiling`).

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer, max_cost_usd: 0.25 }
```

## spec.steps[].approval.agent.max_tokens
type: integer
as: step

The most tokens the decision may use, at most the agent's own limit.

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer, max_tokens: 50000 }
```

## spec.steps[].approval.agent.after
type: duration
as: step

How long the approval waits for a person before the agent starts. Without
it, the agent starts as soon as the approval opens. People can still decide
while it works.

```yaml
- id: release_gate
  approval:
    prompt: Ship it?
    options: [ship, hold]
    approvers: { teams: [release-managers] }
    allow_agents: true
    agent: { name: release-reviewer, after: 15m }
```

# set and end

## spec.steps[].set
type: object
as: doc

Sets variables and fields. Values may be expressions.

```yaml
metadata: { id: classify }
spec:
  fields:
    severity: { type: string }
  vars:
    result: unknown
  steps:
    - id: record
      set:
        fields: { severity: major }
        vars: { result: degraded }
```

## spec.steps[].set.vars
type: map of values
as: doc

New values for variables declared in `spec.vars`.

```yaml
metadata: { id: tally }
spec:
  vars: { outcome: pending }
  steps:
    - id: done
      set: { vars: { outcome: done } }
```

## spec.steps[].set.fields
type: map of values
as: doc

New values for fields declared in `spec.fields`; each must match its
declaration.

```yaml
metadata: { id: incident }
spec:
  fields:
    severity: { type: enum, options: [sev1, sev2, sev3] }
  steps:
    - id: escalate
      set: { fields: { severity: sev1 } }
```

## spec.steps[].end
type: object
as: step

Ends the run early with a status. Not allowed in `finally`
(`spec.end_in_finally`).

```yaml
- id: nothing_to_do
  end: { status: succeeded, message: No certificates expire this month. }
```

## spec.steps[].end.status
type: succeeded | failed | cancelled
required: yes
as: step

The run's final status.

```yaml
- id: stop
  end: { status: cancelled }
```

## spec.steps[].end.message
type: string or template
as: step

Why the run ended, shown on the run page.

```yaml
- id: stop
  end: { status: failed, message: The preflight found replication lag. }
```

# call

## spec.steps[].call
type: object
as: step

Runs another published runbook of the same project as a child run and
waits for it; the step takes the child's outcome. Calls nest at most 3
deep, and a runbook can't reach itself (`spec.call_depth_exceeded`,
`spec.call_cycle`). Cancelling the run cancels the child.

```yaml
- id: verify
  call:
    runbook: post-deploy-checks
    with: { service: api }
```

## spec.steps[].call.runbook
type: runbook ID
required: yes
as: step

The child runbook's ID (its kebab-case slug). It resolves when you
publish (`spec.call_unresolved`).

```yaml
- id: verify
  call: { runbook: post-deploy-checks }
```

## spec.steps[].call.version
type: positive integer
as: step

Pins a published version. Without it, the version that's current when you
publish is pinned.

```yaml
- id: verify
  call: { runbook: post-deploy-checks, version: 4 }
```

## spec.steps[].call.with
type: map of values
as: step

The child's inputs; values may be expressions. They must match the
child's inputs (`spec.call_unknown_input`, `spec.call_missing_input`,
`spec.call_input_type`).

```yaml
- id: verify
  call:
    runbook: post-deploy-checks
    with: { service: api, environment: production }
```

## spec.steps[].call.outputs
type: map of declarations
as: step

The child's outputs this step exposes, read as
`steps.<id>.outputs.<name>`. Each must be one of the child's outputs, of
a matching type (`spec.call_unknown_output`, `spec.call_output_type`).

```yaml
- id: verify
  call:
    runbook: post-deploy-checks
    with: { service: api }
    outputs:
      error_rate: { type: number }
```
