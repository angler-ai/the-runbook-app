---
title: The runbook language
description: The shape of a runbook, inputs, outputs, fields, variables and expressions.
sidebar:
  order: 1
---

A runbook is YAML with `apiVersion: therunbook.app/v1` and
`kind: Runbook` (both optional), `metadata` and `spec`. Unknown keys are
refused (`spec.unknown_key`); keys starting with `x-` are kept verbatim and
never executed.

```yaml
apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: post-deploy
  title: Post-deploy verification
  tags: [deploy]
spec:
  execution:
    title: "Verify ${{ inputs.service }}"
    timeout: 2h
  inputs:
    service: { type: enum, options: [api, web], required: true }
  fields:
    severity: { type: enum, options: [none, minor, major], default: none, indexed: true }
  vars: { result: undetermined }
  defaults: { actor: agent, timeout: 15m }
  steps:
    - id: check
      check: true
      instructions: Check ${{ inputs.service }}.
      outputs:
        status_code: { type: integer, required: true }
    - id: record
      if: "${{ steps.check.verdict == 'fail' }}"
      set: { fields: { severity: major }, vars: { result: failed } }
  outputs:
    result: "${{ vars.result }}"
```

## Inputs, outputs, fields and variables

- **`inputs`** are typed values given when a run starts. Types: `string`,
  `text`, `number`, `integer`, `boolean`, `enum`, `list` and `object`, with
  `required`, `default`, `options`, `min`, `max` and `pattern`.
- **Step `outputs`** declare what a step reports, in the same types. An
  output that doesn't match is refused before anything is recorded.
- **`fields`** are typed values on the run that steps can `set`. Up to 10
  can be `indexed`, so you can search runs by them
  (`runbook run list --field severity=major`).
- **`vars`** are the run's working variables, changed with `set`.
- **`spec.outputs`** are the run's results, computed from expressions.

## Expressions

`${{ … }}` holds an expression in a bounded profile of CEL
(`runbook-cel/1`). Expressions read `inputs`, `vars`, `fields`,
`steps.<id>.outputs.<name>`, `steps.<id>.verdict` and loop variables such as
`region` in a `foreach`. Expressions are checked when you publish.

## Triggers

`spec.triggers` starts runs on its own: `schedule` entries with a five-field
`cron` and an IANA `timezone`, and `webhook` entries called at
`POST /v1/projects/{project}/runbooks/{runbook}/triggers/{id}`. Only the
latest published version's triggers act.

## Validating

```sh
runbook spec validate my-runbook.yaml
```

prints `ok` with the worst-case number of step runs, or each diagnostic with
its path and code. The [samples](/samples/) are complete runbooks to start
from.
