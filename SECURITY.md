# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue,
discussion or pull request.

Use GitHub's private vulnerability reporting on this repository: open
[Security → Advisories → Report a vulnerability](https://github.com/angler-ai/the-runbook-app/security/advisories/new).
Only the maintainers see the report.

Include:

- the affected component (server, sign-in service, CLI, MCP server, web app
  or Cloudflare edition) and the version or commit;
- what an attacker can do, and the steps to reproduce it (a proof of concept
  helps);
- any logs or requests, with secrets removed.

We'll acknowledge the report, keep you updated while we work on a fix, and
credit you in the published advisory if you'd like.

Please give us a reasonable time to release a fix before disclosing the
problem publicly, and test only against servers you own.

## Supported versions

The Runbook hasn't had its first release yet. Until it does, fixes land on
the default branch. After it, security fixes go to the latest release.

## How the project is secured

The security model (what the server enforces, credentials and scopes,
redaction and the audit trail) is described at
[docs.therunbook.app/security](https://docs.therunbook.app/security/).
