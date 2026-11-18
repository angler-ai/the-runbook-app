---
title: Memory
description: What agents and people learn on a runbook, kept for its next runs.
sidebar:
  order: 5
---

Agents and people save what they learn on a runbook, and the next run's
steps see it. An agent might save "the eu-west dashboard lags by five
minutes"; the next agent on that step reads it before starting.

```sh
runbook memory search --help
runbook memory save --help
```

People stay in control of what agents write:

- every entry records who wrote it, so an agent's writes name its token or
  run;
- `history` and `revert` show and restore earlier revisions;
- `lock` stops agents editing an entry; `unlock` allows it again;
- `save --propose` saves a proposal instead of a live entry, and people
  `accept` or `reject` it;
- `archive` retires an entry.

See [`runbook memory`](/reference/cli/#runbook-memory) for every command and
flag, and the [MCP reference](/reference/mcp/) for the matching tools.
