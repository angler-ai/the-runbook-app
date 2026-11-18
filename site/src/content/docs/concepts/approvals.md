---
title: Approvals and destructive steps
description: Decisions by named people, escalation, and the gates that keep agents from acting alone.
sidebar:
  order: 3
---

An **approval** step asks named approvers to choose between options. Later
steps read the choice as `steps.<id>.outputs.decision`.

```yaml
metadata: { id: ship-or-hold }
spec:
  inputs:
    version: { type: string, required: true }
  steps:
    - id: go_no_go
      approval:
        prompt: Ship ${{ inputs.version }} to every region?
        options: [ship, hold]
    - id: rollout
      if: "${{ steps.go_no_go.outputs.decision == 'ship' }}"
      destructive: true
      instructions: Roll out ${{ inputs.version }}.
```

- `approvers` names people or teams, for example
  `approvers: { teams: [oncall] }`. Teams resolve when you publish.
- `timeout` with `on_timeout: escalate` and `escalate_to` hands an
  undecided approval to more people after a while.
- People decide in the web app, from Slack, or with the CLI:

```sh
runbook step decide "$SR" --option ship --comment "Canaries are clean"
```

## Agents and approvals

Approvals aren't in an agent's work brief. `approval.allow_agents` lets an
agent decide, and requires `min: 1`; an agent's decision is recorded apart
from human signatures.

## Destructive steps

`destructive: true` marks a step that changes something you can't easily
undo, such as a rollback or a deletion. Put an approval in front of it so a
person decides before an agent acts.
