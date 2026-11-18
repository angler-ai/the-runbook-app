# Internal contract between the api Worker (or native server) and the sign-in service

UPD1 review #8. During an update the two run different releases for a moment
(the sign-in Worker deploys first, then the api Worker; a rollback goes the
other way), so each must work with the other's previous release.

| Route | Caller | Request | Answer |
| --- | --- | --- | --- |
| `POST /internal/session` | api | `Cookie` header, `x-runbook-internal-secret` | `auth-session.v*.json` |
| `POST /internal/can-register` | sign-in | `can-register.v*.request.json`, `x-runbook-internal-secret` | `can-register.v*.response.json` |

Rule: these shapes only grow. A field may be added (readers ignore unknown
fields); none is removed, renamed or retyped. A change that cannot be
additive adds the next `v*` files, and both sides keep accepting every
version still in this directory for at least one release before an old one
is deleted. The tests read every file here: `crates/runbook-api/tests/
internal_contract.rs` (the api parses each session answer, answers each
can-register request) and `auth/tests/support/suite.mjs` (the service's
session answer has every field of each version with the same type; it
accepts each can-register answer).
