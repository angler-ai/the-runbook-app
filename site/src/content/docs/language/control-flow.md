---
title: Control flow
description: parallel, switch and if, foreach, loop, call, wait, catch and finally.
sidebar:
  order: 2
---

## `parallel`

Branches run at the same time. `join` says when the parallel succeeds:
`all` (the default), `any`, or `{ at_least: n }`. Once it can no longer
succeed, or has succeeded under `any`/`at_least`, its open branches are
cancelled. At most 20 branches.

```yaml
metadata: { id: canary-pair }
spec:
  steps:
    - id: canary
      join: all
      parallel:
        - { id: canary_us, check: true, instructions: Watch us-east at 5% for 15 minutes. }
        - { id: canary_eu, check: true, instructions: Watch eu-west at 5% for 15 minutes. }
```

## `if` and `switch`

`if` skips a step when its expression is false. `switch` runs the steps of
the first case whose `when` is true, or its `default`.

```yaml
metadata: { id: keep-or-roll-back }
spec:
  inputs:
    error_rate: { type: number, required: true }
  steps:
    - id: decide
      switch:
        cases:
          - when: "${{ inputs.error_rate > 0.01 }}"
            steps:
              - { id: roll_back, destructive: true, instructions: Roll back. }
        default:
          steps:
            - { id: keep, instructions: Keep the deploy. }
```

## `foreach`

Runs its steps once per item. `max_items` is required (at most 100), and
`concurrency` limits how many items run at once.

```yaml
metadata: { id: per-region }
spec:
  inputs:
    regions: { type: list, items: { type: string }, default: [us-west, eu-west] }
  steps:
    - id: regions
      foreach:
        items: "${{ inputs.regions }}"
        as: region
        concurrency: 1
        max_items: 10
        steps:
          - id: deploy
            instructions: Roll out to ${{ region }}.
```

## `loop`

Repeats its steps until `until` is true, or while `while` is true, tested
after each iteration. `max_iterations` is required (at most 50); `delay`
waits between iterations.

```yaml
metadata: { id: settle }
spec:
  steps:
    - id: stabilize
      loop:
        until: "${{ steps.recheck.outputs.rate <= 0.01 }}"
        max_iterations: 3
        delay: 5m
        steps:
          - id: recheck
            check: true
            outputs:
              rate: { type: number, required: true }
```

## `call`

Runs another published runbook as a child and waits for it. Calls nest at
most 3 deep, and a runbook can't reach itself through `call`.

```yaml
- id: verify
  call:
    runbook: post-deploy-checks
    inputs: { service: api }
```

## `wait`

See [signals and waits](/concepts/signals/).

## `catch` and `finally`

A group's `catch` runs when one of its steps fails; `catch_on` limits it to
failure types (`timeout`, `failed`, `call_failed`). `finally` always runs,
on a group or for the whole runbook under `spec.finally`.

```yaml
metadata: { id: guarded }
spec:
  steps:
    - id: migrate
      steps:
        - { id: apply, instructions: Apply the migration. }
      catch_on: [failed]
      catch:
        - { id: restore, instructions: Restore from the backup. }
  finally:
    - { id: notify, instructions: Post the outcome to the channel. }
```

## Retries

`retry: { max_attempts: 3, on: [timeout] }` retries only those failure
types; without `on`, it retries any failure. It also works under
`spec.defaults.retry`.
