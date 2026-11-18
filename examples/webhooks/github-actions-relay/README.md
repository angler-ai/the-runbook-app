# GitHub Actions relay

Turns `agent.work_available` into a GitHub Actions `workflow_dispatch` of
[`runbook-resume.yml`](runbook-resume.yml), which mints execution-scoped
credentials and resumes an agent on the execution.

```
The Runbook --agent.work_available--> relay.py --workflow_dispatch--> runbook-resume.yml
                                      (verify, dedupe)                (runbook run token, agent)
```

## What the relay does

For each POST it:

1. checks `Runbook-Signature` against `RUNBOOK_WEBHOOK_SECRET` and rejects
   a timestamp more than five minutes old (`401`);
2. acknowledges other event types with `204`;
3. reads the target from the runbook's `spec.agents.params` (`repo`,
   `workflow`, optional `ref`), falling back to `GITHUB_REPOSITORY`,
   `RUNBOOK_WORKFLOW` and `GITHUB_REF`. Params are written by project
   editors, so all three are allow-listed and anything else is refused with
   `403` before GitHub is called: `RELAY_ALLOWED_REPOS` (default
   `GITHUB_REPOSITORY`), `RELAY_ALLOWED_WORKFLOWS` (default
   `RUNBOOK_WORKFLOW`, i.e. `runbook-resume.yml`) and `RELAY_ALLOWED_REFS`
   (default `GITHUB_REF`, i.e. `main`). A runbook cannot make your token
   dispatch `deploy-prod.yml` or run on another branch;
4. deduplicates on the event ID: a redelivery answers `200
   {"duplicate": true}` without dispatching again;
5. calls `POST {GITHUB_API_URL}/repos/{owner}/{repo}/actions/workflows/{workflow}/dispatches`
   with `{"ref": ..., "inputs": {"execution_id": "exe_...", "event_id": "evt_..."}}`.
   If GitHub fails, it answers `502` and does not remember the event, so The
   Runbook's retry dispatches it later.

Malformed requests (bad or negative `Content-Length`, non-ASCII timestamps
or signatures) are refused with `400`/`401`/`413`. Handled event IDs live in memory; set `RELAY_DEDUP_FILE` to keep them across
restarts (or replace `Seen` with your own store). The workflow's
`concurrency` group is a second guard: one job per execution at a time.

## Run it

```sh
export RUNBOOK_WEBHOOK_SECRET=whsec_...      # shown once when you create the subscription
export GITHUB_TOKEN=...                      # fine-grained token or GitHub App token with actions:write
export GITHUB_REPOSITORY=acme/api            # default target and allow-list
python3 relay.py                             # listens on 0.0.0.0:8080
```

Put it behind HTTPS (a platform router, load balancer or tunnel) and
subscribe it:

```sh
runbook webhook create --name github-relay --url https://relay.example.com/ \
  --event agent.work_available
```

It deploys as a container (`python:3-slim`, `CMD ["python3", "relay.py"]`) or
ports directly to a Cloudflare Worker: the steps above are the whole job
(HMAC with Web Crypto, dedupe in KV or a Durable Object, `fetch` to GitHub).

## The workflow

Copy [`runbook-resume.yml`](runbook-resume.yml) to `.github/workflows/` of the
dispatched repository and set `secrets.RUNBOOK_SERVICE_TOKEN` (a project
service token with `executions:act`), `vars.RUNBOOK_HOST` and your agent's
key. The workflow:

- runs one job per execution (`concurrency: group: runbook-${{ inputs.execution_id }}`);
- passes inputs to scripts only through `env` (no expression interpolation
  into shell);
- checks out with `persist-credentials: false` and pins `actions/checkout`
  by commit SHA and the agent CLI by `vars.CLAUDE_CODE_VERSION`;
- mints credentials in a step that alone sees the service token
  (`runbook run token "$EXECUTION_ID"` stores the pair, prints no secret);
- starts the agent without `RUNBOOK_TOKEN`, so it holds only the
  execution-scoped pair, which refreshes itself and is revoked when the run
  ends. The pair is stored under `$RUNNER_TEMP` (`XDG_CONFIG_HOME`), outside
  the workspace, and the agent may only use the runbook CLI and read files
  inside the workspace (`Read(./**)`), so it cannot read the stored pair. The agent reads the resume brief and exits when no work is ready.

Secrets are never echoed or put in prompts.
