# Complete bounded-expression differential evidence

The completed corpus executes the public `runbook-expr` APIs natively and in a
fresh `wasm32-unknown-unknown` binary under Node. This records implementation
evidence; `cel-decision.md` remains the authority for whether A4 may begin.

## Reproducible invocation

```sh
cargo test -p runbook-expr --test parity --locked -- --nocapture
```

`tests/parity.rs::native_wasm_agree_on_complete_corpus` creates a new, exclusively
test-owned target directory for each invocation and builds `examples/parity.rs`
from current source with the locked dependencies. It invokes
`tests/wasm-runner.mjs`, which rejects nonfunction imports and supplies throwing
implementations for every imported function. Any attempted guest clock, network,
filesystem, storage or randomness call therefore fails the test. The fixture
uses explicitly supplied event time. Temporary target cleanup removes only the
directory created by that invocation.

The example exposes an immutable once-initialized buffer through pointer/length
exports. The runner checks its memory bounds, stable address/length and UTF-8,
then forwards the original JSON bytes. It does not reserialize parsed JavaScript
numbers, which would lose exact i64 values. Rust parses both results and compares
the complete ordered JSON arrays, including every value, structured diagnostic
or error, source span, full ordered read identity/kind, cost and loop count.

The completed run on Rust 1.99.1, aarch64 Apple Darwin and Node 26.8.1 reported:

```text
396 complete expression corpus rows agree across native and fresh Wasm
test native_wasm_agree_on_complete_corpus ... ok
```

The first completed invocation took 13.23 seconds, including the fresh build.
This is observed test duration, not a runtime latency guarantee. The existing
workspace test command includes this same integration test.

## Inventory and independent expectations

| Slice | Rows | Assertions before differential comparison |
| --- | ---: | --- |
| Compiler, descriptor restore, schema, binding ingress, scanner and flagship compilation | 129 | Expected success/rejection, structured diagnostics and valid spans; all 15 flagship expression segments and 14 containing values in their actual scopes |
| Public expression evaluation | 64 | Hand-specified values/errors, complete ordered read origins/kinds, loop counts and selected exact or bounded costs |
| Flagship runtime scenarios and companions | 141 | All 14 containing values across seven scenarios; report, index, whole record, distinct iterations, current/last iteration and explicit-null cases; exact outcomes and provenance |
| Template rendering | 36 | Hand-counted costs, values/errors, full reads and zero loops; shared resource/terminal behavior and late failures |
| Large/deep ingress boundaries | 6 | Borrowed deep rejection, width overflow, exact aggregate byte and node limits plus one-over rejection |
| Compiler metadata | 20 | Independently specified conservative dependencies, lexical alias handling, spans and syntax admission errors |
| **Total** | **396** | Unique named rows, exact slice inventory and mandatory nonempty provenance |

The flagship scenarios are healthy, unhealthy, rollback, keep, null verdict,
missing output and distinct iterations. The source YAML remains byte-identical
to the PRD example. Compilation and runtime fixture tests pin the source paths,
so typed foreach items, local region aliases, instructions, titles, parameters,
guards, approval prompts, result assignments and output expressions cannot be
removed to make the suite pass. This proves expression surfaces, not a complete
runbook-engine execution; that remains a later gate.

The combined guard requires every template/ingress name and every flagship
scenario/site pair. Every flagship runtime row must contain nonempty reads and
positive cost. Expected reads are independently constructed from the fixture
identity contract, never copied from observed evaluator traces. Runtime template
rows use `render/...` in the combined corpus to distinguish them from existing
compiler-only `template/...` scanner rows; the underlying assertions are reused
unchanged.

The integration guard was first added without its rows. Native execution failed
with `missing required row render/whole-list`, and actual Wasm failed inside the
same guard. Adding the reviewed helpers then produced the 364-row pass. Thus
identical empty or incomplete arrays cannot satisfy this test.

The final review added 12 canonical-alias ingress cases: both zero signs,
both alias orderings, and aliases before/after an actual list item. Opposite
signs reject; identical signed zeros pass. The corrected 376-row run took
13.61 seconds including a fresh Wasm build.

## Boundaries and limits of the evidence

Runtime rows cover empty/early/middle/no-match exists, consumed versus unused
aliases, failed indexing, absence versus explicit null, record presence,
short-circuit reads, revisions and iteration identities, deterministic time,
integer extremes, negative zero, output expansion and shared fuel/read/output
limits. Terminal failure remains visible beneath boolean/ternary forms and on
later session calls. Rendering cases additionally prove that later fragments
do no work after failure and returned interpolation syntax is not executed.

The missing-output flagship case deliberately follows ordinary CEL behavior:
item zero records Absence/Error, item one is consumed and yields true, and the
third loop entry recognizes the accumulator. The wrapper returns the saved
MissingValue. Ordinary missing/type errors are not resource-terminal failures;
the exact trace includes all work actually performed.

The deep ingress fixture has 100,000 nested list wrappers and explicitly
dismantles caller-owned input iteratively before asserting rejection. The width
fixture has 100,001 elements. Separate well-typed fixtures reach exactly
8,388,608 aggregate bytes and 262,144 nodes, then reject one extra byte/node.
Large accepted fixtures are dropped between cases. These fixtures execute in
the actual Wasm guest, not only in native tests.

Public schemas stop at depth 16, so output depth 32/33 defense-in-depth guards
are tested through private native fixtures. Native small-stack and private
observer/allocation-counter tests remain additional evidence, not claims that
those private hooks executed in this public-API Wasm corpus. The source audits
establish the recursive and allocation bounds that finite cases cannot prove.

All task slices received independent review. The final review must still assess
the complete implementation, integration audit, maintained-fork patch, corpus,
CI wiring and all residual findings before the adoption decision changes.

The independently reviewed A5 metadata amendment adds20rows; the complete396-row
corpus passed after the private segment-boxing layout correction (14.29seconds
including fresh Wasm compilation). Existing runtime rows remain unchanged.
