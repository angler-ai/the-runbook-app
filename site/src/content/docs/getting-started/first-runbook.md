---
title: Your first runbook, agent run and approval
description: Write a runbook, publish it, work its steps as an agent and decide an approval.
sidebar:
  order: 4
---

This walkthrough uses the CLI against a server you started in a
[quick start](/getting-started/docker/). Sign in first with
`runbook login https://your-server`.

## Write it

Only IDs are required. Save this as `hello.yaml`:

```yaml
metadata: { id: hello }
spec:
  inputs:
    service: { type: string, required: true }
  steps:
    - id: check_health
      actor: agent
      instructions: Call /healthz on ${{ inputs.service }}; pass on 200.
    - id: go_no_go
      approval:
        prompt: Keep ${{ inputs.service }} running?
        options: [keep, rollback]
```

Check it offline:

```sh
runbook spec validate hello.yaml
```

## Publish it

The first runbook of a project is pushed with `--draft`, then published:

```sh
runbook push hello.yaml --project default --draft
runbook publish hello --project default
```

## Start a run

```sh
EXE=$(runbook run start hello --project default --input service=api --json | jq -r .id)
```

## Work the agent's step

Give an agent an execution-scoped credential, never a person's token. A
service token mints one for a run:

```sh
runbook run token "$EXE" --session agent-1
```

Then the agent loop: ask what's next, claim the step, read it, report it.

```sh
runbook step next "$EXE"
runbook step claim "$SR" --session agent-1 --agent-name my-agent --model manual
runbook step get "$SR"
runbook step report "$SR" --status succeeded --verdict pass --output status_code=200
```

`step next` prints `next: claim_ready_work` with the ready steps and their
step run IDs (`sr_…`), or `next: wait` when nothing is ready for this actor.
An execution credential can read only the step it has claimed.

To run a real command under the step's lease, use `step exec`; it streams the
output into the step's log with secrets redacted and reports from the exit
code:

```sh
runbook step exec "$SR" -- ./scripts/smoke-test.sh --env prod
```

## Decide the approval

Approvals aren't in the agent's brief; a person decides them. Find the step
run in the run report and decide:

```sh
runbook run report "$EXE" --format md
runbook step decide "$SR" --option keep --comment "Healthy"
```

## Check the outcome

```sh
runbook run get "$EXE" --wait
runbook run report "$EXE" --format md
```

The run report reads like a document: who did each step, their outputs,
logs and evidence. Export it as Markdown, JSON, JUnit or PDF.
