---
title: Skills
description: Agent skills attached to steps, uploaded to a project and pinned at publish.
sidebar:
  order: 6
---

A skill is a folder of instructions and files that teaches an agent how to
do something, such as checking error rates or rolling back a service.
Declare the skills a runbook uses under `spec.skills`, then attach them to
steps:

```yaml
spec:
  skills:
    error_rates: ./skills/check-error-rates
  steps:
    - id: error_rate
      check: true
      skill: error_rates
```

- `skill: name` attaches one skill; `skills: [a, b]` attaches up to 8, with
  no duplicates. A step can't use both keys.
- Upload skills to the project first:

```sh
runbook skill push examples/pilot/skills/check-error-rates --project pilot
```

- Publishing pins each alias to the latest uploaded version whose name
  matches the last path segment, so a run always uses the version that was
  current when the runbook was published.
- An agent fetches a step's skill files with `runbook step skill-file`.
