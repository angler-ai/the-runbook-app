# The Runbook

**A system of record for operations, run by agents and people.** Write a
procedure once as a short YAML file. Agents do their steps, people approve
and take over where judgment matters, and every run leaves a readable record
of who did what, with logs, outputs and evidence.

Open source and self-hosted on containers with PostgreSQL or on your own
Cloudflare account. Built by [Angler AI](https://getangler.ai).

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

Connect an agent through the CLI or MCP, or target a project agent on your
own runner. It claims each step, reports its result and attaches evidence.
The run page reads like a document and exports as Markdown, JSON, JUnit or PDF.

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
    approves: [ship]
    approvers: { teams: [oncall] }
- id: rollout
  foreach:
    items: "${{ inputs.regions }}"
    as: region
    concurrency: 1
    max_items: 10
    steps:
      - id: deploy
        if: "${{ steps.go_no_go.outputs.decision == 'ship' }}"
        destructive: true
        instructions: Roll out to ${{ region }}.
```

Agents do the canaries in parallel. The rollout runs one region at a time
if a person on call chooses `ship`.

## Capabilities

- **Runbook language.** Steps, checks and approvals, parallel work, loops,
  retries, handoffs and waits for signals. Schedules and webhooks start runs.
  Keep YAML in Git or edit it in the visual builder.
- **People and agents.** Assign steps, take over an agent's work, or approve from
  the web app or Slack. Leases keep crashed agents from holding work forever.
  The CLI, MCP and REST API let agents and scripts work steps, attach evidence
  and turn what they learn into runbook memory for human review.
- **Optional runner.** Set up model providers, MCP servers, credentials, policy
  and budgets at the organization level. Configure agents in each project and
  target them from steps. The runner starts a sandbox when an agent needs to
  run code. A person must approve destructive runner steps. Routine approvals
  can opt in to a read-only agent decision.
- **Security.** Passkeys, authenticator apps and backup codes, with organization
  MFA policy and fresh checks for sensitive actions. Tokens are scoped and
  activity is audited. Runner credentials are sealed from the browser or CLI
  to the registered vault identity, encrypted at rest and released to the
  gateway for a live lease and a bound destination. The gateway applies
  credentials outside the sandbox.

## Get started

| You want to | Read |
| --- | --- |
| Run it with Docker and PostgreSQL | [Docker quick start](https://docs.therunbook.app/getting-started/docker/) |
| Run it on Kubernetes or GKE with Cloud SQL | [Container platforms](deploy/platforms.md) and [GKE quick start](https://docs.therunbook.app/getting-started/gke/) |
| Run it in your Cloudflare account | [Cloudflare quick start](https://docs.therunbook.app/getting-started/cloudflare/) |
| Write your first runbook | [First runbook](https://docs.therunbook.app/getting-started/first-runbook/) |
| Connect your agent or install the runner | [Agents](https://docs.therunbook.app/agents/) and [runner setup](https://docs.therunbook.app/runner/setup/) |
| Install the CLI or configure sign-in | [CLI quick start](https://docs.therunbook.app/getting-started/cli/) and [multi-factor sign-in](https://docs.therunbook.app/security/multi-factor/) |

## Upgrade to v0.1.1

1. Back up and update the core and sign-in service together. Use
   `deploy/update.sh` for Docker Compose or `npm run update` from
   `cloudflare/`. Check that `GET /v1/meta` reports the new release and
   `runner_contract_version` of at least 4 before updating the runner.
2. If you already run a runner, stop old runner API instances before applying
   its database migrations. Update the runner and register its vault identity
   before entering credentials from the updated browser or CLI.
3. Turn on organization MFA after verifying sign-in and reviewing who the
   policy covers.

See the [release notes](https://docs.therunbook.app/changelog/#011) and
[upgrade guide](https://docs.therunbook.app/operating/upgrades/) for the
edition-specific steps.

---

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
