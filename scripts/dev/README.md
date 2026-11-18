# Local review stack

`scripts/dev/review.sh` brings up the web UI with realistic data in one command, for reviewing UI
changes by hand or taking screenshots:

```sh
scripts/dev/review.sh            # start, print the sign-in, run until Ctrl-C (then tear down)
scripts/dev/review.sh --detach   # start, print the sign-in, leave it running
scripts/dev/review.sh stop       # stop a detached run, drop its database, remove its state
```

It builds `runbook-server` and the `runbook` CLI (`cargo build -p runbook-server -p runbook-cli
--locked`; set `CARGO_TARGET_DIR` as usual, or `REVIEW_SKIP_BUILD=1`), resets its own database
`rb_test_review_ui` on the local Postgres (admin connection `REVIEW_PG_ADMIN_URL`, default
`postgres://runbook_dev@localhost:5432/runbook_dev`; no other database is touched), starts the
server on `127.0.0.1:18080` (a free port if that is taken), bootstraps the owner, seeds, and starts
Vite on `http://127.0.0.1:5173` (refused if 5173 is in use). Every start resets everything; a
previous run is stopped first.

Sign in by opening `http://127.0.0.1:5173` and pasting the browser token from
`$TMPDIR/runbook-review-ui/browser.token` (`pbcopy < "$TMPDIR/runbook-review-ui/browser.token"`).
It is the owner's (Riley Chen's) narrow personal token. The state directory (0700) also holds logs,
pid files, `state.json` (IDs) and `browser.header` for `curl -H @…`. Secrets live only in 0600 files
there and are never printed.

The script creates that directory itself and marks it (`.runbook-review-ui`); it refuses to use,
empty or delete a directory without the marker, so `REVIEW_STATE_DIR` can never point it at
something else. A state directory from before the marker existed is refused too: stop its processes
and remove it by hand. Pid files record each process's start time with its pid, and only a process
with that pid, start time and command is signalled.

A second stack beside the first (UI-2): set `REVIEW_STATE_DIR` to a fresh directory,
`REVIEW_DB_NAME` (another `rb_test_…` database; anything else is refused), `REVIEW_API_PORT` and
`REVIEW_VITE_PORT`, and run `review.sh stop` with the same variables.

Theme override: `REVIEW_THEME_ACCENT`, `REVIEW_THEME_ACCENT_SECONDARY`, `REVIEW_THEME_DARK_ACCENT`
and `REVIEW_THEME_DARK_ACCENT_SECONDARY` (`#rrggbb`) are passed to the server as `RUNBOOK_THEME_*`.

## What is seeded

`seed-review.mjs` uses only the CLI and the public API (no SQL): people and teams (`oncall`,
`customer-success`, `solutions`), project `ops` ("Platform Ops") with eleven runbooks from
`review-runbooks/` (earlier published versions in `versions/`, unpublished draft changes in
`drafts/`), and project `sandbox` ("Sandbox") with none. Agents act through execution-scoped pairs
minted by a service token (`runbook run token`), as `examples/pilot/run-flagship.sh` does; the
owner does the people's steps and decides approvals; the service token sends the customer's signal
and reports the ingestion step. `keep-claimed.mjs` heartbeats the one run an agent is mid-way
through so its claim outlives the 10-minute lease. `release-train` (UI-2) is the complex graph:
a group with parallel checks and a catch, a risk switch with approvals, a canary with `join: any`,
a per-region `foreach` with a nested `loop`, a `call` to `post-deploy-checks` (so it is published
after it), a person's task, a customer signal and a `finally`; its one high-risk run waits on the
change advisory review (team `oncall`). UI-3 adds, for the run page: a low-risk release run end to
end first (the default branch, a `join: any` canary, three regions with eu-west settling on its
second loop iteration, the call's child `post-deploy-checks` run, a person, the customer's signal
and the `finally`), a medium-risk release queued behind the waiting one, a realistic agent log on
the failing post-deploy check, and Northwind's billing step claimed mid-way.

Runs are stamped when the seed runs, so they show as a few minutes old.
