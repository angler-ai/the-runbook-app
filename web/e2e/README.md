# Web e2e and accessibility (M6)

Playwright suites that drive the web UI against a real `runbook-server` on an owned Postgres
database, seeded as the pilot org (Alice, Bob, Carol; teams `oncall` and `eng-leads`; the flagship
`post-deploy` published).

## Run it

One-time setup:

```sh
cd web && npm ci --ignore-scripts && npx playwright install chromium
```

Every run builds the Rust binaries it uses first (see below), so there is no separate `cargo build`
step and a stale server can never answer for the code under test.

Then, from `web/`:

```sh
npm run e2e                          # the whole suite (not part of `npm run check`)
npm run e2e -- e2e/flagship.spec.ts  # any Playwright arguments pass through
npm run e2e -- --serve               # start the stack and wait; Ctrl-C tears it down
npm run e2e:container                # the same specs against the packaged image (Docker)
npm run e2e:walkthrough              # the screenshot walkthrough (container mode), see below
```

With `--serve`, the harness prints the `E2E_STATE=… E2E_BASE_URL=… npx playwright test` line to run
specs against the live stack from another shell while you write them.

**The builder spec against a review stack (UI-4).** `e2e/builder.spec.ts` also runs against a
stack from `scripts/dev/review.sh` (its owner's browser token, project `ops`), without the harness:
`E2E_REVIEW_STATE_DIR=<the stack's state dir> E2E_BASE_URL=http://127.0.0.1:<its Vite port> npx
playwright test e2e/builder.spec.ts`. `global-setup.ts` skips its harness check in that mode.

## What `npm run e2e` does

`e2e/harness/run.ts`:

0. Brings the binaries up to date: `cargo build -p runbook-server -p runbook-cli --locked` from the
   repository root into `$CARGO_TARGET_DIR` (default `target/`), logged to
   `e2e/.run/cargo-build.log`. It is incremental, so it takes seconds when nothing changed, and it
   runs before the locks below, so a compile never holds up another Postgres suite. A failed build
   fails the run. It is skipped when `E2E_SERVER_BIN` or `E2E_RUNBOOK_BIN` names a binary (used as
   is), and when cargo is not on `PATH` (the existing binaries are used, and the log says they may
   be stale). The stack then logs the binaries it runs, when each was built and the checkout's
   commit, for example
   `[e2e] binaries: …/debug/runbook-server (built 2026-11-11T17:02:11.000Z), …/debug/runbook (built …); checkout at 0123456789ab with uncommitted changes`.
1. Re-runs itself under `lockf` (macOS) or `flock` (Linux) on this checkout's `e2e/.run/harness.lock`
   (both modes share `e2e/.tokens`), then on the shared Postgres lock, the nearest
   `.locks/pg.lock` above `web/` (or `E2E_PG_LOCK`), so it never overlaps another Postgres suite.
2. `stack.ts` creates **its own** database `runbook_web_e2e_<pid>` as `runbook_dev`
   (`E2E_PG_ADMIN_URL`, default `postgres://runbook_dev@localhost:5432/runbook_dev`), starts
   `runbook-server serve` on a free port with a generated `RUNBOOK_WEBHOOK_SECRET_KEY`, and runs
   `runbook-server bootstrap`.
3. Runs `examples/pilot/setup.sh` unchanged (project `pilot`, people, teams, launcher service token,
   skills, published flagship, webhooks to a closed local port, each person's seed and CLI token).
4. Seeds one more first token per person from `seed-first-token.sql` (it carries `runbooks:read`), and
   mints each person a narrow browser PAT from it. The editor (Carol) and the controller (Alice) each
   get one more seed with wider scopes and a browser token from it (see Secrets).
5. Starts Vite on a free port with `RUNBOOK_DEV_API` pointing at the server, waits for `/v1/meta`
   through the proxy, and runs Playwright with `E2E_STATE` and `E2E_BASE_URL`.
6. Always (also on failure or Ctrl-C): stops Vite and the server, drops that one database, and deletes
   `e2e/.tokens/`. No other database is touched.

Logs go to `e2e/.run/{cargo-build,server,vite,setup}.log` (gitignored). Bring-up takes about 4 s
after an up-to-date build.

Every run also records browser-side problems no spec asserts on (console errors and warnings,
uncaught errors, failed requests, responses ≥ 400; paths only) in `e2e/.run/browser-<mode>.jsonl`,
so a native and a container run can be compared line by line.

**A CSP refusal fails the test and the run** (U12), even when every assertion passed: a spec
notices a blocked font, worker or wasm module only when it breaks something it asserts on.
`support/fixtures.ts` catches refusals three ways in every context a test opens: the document's
`securitypolicyviolation` event, the page's console and each dedicated worker's console (Monaco's
and the compiler's). Each fails the test that caused it, and `harness/run.ts` fails the run on any
`csp` line in the log. That gate fails closed: a missing log, an unreadable line or an error after
Playwright exits 1. Vite's dev server sends no CSP, so native mode cannot see a refusal; **container
mode is the CSP gate**.

**External stack**: set `E2E_BASE_URL` (the web origin) and `E2E_API_URL` (the server), plus
`E2E_BOOTSTRAP_TOKEN_FILE` and `PILOT_PSQL` as in `docs/install.md`. The harness then starts nothing
and drops nothing; it only seeds. This path is not exercised; for the compose stack, use container
mode, which is.

## Container mode (`npm run e2e:container`)

The same specs against the packaged image, the way an operator runs it: `deploy/docker-compose.yml`
with the UI on (the server serves UI and API on one origin, so there is no Vite), its real CSP and
caching headers, the SPA fallback and the production bundle. Needs Docker with compose v2 and the
host CLI, which the run builds first (the same `cargo build`; the server it runs is the image's);
no host Postgres, so it skips the Postgres lock.

```sh
npm run e2e:container                                  # build (layer cache) and run, ~45 s warm
npm run e2e:container -- e2e/flagship.spec.ts          # Playwright arguments pass through
npm run e2e:container -- --serve                       # stack stays up until Ctrl-C
E2E_IMAGE_REUSE=1 npm run e2e:container                # skip the build when the tag exists
E2E_IMAGE=runbook-pilot:local npm run e2e:container    # another local tag (default runbook-pilot:e2e)
```

`e2e/harness/container.ts`:

1. Checks the build context first (`harness/context-check.ts`, U26): a throwaway BuildKit build
   copies `web/e2e/` out of the real context into a temp dir (no image, no tag) and fails on any
   hidden entry, so `.dockerignore` (`web/e2e/.*`) is verified the way docker reads it and
   `e2e/.tokens` or `e2e/.run` never reach a cached layer. `node e2e/harness/context-check.ts` runs
   it alone. Then builds `runbook-pilot:e2e` from the repository with `docker build` (log in
   `e2e/.run/image-build.log`; no registry, never pushed), unless `E2E_IMAGE_REUSE=1` and the tag
   exists.
2. Brings up compose project `runbook-e2e-<pid>` (`--env-file /dev/null`, so `deploy/.env` never
   applies) on a free loopback port, with `RUNBOOK_ORIGIN` set to it and freshly generated
   database passwords and webhook key that reach Docker only through the environment. Its
   `pgdata` volume is its own. `up --wait` waits for both healthchecks.
3. Runs `runbook-server bootstrap` in the server container and keeps the owner PAT in
   `pilot/owner.pat`; then the same seeding as native mode, with the host CLI against the published
   port and the interim first tokens through
   `PILOT_PSQL="docker compose -p runbook-e2e-<pid> … exec -T postgres psql -U runbook -d runbook"`.
4. Runs Playwright with `E2E_BASE_URL` (and `apiURL`) at the container's port and `E2E_MODE=container`.
5. Always (failure, Ctrl-C at any stage, also during bring-up): saves the server log to
   `e2e/.run/server.log`, then `docker compose -p runbook-e2e-<pid> down -v --remove-orphans`
   (in its own process group, so a second Ctrl-C cannot cut it short), and deletes `e2e/.tokens/`.
   Nothing else is touched: no other project, volume or image, and the image tag is kept for reuse.

If a run is killed with SIGKILL, remove its leftovers by name: `docker compose -p runbook-e2e-<pid>
down -v` (`docker ps -a --filter name=runbook-e2e-` lists them).

Container-only differences from native mode are product findings. One so far: the image's CSP had
`font-src 'self'`, which refused the two JetBrains Mono subsets Vite inlines as `data:` fonts
(cyrillic-ext, vietnamese), so those characters fell back to another monospace font; fixed in
`web_assets.rs` (`font-src 'self' data:`).

## Secrets

Everything secret is a 0600 file under `e2e/.tokens/` (0700, gitignored, deleted at teardown;
`E2E_KEEP_TOKENS=1` keeps it):

| File                              | What                                                                             |
| --------------------------------- | -------------------------------------------------------------------------------- |
| `<who>.browser`                   | Narrow human PAT: `runbooks:read`, `executions:read/start/act`, 1 day            |
| `<editor>.editor`                 | The editor's browser PAT: the above plus `runbooks:write/publish`, `skills:read` |
| `<controller>.control`            | The controller's (Alice's) browser PAT: `<who>.browser` plus `executions:admin`  |
| `pilot/<who>.pat`                 | setup.sh's CLI token (`executions:*` only)                                       |
| `pilot/<who>.seed`                | setup.sh's seed (adds `tokens:manage`; the browser refuses it)                   |
| `pilot/<who>.seed-web`            | The harness's own seed (`runbooks:read`, `executions:*`, `tokens:manage`)        |
| `pilot/<editor>.seed-editor`      | The editor's seed (adds the authoring scopes)                                    |
| `pilot/<controller>.seed-control` | The controller's seed (adds `executions:admin`)                                  |
| `pilot/launcher.svc`              | Launcher service token (mints execution pairs with `run token`)                  |
| `pilot/owner.pat`                 | Bootstrap owner PAT                                                              |
| `sign-in-*.token`                 | Tokens `sign-in.spec.ts` mints to test refusals                                  |
| `state.json`                      | IDs, URLs and paths only (`E2EState` in `support/state.ts`)                      |
| `a11y-fixture.json`               | The a11y spec's run IDs, so a restarted worker reuses its run                    |

Tokens reach the CLI through `RUNBOOK_TOKEN` in the child's environment, and the browser through the
sign-in field (a password input). Never put a secret in a test title, `console.log`, an assertion
message or a screenshot. Traces are off because they record request headers; `E2E_TRACE=1` turns
them on locally, and the result must not be shared.

## Fixtures (`support/`)

Import `test` and `expect` from `support/fixtures.ts`.

- `test` adds a `stack` fixture (`E2EState`: `baseURL`, `apiURL`, `project`, `projectId`, `users`,
  `teams`, `cli`, `pilotDir`, `tokensDir`, `editor`, `controller`). In `beforeAll`, use `loadState()`. Its
  automatic `browserIssues` fixture records browser problems and fails the test on a CSP refusal.
- `signIn(page, stack, 'alice')` signs in through the form with the person's browser token and
  waits for My tasks. `signInWithFile(page, file)` uses any token file and does not wait.
  `controlTokenFile(stack)` is the controller's `executions:admin` token, for pause, resume and
  cancel; never sign out with it (sign-out revokes, and other specs share it).
- `openAs(browser, stack, 'bob', { viewport: PHONE })` returns `{ context, page }` in a fresh
  context (sessionStorage is per tab, so each person needs their own). Close the context afterwards.
- `axeViolations(page)` runs axe (WCAG 2.0/2.1/2.2 A and AA) after finite animations finish and
  returns **moderate, serious and critical** findings (U32; `minor` is reported, not gated). It skips
  the reviewed `AXE_EXCEPTIONS` (each entry needs a `reason`; the list is empty) unless
  `{ exceptions: 'include' }`. `{ include: selector }` scans only that part of the page and throws
  when the selector matches nothing, so a renamed label cannot turn a scan into a pass.
  `E2E_AXE_REPORT=<file>` appends full detail (impacts, selectors, snippets, colors) as JSON lines
  for triage. `seriousAxeViolations` is a deprecated alias with the same, stricter gate.

`support/cli.ts`:

- `asPerson(stack, who, args, token?)` / `asPersonOk(…)` run the CLI as a person (`pat` by default).
- `startRun(stack, who, inputs)` starts `post-deploy` and returns the execution ID.
- `AgentDriver.start(stack, exe)` waits for the run to leave `queued`, has the launcher mint an
  execution pair (`run token`) in a private config dir, and then `doStep(path, report)` does
  `step next` → `step claim` → `step get` → `step report`, claim before read (D32/F60).
  `smoke()` and `regionsUnhealthy()` are the flagship's agent work on the approval path.
- `mintToken(stack, who, file, { scopes, agent?, seed? })` mints a token as the person; a token never
  exceeds its caller; both the pilot seed and `seed-web` carry `runbooks:read`.
- `sectionStepRun(stack, who, exe, path)`, `cliMarkdownReport(…)`, `apiGet(stack, file, path)` and
  `waitFor(check, what)` (polling with backoff, for the places a UI wait does not apply).

## Conventions

- No fixed sleeps. Wait on UI state (`expect(...).toBeVisible()`), a response
  (`page.waitForResponse`), or `waitFor` for CLI-side state.
- Specs run one at a time (`workers: 1`) against one org. `post-deploy` admits **one run per
  service** (`api`, `web`, `worker`) and queues the rest. The flagship spec uses `api`, and the a11y
  spec uses `worker` and `web`. New specs should publish their own runbook, or finish their run.
- Each person's inbox counts only their claimed tasks and the approvals they may decide: a ready
  human step with no assignee is in nobody's My tasks until it is claimed.
- The editor (M5) runs as `stack.editor` (Carol), whom the harness gives the `editor` project role
  and the `<editor>.editor` browser token (`EDITOR_SCOPES` in `harness/stack.ts`).
- Pause, resume and cancel run as `stack.controller` (Alice): a project operator, as setup.sh makes
  every pilot person, with the `<controller>.control` browser token (`CONTROL_SCOPES`: the browser
  scopes plus `executions:admin`; D27 needs both).
- Browser sign-in takes an allow-list of scopes (`BROWSER_SCOPES` in `src/features/auth/session.ts`,
  U3): `runbooks:read/write/publish`, `executions:read/start/act/admin` and `skills:read`. A token
  with any other scope (`tokens:manage`, `webhooks:manage`, …) is refused and the scopes are named.

## Suites

| Spec                   | Covers                                                                                                                                                                                                               |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sign-in.spec.ts`      | Narrow PAT accepted; `executions:admin` accepted; `tokens:manage`, `webhooks:manage` and agent-kind refused; sign-out revokes (401) and leaves nothing of the first person                                           |
| `flagship.spec.ts`     | Start dialog via ⌘K at 390×844, agent via run token, checklist, Carol refused, Bob decides and Alice's page follows live (within 3 s, no new stream), completion, Markdown export equals the CLI, My tasks counts    |
| `run-controls.spec.ts` | The human report form (outputs and a summary in one report), a signal releasing a wait step, pause, resume, cancel and rerun as the controller, and the live stream falling back to polling when every attempt fails |
| `start.spec.ts`        | Start dialog inputs as saved, a 422 on its field with the same key on retry, My tasks following the inbox cursor                                                                                                     |
| `editor.spec.ts`       | Edit, provisional then server diagnostics, save, publish; a concurrent CLI push is a 412 (keep mine, publish, diff); axe on the editor; outline expanders ≥ 24×24 px with a focus ring                               |
| `a11y.spec.ts`         | axe on every route and overlay in light and dark; the fixed R56–R58 findings; a keyboard-only approval                                                                                                               |

## Walkthrough (`npm run e2e:walkthrough`)

`walkthrough.spec.ts` (tag `@walkthrough`) walks the flagship as Alice, Bob and Carol at 1440×900 and
390×844 and saves numbered screenshots for a human reviewer (plan §6, "Pilot done"). It is left out of
`npm run e2e` and `e2e:container` (`grepInvert` in `playwright.config.ts`); `npm run e2e:walkthrough`
sets `E2E_WALKTHROUGH=1` and runs it alone in container mode, on a fresh stack (~35 s warm). It writes
outside the repository, to `E2E_WALKTHROUGH_DIR` (default `<checkout>/../.reviews/web-ui/`):
`NN[x]-<desktop|phone>-<light|dark>-<what>.png`, the downloaded Markdown export, `INDEX.md` (one line
per screenshot) and `walkthrough-log.txt` (console errors and warnings, failed requests and responses
≥ 400 by path, phone layouts wider than the viewport, and a few observed inconsistencies). Only those
files are replaced on a rerun. Screenshots are taken before any token is typed and every password field
is masked.

## Known gaps

No `test.fixme` is open and `AXE_EXCEPTIONS` is empty. When a spec finds a product bug, add a
`test.fixme` that describes it and passes once it is fixed (plus an `AXE_EXCEPTIONS` entry with a
`reason` if route scans must skip it meanwhile), and remove both with the fix.
