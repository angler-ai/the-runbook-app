# The Runbook

**Runbooks that people and AI agents run together.** Write a procedure once as a short YAML file. Agents claim and do their steps, people approve and take over where judgment matters, and every run leaves a readable record of who did what, with logs, outputs and evidence.

Open source and self-hosted: one container with PostgreSQL, or your own Cloudflare account. Built by [Angler AI](https://getangler.ai).

> **Status:** pre-release. The server, CLI, MCP server and web UI work end to end; the first release, `v0.1.0`, is coming.

---

## What it looks like

### Post-deploy checks, done by an agent

Every field except the IDs is optional. Titles come from the IDs, and every step defaults to whoever is available, a person or an agent.

```yaml
metadata: { id: post-deploy-checks }
spec:
  defaults: { actor: agent }
  inputs:
    service: { type: string, required: true }
  steps:
    - id: check_health
      instructions: Call /healthz on ${{ inputs.service }} three times; pass on 200 within 500 ms.
    - id: compare_error_rate
      instructions: Compare the 5xx rate with the hour before the deploy.
    - id: summarize
```

Start it from CI after a deploy:

```sh
runbook run start post-deploy-checks --project prod --input service=api
```

Your agent (Claude Code, Codex, Cursor, or a script) is told there's work through a webhook or MCP. It claims each step, reports its result and attaches logs. You get a run page that reads like a document, and you can export it as Markdown, JUnit or PDF.

### A step that runs a real command

```sh
runbook step exec sr_01H… -- ./scripts/smoke-test.sh --env prod
```

The CLI holds the step's lease while the command runs, streams its output into the step's log with secrets redacted, and reports success or failure from the exit code.

### People and agents in one flow

```yaml
- id: canary
  parallel:
    - { id: canary_us, check: true, instructions: Watch us-east at 5% for 15 minutes. }
    - { id: canary_eu, check: true, instructions: Watch eu-west at 5% for 15 minutes. }
- id: go_no_go
  approval:
    prompt: Ship ${{ inputs.version }} to every region?
    options: [ship, hold]
    approvers: { teams: [oncall] }
- id: rollout
  foreach:
    items: "${{ inputs.regions }}"
    as: region
    concurrency: 1
    max_items: 10
    steps:
      - id: deploy
        destructive: true
        instructions: Roll out to ${{ region }}.
```

Agents do the canaries in parallel, a person on call approves, and the rollout goes one region at a time. Destructive steps and approvals keep agents from doing what should need a human.

### Customer onboarding with your own app in the loop

Steps can wait for a signal, so your product marks them done when the customer acts:

```yaml
- id: connect_shopify
  wait: { signal: shopify_connected, timeout: 14d }
```

```sh
curl -X POST "$RUNBOOK/v1/executions/$RUN/signals/shopify_connected" \
  -H "Authorization: Bearer $TOKEN" -d '{"connected_by": "…"}'
```

Internal people, agents, customers and APIs each advance their part of the same run, in any order where it doesn't matter.

### Runs that start themselves

A schedule or a call from another system starts a run of the latest published version. Set them up in the editor's Triggers form, or in YAML:

```yaml
spec:
  triggers:
    schedule:
      - { id: weekday_check, cron: "0 9 * * 1-5", timezone: America/New_York, inputs: { service: api } }
    webhook:
      - id: on_deploy
        payload: { status: { type: string }, service: { type: string } }
        when: "${{ payload.status == 'success' }}"
        inputs: { service: "${{ payload.service }}" }
```

### Ask your agent to write the runbook

Over MCP, an agent can draft a runbook (`save_runbook_draft`), validate it and show you the diff. A person publishes it.

---

## Capabilities

**Runbook language**
- YAML with steps of many kinds:
  - tasks, checks and approvals;
  - `parallel` branches with `join: all | any | at_least`;
  - `switch` and `if`, `foreach`, and `loop` with `until` or `while`;
  - `wait` for signals, `set` for variables and fields;
  - `call` to run another runbook as a child;
  - `end`, plus `catch` and `finally`.
- Typed inputs, outputs and fields; expressions; per-step timeouts; retries by failure type, catches and handoffs; reminders and escalation; concurrency keys.
- Triggers: schedules (cron in a time zone) and webhooks that start runs on their own.
- Only IDs are required. A runbook can be three lines long.

**People and agents**
- Each step is for a person, an agent or anyone. An assignee is guidance unless a step makes it strict.
- People can take over an agent's step, and the takeover is recorded.
- Agents work through the CLI (`runbook step next / claim / report / exec`) or MCP, under leases with heartbeats, so a crashed agent never holds a step forever.
- Agent skills can be attached to steps.
- Memory: agents and people save what they learn per runbook, and the next run's steps see it. People can lock, restore, accept or archive entries.

**Web app**
- Home, My tasks and the runbooks list, with people's photos and project images.
- A graph-first runbook page, its runs beside the selected run, and a visual builder (with a Triggers form) kept in sync with the YAML.
- A live run page with an execution graph, plus a document-style run log.
- Evidence uploads with thumbnails.
- Account settings, organization and project administration, webhooks, skills and daily archives.

**Integrations**
- **MCP server:** local over stdio, or remote at `/mcp` with OAuth sign-in for Claude, Cursor and other clients.
- **Webhooks:** signed, with delivery logs, redelivery and secret rotation.
- **Reports:** JSON, Markdown, JUnit and PDF.
- **CLI:** for scripts, CI and agents. `runbook login https://your-server` asks nothing else; contexts hold several servers, each with a default project. `runbook push`, `diff` and `publish` keep runbooks in git.
- **REST API:** full, with an OpenAPI contract (`api/openapi.draft.json`).

**Security and control**
- **Sign-in:** email codes or Google, never passwords. Invitations only, optionally limited to your email domain.
- **Roles:** organization roles (owner, admin, member) and project roles (viewer, operator, editor, admin). Organization owners and admins are admins of every project.
- **API tokens:** scoped to chosen permissions and projects.
- **Secret redaction** in logs.
- **An audit trail:** every run event records who did it.

**Operations**
- **Self-host with Docker:** one container and PostgreSQL, with several replicas behind a load balancer.
- **Run it on any container platform:** Cloud Run, Kubernetes, ECS. There's Terraform for GKE with Cloud SQL.
- **Run it on Cloudflare:** Workers, Durable Objects, D1 and R2 (needs Workers Paid).
- **File storage:** files stay in Postgres by default, or go to GCS or S3.
- **Daily archives and retention**, and project import and export.
- **One-command updates** with automatic rollback.
- **Limits you set:** an optional cap on steps per run (`RUNBOOK_LIMIT_STEP_RUNS`), run size, request size and monthly runs.

---

## Get started

| You want to | Read |
| --- | --- |
| Run it with Docker | [`deploy/README.md`](deploy/README.md) |
| Run it on Cloud Run, Kubernetes or GKE with Cloud SQL | [`deploy/platforms.md`](deploy/platforms.md), [`deploy/terraform/gcp-gke/`](deploy/terraform/gcp-gke/) |
| Run it on your Cloudflare account | [`cloudflare/README.md`](cloudflare/README.md) |
| Set up people, sign-in and agents | [`docs/install.md`](docs/install.md) |
| Learn the runbook language | [`docs/pilot-profile.md`](docs/pilot-profile.md) |
| Keep runbooks in a Git repository | [`docs/git-sync.md`](docs/git-sync.md) |
| Teach your agent | [`skills/therunbook/`](skills/therunbook/), or the Claude Code plugin in [`plugins/claude-code/`](plugins/claude-code/) |

## Develop

The workspace is Rust: engine, API, server, CLI, MCP and the store backends. The web app is React and Vite in `web/`, and the sign-in service is TypeScript in `auth/`. To run the checks CI runs:

```sh
scripts/check.sh
```

To regenerate the API contract after changing shared types:

```sh
cargo run -p runbook-contract --bin export -- api/openapi.draft.json
```

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the rules a change must follow, including the contributor license agreement a first pull request signs.

## License

The Runbook is owned by Angler AI, Inc. and licensed in two parts ([LICENSING.md](LICENSING.md)):

- **AGPL-3.0-only** ([LICENSE](LICENSE)): the server, web app, CLI, MCP server, sign-in service and Cloudflare edition.
- **Apache-2.0**: the runbook language and expression crates, the API data types and OpenAPI contract, the JSON Schema, the Claude Code plugin and agent skill, and the examples, so you can build on them in anything.
