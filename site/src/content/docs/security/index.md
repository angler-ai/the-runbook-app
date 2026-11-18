---
title: Security model
description: Sign-in, roles, tokens, redaction and the audit trail.
sidebar:
  order: 1
---

## Sign-in

People sign in with an email code or Google; there are no passwords.
Sign-in is by invitation, optionally limited to your email domains
(`RUNBOOK_AUTH_ALLOWED_DOMAINS`).

## Roles

- Organization roles: owner, admin and member. Owners and admins are admins
  of every project.
- Project roles: viewer, operator, editor and admin.

## Tokens

| Token | Prefix | For |
| --- | --- | --- |
| Personal access token | `trb_pat_` | A person, or an agent with `--agent`; 90 days by default, at most 1 year |
| Service token | `trb_svc_` | A project's automation, such as CI; until revoked |
| Execution pair | `trb_exe_` / `trb_exr_` | One run's agent; the access token lasts 1 hour, the refresh family at most 30 days, only while the run is open |

Tokens are scoped to chosen permissions and projects. An execution pair is
revoked when its run ends.

## What the server enforces

The server enforces who may claim and complete a step, approvals and
destructive-step gates, leases, timeouts and retries, and output and
evidence validation. It can't see which tools an agent uses, so
`agent.hints` are preferences only: limit what an agent can do through the
access you give it.

## Redaction and the audit trail

Secrets are redacted from step logs, including output streamed by
`runbook step exec`. Every run event records who did it, so a run's record
is its audit trail.
