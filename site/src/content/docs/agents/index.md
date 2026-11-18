---
title: Connecting agents
description: Claude Code, Codex, Cursor and custom agents, over the CLI or MCP.
sidebar:
  order: 1
---

An agent works a run's steps in a loop: ask what's next, claim a step, read
it, do the work, report it, and repeat until the brief says `exit`.

## With a shell: the CLI

An agent that can run commands uses the `runbook` CLI with `--json`. The
`therunbook` skill (`skills/therunbook/` in the repository, also packaged as
a Claude Code plugin in `plugins/claude-code/`) teaches it the loop:

```sh
runbook step next "$EXE" --json
runbook step claim "$SR" --session agent-1 --agent-name my-agent
runbook step get "$SR" --json
runbook step exec "$SR" -- ./scripts/smoke-test.sh
runbook step report "$SR" --status succeeded --verdict pass
```

Give the agent an execution pair (`runbook run token`) or an agent-kind
token (`runbook token create --agent`), never a person's token.

## Over MCP

The same tools are served two ways ([MCP reference](/reference/mcp/)):

- **Local, over stdio:** the client runs `runbook mcp serve`, which uses the
  CLI's context and stored credentials.
- **Remote, at `/mcp`:** Streamable HTTP on your server. Clients that
  support OAuth sign in and appear in `runbook apps list`; others send
  `Authorization: Bearer <token>`. For example, in Claude Code:

```sh
claude mcp add --transport http runbook "https://runbooks.example.com/mcp" \
  --header "Authorization: Bearer $RUNBOOK_TOKEN"
```

A token has exactly the scopes it has on the REST API, and an execution
token reaches only its own run. Published runbooks are MCP resources
(`runbook://runbooks/<rb_id>`), and the `run_runbook` prompt starts a run and
walks the loop.

## Telling agents there's work

Subscribe a webhook to `agent.work_available` to wake an agent, for example
through a CI workflow dispatch, when one of its steps becomes ready.
