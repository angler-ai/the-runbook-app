---
title: People, agents and anyone
description: Who may do a step, how agents hold steps with leases, and how a person takes over.
sidebar:
  order: 2
---

## Actors

Each step says who may claim it with `actor`:

- `human`: a person;
- `agent`: an agent, working through the CLI or MCP;
- `any`: either. This is the default.

`assignee` is guidance: it puts the step in the named person's or team's
inbox and notifications, and anyone allowed by `actor` may still claim it.
`assignment: strict` makes the assignee binding: on a `human` or `any` step
only the assignees may claim it, and on an `agent` step it limits who may
take over.

```yaml
- id: review
  actor: human
  assignee: team:oncall
  assignment: strict
  instructions: Review the change.
```

## How the server tells them apart

The server decides whether a caller is a person or an agent from the kind of
credential, not from what the caller says:

- a **personal access token** is a person;
- an **agent-kind token** (`runbook token create --agent …`) or an
  **execution pair** (`runbook run token`) is an agent.

Give an agent an execution pair or an agent-kind token, never a person's
token: with a person's token the agent is indistinguishable from that
person. Agents never cast human approvals.

## Claims and leases

An agent **claims** a step before it reads or reports it. A claim is a lease
that lasts 10 minutes after the last heartbeat:

```sh
runbook step claim "$SR" --session agent-1
runbook step heartbeat "$SR"
runbook step report "$SR" --status succeeded
```

If an agent crashes, its lease lapses and the step becomes ready again for
another agent. A lapsed lease isn't a failure. `runbook step exec` holds the
lease for you while a command runs.

## Take-over

A person may take over an `actor: agent` step while it's ready: never
claimed, or its agent's claim expired or was released. The take-over is
recorded on the run (`StepTakenOver`).

## Handoffs

`on_failure: handoff` (often under `spec.defaults`) hands a failed step to a
person instead of failing the run. `runbook step request-human` asks for one
explicitly.
