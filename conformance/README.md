# Backend conformance fixtures

`spec/post-deploy.yaml` is the complete PRD example, byte-for-byte. A test also
compares it with the compiler fixture and the mirrored PRD. No queue, index,
instruction, parameter, checklist, or cleanup keys were removed.

`execution/post-deploy.json` records deterministic pure-engine events and commands
for manual rollback and a recovered loop followed by a human keep decision. Both
traces replay without evaluating expressions again and run in native/Wasm parity.
Directory membership, actors, timestamps, and skill hashes are explicit fixtures.
These fixtures do not establish real authentication, claims, queueing, indexed
search, or webhook delivery. Those integration seams are named in the artifact.

The draft API examples `post-deploy-completed.json` and `loop-recovered.json` are
validated against the shared Rust models and draft OpenAPI. A successful rollback
has execution status `succeeded`; a recovered loop retains its worst finding
`fail` and separately exposes `satisfied: true`, `exhausted: false`.

## Gate A policy boundaries

Planner decisions D13–D15 are adopted: agent-enabled gates require min=1 and
record agent DecisionApplied provenance separately from human signatures;
node-level timeouts are task-only; end is rejected within any finally subtree.
Nested wait/approval timeouts and the execution deadline remain supported.

The expression implementation remains the bounded CEL profile established by A0,
with its documented pilot restrictions; these fixtures do not claim full CEL or
full v1 conformance. Postgres/runtime integration has not been exercised here.

To refresh the recorded fixtures after an intentional semantic change:

```sh
RUNBOOK_UPDATE_FIXTURES=1 cargo test -p runbook-engine --test flagship --locked
cargo test -p runbook-contract --test draft flagship_examples --locked
```
