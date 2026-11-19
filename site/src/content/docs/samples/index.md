---
title: Samples
description: Complete runbooks you can copy, each checked in CI.
sidebar:
  order: 0
---

Complete runbooks you can copy and adapt. Each page is generated from the
runbooks this repository seeds for review (`scripts/dev/review-runbooks/`),
and CI checks every one with `runbook spec validate`.

Validating offline checks the whole runbook except references to your
server's directory: teams, people, skills and other runbooks. Those resolve
when you publish to a project that has them, and the publish fails with
`spec.unresolved_team` (or `_user`, `_skill`, `spec.call_unresolved`) if one
is missing.
