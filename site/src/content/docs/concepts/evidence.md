---
title: Evidence
description: Links, logs, files and screenshots attached to steps.
sidebar:
  order: 7
---

A step can require or accept evidence of what was done:

```yaml
- id: error_rate
  check: true
  instructions: Check the 5xx rate on the dashboard.
  evidence: { required: [link], accepted: [screenshot] }
```

The kinds are `link`, `log`, `file` and `screenshot`. A report that's
missing required evidence is refused.

```sh
runbook step report "$SR" --status succeeded --verdict pass \
  --evidence https://dashboards.example/errors/eu-west
runbook step evidence "$SR" ./screenshot.png
```

## Uploads

Files and screenshots are uploads. On the container edition they're off
until both `RUNBOOK_UPLOAD_SIGNING_KEY` and `RUNBOOK_USERCONTENT_ORIGIN` are
set ([environment variables](/reference/environment/)). Files are served
from that separate origin so a malicious file can never act as the app.
Accepted types are PNG, JPEG, WebP, GIF, PDF, text and JSON; HEIC and SVG
are refused.

Logs are capped at 5 MiB per step and outputs at 64 KiB per step.
