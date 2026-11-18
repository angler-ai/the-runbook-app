---
title: Runbooks, runs and steps
description: What a runbook is, what happens when it runs, and what a run leaves behind.
sidebar:
  order: 1
---

A **runbook** is a procedure written as a short YAML file: inputs, steps and
outputs. Only IDs are required; titles come from the IDs.

```yaml
metadata: { id: post-deploy-checks }
spec:
  inputs:
    service: { type: string, required: true }
  steps:
    - id: check_health
      instructions: Call /healthz on ${{ inputs.service }}; pass on 200.
    - id: summarize
```

You save a runbook as a draft and **publish** it. Publishing compiles it:
unknown keys, bad expressions and broken limits are refused, and teams,
people, skills and called runbooks are resolved against your server. Each
publish is a new numbered version.

A **run** (an execution, `exe_…`) is one pass through a published version
with its inputs. Start one from the CLI, the web app, the API, a schedule or
a webhook trigger:

```sh
runbook run start post-deploy-checks --project prod --input service=api
```

Each step in a run is a **step run** (`sr_…`). A step run is ready, claimed,
waiting, or finished with a status (`succeeded`, `failed` and so on) and,
for a `check`, a verdict (`pass` or `fail`). Steps run in order unless
control flow says otherwise; see [control flow](/language/control-flow/).

## The run record

Every action on a run is an event that records who did it: a person, an
agent, a schedule (`schedule:<id>`) or a webhook trigger (`webhook:<id>`).
The run page reads like a document, with each step's outputs, logs and
evidence, and the report exports as Markdown, JSON, JUnit or PDF:

```sh
runbook run report "$EXE" --format md
runbook run report "$EXE" --format pdf --out report.pdf
```

## Steps of many kinds

| Kind | Key | What it does |
| --- | --- | --- |
| Task | `instructions` | Work for a person or an agent |
| Check | `check: true` | A task that also records a `pass` or `fail` verdict |
| Approval | `approval` | A decision between options, by named approvers |
| Wait | `wait` | Waits for a signal, a duration or a time |
| Set | `set` | Sets variables and fields |
| Call | `call` | Runs another runbook as a child |
| Containers | `parallel`, `switch`, `foreach`, `loop`, `steps` | Group and repeat steps |
| End | `end` | Ends the run early |
