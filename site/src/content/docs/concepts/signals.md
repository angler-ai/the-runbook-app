---
title: Signals and waits
description: Steps that wait for your own systems, a duration or a time.
sidebar:
  order: 4
---

A `wait` step pauses its branch until something happens:

```yaml
- id: connect_shopify
  wait: { signal: shopify_connected, timeout: 14d }
```

Your system sends the signal through the API or the CLI:

```sh
curl -X POST "$RUNBOOK/v1/executions/$RUN/signals/shopify_connected" \
  -H "Authorization: Bearer $TOKEN" -d '{"store_id": "…"}'
runbook run signal "$RUN" shopify_connected
```

A signal wait may declare `outputs`, in the same form as a task's outputs.
The payload must match: an invalid payload is refused with
`422 signal.invalid_payload`, nothing is recorded, and the step keeps
waiting. A valid payload becomes the step's outputs.

```yaml
- id: connect_shopify
  wait:
    signal: shopify_connected
    outputs:
      store_id: { type: string, required: true }
```

Waits can also be for a duration (`wait: { for: 30m }`) or until a time. A
single wait is at most 7 days; signal waits are bounded by the run's own
timeout.
