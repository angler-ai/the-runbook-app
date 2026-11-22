# Runner harness evaluation

The suite runs the native harness (`new-loop`), the legacy JSON-text loop (`current`), the minimal Bash loop (`bash`), and the Claude Code and Gemini CLI references. Use `--subject all` or a comma-separated list for paired comparisons. Each subject receives the same task instructions, schema, checklist, initial files and mock MCP data. Runner subjects use the sandbox-ctl Docker backend and its isolated network. Claude uses a temporary copy, restricted file tools, and an explicit stdio MCP configuration. Both MCP transports use the same fixture server implementation.

## Offline checks

```sh
npm ci --ignore-scripts
env -u RUNNER_TEST_DATABASE_URL -u DOCKER_HOST -u RUNNER_TEST_DOCKER_SOCKET -u ANTHROPIC_API_KEY npm run test:evals
```

Set `DOCKER_HOST` or `RUNNER_TEST_DOCKER_SOCKET` to include the fake-model Docker integration tests. They need the locally available `runbook-sandbox:1` sandbox image (override with `RUNNER_TEST_SANDBOX_IMAGE`) and the built runner redaction WASM. Build the image from the repository root with `docker build -f runner/sandbox.Dockerfile -t runbook-sandbox:1 .`. The remaining eval tests use scripted model streams, fake tools and local MCP transports.

The fault tests pin the current loop and mark proposed recovery behavior with `it.fails`. The replay fixtures under `fixtures/` are fake recordings. Regenerate them intentionally with `node --conditions=development packages/evals/test/generate-replays.ts`. A request hash covers the serialized call options except the abort signal. Replay also checks consumed call count, tool results and final outcome.

## Estimates and live runs

```sh
npm run eval -- --subject all --profile default --trials 3 --tasks full --dry-run
```

A dry run prints the estimate without finding credentials, starting Docker, or invoking Claude. The default task set is `tuning` (64 tasks). Reserve `holdout` (16 tasks) for evaluation after tuning. `full` runs all 80 tasks. `challenge` selects the 30 hard and expert tasks, including six holdouts. Pass exact task IDs in a comma-separated list to run only those fixtures, for example `--tasks config/timeout,logs/disk`. Live runs accept three to five trials per task.

Live runs require a positive `--max-usd` and provider credentials. The cap includes subject and rubric spend. Each subject gets a separate task-sized allocation. Runner and Gemini CLI requests reserve spend before dispatch, and Claude Code receives `--max-budget-usd`. Claude Code can report an overrun after billing. A subject that exhausts its trial allocation records `budget_exceeded` and the run continues while the global cap permits.

Gemini dry runs print the observed category costs from the first full run and the margin or fallback applied to each category. These are planning estimates. Runtime reservations account for the request, model and global spend available at the time of dispatch. Estimates cover model calls and exclude machine costs.

Resume with `--resume /path/to/report.json` and the same provider, profile and Vertex target. The command keeps completed trial IDs and updates the JSON report in place and the Markdown report beside it. Select the tasks and subjects to continue, and set `--max-usd` for the cumulative run, including the prior report's spend. For a preview of only the outstanding trials, add `--dry-run`.

The reports use actual usage returned by the gateway or CLI. Both reports are written to `eval-results/` by default. Use `--out` to choose a different directory. A Claude Code run is skipped when the executable is missing. Claude Code requires a version that supports restricted headless mode with explicit MCP settings. No Agent SDK is used.

`--record-model` saves per-trial runner model streams and request hashes alongside the report. The traces are intended for these synthetic fixtures and may contain fixture canaries. Replay one with `npm run eval:replay -- <trace.json>` using the same sandbox image. Claude's headless stream carries session events and result usage. It does not expose the model request boundary for harness replay.

## Grading and calibration

Deterministic grading covers outputs, verdict, checklist, blocked kind and structured partial findings, tool evidence, the final file set, and leaked synthetic canaries. Success and false-result rates use the number of trials as denominator. Cache hit rate uses cache-read tokens divided by input tokens. Tool error rate uses tool errors divided by tool calls. The paired report matches on profile, task and trial number and bootstraps tasks as clusters. Cost excludes rubric calls in the subject comparison. Rubric spend is reported separately.

The summary judge scores from 0 to 3 and reports its score separately. Label about 30 tuning summaries blind to the judge score and save the labels in a JSONL file that follows `fixtures/labels.schema.json`. Each record uses the report's trial ID and one human score. Run `npm run eval:calibrate -- <report.json> <labels.jsonl>` to calculate the confusion matrix, exact agreement, pass agreement and mean absolute error. The calibration command rejects holdout labels.

To estimate a Claude run on Vertex AI using the global endpoint:

```sh
npm run eval -- --provider vertex --project angler-dev --location global --subject new-loop --dry-run
```

For a live run, use Google Application Default Credentials and replace `--dry-run` with a positive `--max-usd` ceiling. The CLI obtains short-lived OAuth tokens through ADC. The estimate does not read credentials. Use runner subjects for Vertex evaluations.

Each completed trial appends its model and judge spend to the repository's
`eval-results/spend-ledger.jsonl`. Set `--ledger <path>` to use a different ledger.
Use `--total-cap <usd>` to stop before the next trial when its model and judge
allocations plus recorded spend would exceed the cumulative cap. Use the same
ledger for runs that share a cap. An interrupted run can leave unrecorded spend
for its in-flight trial. If a process was killed and left a `.lock` directory,
remove the empty lock directory after verifying no process owns it.

To seed past spend, append a JSON line like this while no run is using the ledger.
Use zero for an unknown breakdown and put the known total in the other cost field.

```json
{"timestamp":"2026-11-21T00:00:00.000Z","subject":"manual","task":"past challenge runs","runDir":"eval-results/challenge-{1,2,3}","modelCostUsd":9.93,"judgeCostUsd":0}
```
