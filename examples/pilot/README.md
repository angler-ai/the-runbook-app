# Pilot install kit

Scripts that automate the [clean-install guide](../../docs/install.md) for
the native pilot. Run them from the repository root after the server is up
and bootstrapped (guide sections 1 and 2). They use the `runbook` CLI for
everything it covers, and `curl` against the REST API only for what it does
not cover yet (directory lists, service tokens, skill uploads).

| File | What it does |
| --- | --- |
| [`setup.sh`](setup.sh) | Creates the `pilot` project; Alice, Bob and Carol as project operators; teams `oncall` (Alice, Bob) and `eng-leads` (Carol); a launcher service token; the flagship's two skills; publishes `conformance/spec/post-deploy.yaml`; subscribes the recording receiver (every event) and the GitHub Actions relay (`agent.work_available`); and gives each person their own token. Safe to re-run. |
| [`run-flagship.sh`](run-flagship.sh) | Runs the flagship end to end: Alice starts it, the launcher mints an execution-scoped pair, an agent session does the agent steps with `runbook step`, Alice ticks the human checklist, Bob (on call) decides the approval with `runbook step decide`. |
| [`recording-receiver.py`](recording-receiver.py) | A webhook receiver that verifies `Runbook-Signature` (constant-time compare, five-minute staleness window), deduplicates by event ID and prints each event's type and ID. With `RECEIVER_MOCK_GITHUB=1` and `RECEIVER_GITHUB_TOKEN` it also stands in for the GitHub API behind the relay (that bearer token only). Listens on 127.0.0.1 by default. |
| [`seed-first-token.sh`](seed-first-token.sh), [`seed-first-token.sql`](seed-first-token.sql) | **Headless fallback:** give a CLI-only person their first token. See below. |
| [`pilot-json.py`](pilot-json.py) | Reads `--json` output: prints one field (`get`, `find`), or stores secret fields in 0600 files and prints the rest (`keep`). |
| [`skills/`](skills/) | The two skills the flagship names (`./skills/check-error-rates`, `./skills/rollback-service`). |

## Run it

```sh
export PILOT_DIR="$HOME/runbook-pilot"            # private directory for secrets (0700)
export RUNBOOK_HOST=http://127.0.0.1:8080
export RUNBOOK_BOOTSTRAP_TOKEN_FILE="$PILOT_DIR/owner.pat"   # written in guide section 2
export DATABASE_URL='postgres://...'              # native path; or PILOT_PSQL, see below
examples/pilot/setup.sh
```

The seeds read `DATABASE_URL` from the environment and hand `psql` its
parts as `PGHOST`, `PGUSER`, `PGPASSWORD`, ... so the password never
appears on a command line. With the compose stack, reach Postgres through
the container instead (no password needed):
`PILOT_PSQL="docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T postgres psql -U runbook -d runbook"`
(use your `POSTGRES_USER` and `POSTGRES_DB` if you changed them). A
`PILOT_PSQL` containing a URL or password is refused.

`setup.sh` adopts an existing user named Alice, Bob or Carol only if an
earlier run recorded it in `$PILOT_DIR/state.env`, or with
`PILOT_REUSE_USERS=1`; otherwise it stops rather than give a real person's
account to the pilot.

Then start the receiver and the relay in their own terminals (guide
section 7) and run the flagship:

```sh
examples/pilot/run-flagship.sh                     # PILOT_SCENARIO=healthy for the short path
```

## Secrets

Nothing here prints a secret. Every token and signing secret is written once
to a 0600 file in `$PILOT_DIR` (`owner.pat`, `alice.seed`, `alice.pat`,
`launcher.svc`, `receiver.whsec`, `relay.whsec`, ...) and read from there:
into a CLI's environment (`RUNBOOK_TOKEN`), its private login store
(`runbook login --token-stdin`) or a curl header file, never onto a command
line. Each person gets their own CLI config directory under
`$PILOT_DIR/config/<name>`. `state.env` holds only IDs.

## Headless fallback: a person's first token

People normally join by email invitation and sign in with a code
(docs/install.md, sections 2 and 4). The CLI cannot sign in that way yet, and
`POST /v1/tokens` only issues tokens to the caller, so for CLI-only people
(this walkthrough) and installs without sign-in the operator seeds one
token per person with
`seed-first-token.sh`, which generates the secret locally, inserts only its
SHA-256 (`seed-first-token.sql`), and limits the seed to one project and
`executions:read|start|act` plus `tokens:manage` for 90 days. Each person
then mints their own working token with `runbook token create`. This is
exactly how the C4 flagship gate provisions people; it is not a production
onboarding path.
