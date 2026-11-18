# A0 CEL decision — reviewed SUBSET; A4 integration permitted

Updated 2026-11-09. **SUBSET implementation reviewed; A4 integration permitted.**
The accepted implementation is `c49ac9394a10b8885770eba3009e3eb640c61b42`,
following whole-source review of `34d8b45` and independent review of the sole
required correction (bit-exact scalar snapshot aliases). This does not approve
full CEL support or any operation excluded by `runbook-cel/1`.

The production boundary is `runbook-expr`, with the maintained pinned
`vendor/cel-core` fork. The immutable A0 spike remains experimental evidence;
production consumers must not import its APIs or the vendor AST/Program.
The approved [design](../superpowers/specs/2026-11-09-bounded-expressions-design.md)
is the exact API, profile and `runbook-cost/1` contract. The
[audit index](bounded-expression-audit.md) links source-level proofs and the
[native/Wasm evidence](bounded-expression-parity-evidence.md).

Key enforced limits: source 4,096 bytes; final AST 256 nodes/depth 32; closed
schema depth 16 with 1,024 types/4,096 fields; borrowed bindings depth 32,
262,144 nodes, 100,000 list items and 8 MiB string/metadata bytes; session fuel
at most 100,000 (default 10,000), 4,096 distinct reads, 65,536 encoded output
bytes/16,384 nodes/depth 32; templates 65,536 source bytes/64 expressions.
Other lexical, identity and parser limits remain as specified in the design;
these figures are logical envelopes, not total process-memory guarantees.

Evidence: 176 workspace tests and 395 vendor tests passed before the final
correction, with two ignored vendor documentation tests. The correction passed
all 19 binding tests and freshly built actual native/Wasm parity across 376
rows, including all unchanged flagship sites. Strict workspace Clippy and
formatting passed after the correction. The complete integration also passed
workspace Wasm checks, vendor checks and exact pinned-archive/license/tamper
verification. Remote CI was not run. The whole-source review and focused
correction review found no remaining adoption blockers.

The backend maintainers own the fork under the existing
[vendor provenance policy](../../vendor/cel-core-provenance/PROVENANCE.md): pin the
upstream archive/checksum, preserve licenses, reconstruct every packaged byte
from the cumulative patch, and independently re-audit changed reachable parser,
checker and runtime paths on upgrades. Passing upstream tests alone does not
approve a profile or resource-bound change.

A4 integrates the sealed types without reducing read identities. A5 owns schema
lowering and actual compiler scopes; A20 owns render-session lifetime. Engine
revision creation, atomic read persistence and full runbook execution remain
later tasks. The historical spike findings below are retained for provenance;
their pending-production statements describe the earlier decision only.

---

# A0 CEL feasibility decision — SUBSET (gate remains closed)

Updated 2026-11-09 after a bounded instrumented-fork experiment. **Not GO. A4 and expression-dependent work remain blocked pending a separately reviewed subset implementation.** A1–A3 may continue on types. All current spike tests pass, but they prove only the fixture paths and explicitly restricted operations below, not the complete bounded pilot.

The unmodified `cel-core 0.5.1` has a real checker and runtime schema/read hooks but no public cost-limit API. A small isolated patch now demonstrates shared fuel, terminal cancellation, early stopping and native/Wasm metric parity. This makes instrumentation of the existing evaluator the smallest evidenced next option. A bespoke evaluator is **not selected**. The earlier **5–8-day estimate is withdrawn as unvalidated**; no replacement delivery estimate is established.

## Candidates and inspected evidence

Versions were resolved with `cargo search cel --limit 30`, `cargo search cel-core --limit 10`, and `cargo info <crate>` on 2026-11-09. Crates.io package source, not only API descriptions, was inspected. Commit IDs below come from each package's `.cargo_vcs_info.json`.

| Candidate | Version / packaged commit | Findings and disposition |
| --- | --- | --- |
| [cel-core](https://docs.rs/cel-core/0.5.1/cel_core/) | 0.5.1 / `77277ec6a336646d946ee549a5012116378b82e4` | `Env::compile` performs checking; `ProtoTypeResolver` supplies closed field schemas; `ProtoRegistry::message_field_access` observes real reads. `Program::eval` and the private evaluator expose no budget/observer. Selected as the checker foundation, **not** as an approved runtime. |
| [cel](https://docs.rs/cel/0.15.0/cel/) | 0.15.0 / `57b2a7e9b835425be0a7320e6bc0461380843f2d` | `Program::compile` returns parse errors, accepts both negative fixtures. `references()` is static AST reference discovery. No cost hook in inspected evaluation path. |
| [cel-interpreter](https://docs.rs/cel-interpreter/0.10.0/cel_interpreter/) | 0.10.0 / `2822f8623da6aca8acfb9f504faf0d6b2acfcea0` | Same distinction: its actual `compile` implementation invokes the parser; both invalid typed expressions parse. It is not a publish-time schema checker. |
| [cellang](https://docs.rs/cellang/0.7.0/cellang/) | 0.7.0 / `6c8cb5c3b3469b7101ee5f24e91c39799e4506c9` | Another real checker: `Env::compile` lowers and calls `type_check`. Wasm is documented. Inspected `interpreter.rs` private `EvalContext`/recursive evaluation has no cost/budget hook. Source-screened; no claim of executed parity or checker fixtures for this candidate. |
| [rscel](https://docs.rs/rscel/1.0.8/rscel/) | 1.0.8 / `9268ce9faf9346a57ff2f21260775c13c5b9d206` | Runtime/compiler source screened. `type_prop` includes runtime type propagation; it is not proof of closed-schema publish checking. No interruptible budget found in the interpreter. Not execution-probed. |
| [cel-cxx](https://docs.rs/cel-cxx/0.2.5/cel_cxx/) | 0.2.5 / `58138da98a8f0ab2f66a32bd5148f89ec8f0a16f` | Real CEL-C++ checker. Package documents `wasm32-unknown-emscripten`, which does not establish the required Workers/browser `wasm32-unknown-unknown` fit. Rust `Program::evaluate` wrapper inspected; no general meter/trace API established. Not compiled in this spike; native/C++ conformance does not prove the deployment requirement. |
| [ferricel-core](https://docs.rs/ferricel-core/0.14.0/ferricel_core/) | 0.14.0 / `af7aacfcb84a6481c7b634413bcaa818ad687e15` (package says dirty) | Compiles CEL to Wasm; runtime supports Wasmtime engine/epoch/resource configuration. Compiler directly consumes `cel::parser::Parser`; no closed variable-schema checker established. Unconditional Wasmtime dependency and host-managed interruption do not prove a portable browser/Workers Rust core with identical CEL cost accounting. Not compiled; no claim it cannot be adapted. |

The conclusions are limited to the inspected releases/APIs. They do not claim that no Rust CEL implementation can ever satisfy the requirements.

## Exact fork, licensing and reproducibility

The experiment's code (a fork of cel-core 0.5.1 with a 169-line metering patch, its provenance and a reproduction harness) was removed once the decision was made; no production crate ever depended on it. The vendored cel-core release the expression engine does use is described in `vendor/cel-core-provenance/PROVENANCE.md`.

The parity test now **rebuilds the Wasm artifact** before executing it in Node. It compares all ten checked value/read-trace rows and twelve budget/size/projection rows, including costs, loop counts, callback counts and message-clone counts. Wasm imports throw if called; no imported host function is invoked by these fixtures. `now()` returns the supplied timestamp 42. This is actual Wasm execution, not compile-only evidence or complete browser packaging.

The follow-on experiment ran from 2026-11-09 23:02:06 UTC to 23:15:02 UTC (**12m 56s**, inside its 90-minute cap and the original eight-hour ceiling). It stopped on bounded experimental evidence; unresolved production work was not implemented. Final ten-probe run: **1.262 s**, exit 0 (0.58 s test harness), including automatic Wasm freshness checking/execution. The prior post-format run including native/Wasm recompilation took 3.571 s. Upstream fork unit tests: **331 passed**, 2.282 s including compilation, 0.01 s harness. Formatting: exit 0, 0.074 s. Exact-patch/license verification: exit 0.

## Probe outcomes and measured limits

| Probe | Result | Exact evidence |
| --- | --- | --- |
| `reject_string_ordering` | PASS | Valid syntax `inputs.rate > 0.01` is rejected by checked compilation with `NoMatchingOverload [String, Double]`, before evaluation. |
| `reject_unknown_output` | PASS | `steps.health.outputs.missing` is rejected as `UndefinedField` against a closed output record schema. |
| `stop_bounded_exists` | PASS **on experimental fork only** | Exact 100,000-integer expression, budget 100: terminal `Err(CostExceeded)`, used 100, 13 charged loop entries. |
| `native_wasm_agree` | PASS for tested forms | Exact values, complete positive/negative read lists, fuel/loop counts and callback/clone counts agree on native and Wasm. |
| `terminal_cancellation_and_shared_child_fuel` | PASS | Exhaustion beneath `&&`, `||`, ternary and comprehension remains terminal; error-suppression forms cannot turn it into a value; zero later callbacks. |
| `size_dependent_operations_are_precharged_or_excluded` | PASS for experiment | Million-byte string equality rejects before comparison at used cost 3; forbidden collection equality/string indexing/macros fail the profile gate; list indexing/resolution uses shared storage; oversized/collection formatting rejects. |
| `message_projection_does_not_clone_the_entire_collection` | PASS for fixture adapter | 100,000 pre-built message items: used 100, 9 loop entries, 43 instrumented Region-record clones during evaluation (not all record clones), terminal exhaustion. No traversal/cloning of the full collection on root projection. |
| `upstream_meter_failure_remains_visible` | PASS as baseline control | Unmodified release still returns `false`; counted equivalent visits all 100,000 items. This is evidence of the upstream limitation, not an upstream metering pass. |
| `parse_success_is_not_check_success` | PASS | `cel 0.15.0` and `cel-interpreter 0.10.0` parse both invalid typed expressions. |
| `static_references_are_not_reads` | PASS | Static `steps` reference survives in a skipped branch that successfully evaluates without binding `steps`. |

The first investigation's unmodified test suite had five passes and one intentional failure. That historical failure remains valid; the current suite tests the separate fork API and preserves an explicit upstream control. The fork's public result is `Result<Value, CostExceeded>` backed by a sticky fuel state outside CEL values. `child_evaluator` borrows that same state. Charges occur before expression dispatch and before loop element cloning/binding; exhaustion is rechecked after child evaluation and before function dispatch. Consequently ordinary CEL error suppression cannot clear exhaustion. This is not implemented with unwinding, a timeout, a wall clock, or a post-execution count.

Experimental units are one per AST dispatch, one per loop entry, and one per allocated literal/string-comparison byte. Failed charges set terminal exhaustion without charging work not performed: long-string equality reports used 3, not 100. These units are deterministic experiment units, **not a production cost specification**. Loop entries count attempted binding/condition evaluation, not completed predicates; the upstream algorithm opens a second entry to observe a successful `exists` accumulator, so first-item success reports two entries. No skipped item's fields are read.

Exact callback cases include `items.exists(i, visit(i) || later())` at budget 10: used 10, one loop entry, one `visit` callback, zero `later` callbacks. `(items.exists(i, i > 999999) || true) && later()` remains `CostExceeded`, used 100, twelve loop entries, zero later callbacks. Shared-budget assertions would fail if a child/iteration reset fuel.

## Allowed-operation resource audit

The experiment's checked-AST allowlist runs before evaluator dispatch. It allows scalar/null literals, declared record fields, integer list indexing, boolean/comparison operators, ternary, a single canonical one-variable `exists` expansion, supplied `now()`, and literal `timestamp()` (at most 64 bytes). `visit`/`later` are constant-work **test-owned instrumentation callbacks**, not proposed user extensions.

| Surface | Evidence / enforced experiment bound | Production gap |
| --- | --- | --- |
| Scalar and boolean operations | Per-node fuel; fixed-size numeric/null values; scalar type-map validation; collection/message comparison refused before recursive equality. | Runtime input shapes must be checked against declarations; the fixture is already well formed. |
| Strings | Literal bytes charged before allocation. Comparison operand lengths charged before scanning strings. A 1,000,000-byte bound value aborts at the comparison pre-charge. | Define the final unit model and ingress byte limits. Other string functions are excluded. |
| Lists | `Value::List` uses `Arc<[Value]>`; pointer-identity test proves root resolution shares the existing allocation. Integer indexing is constant time and clones one value; loop entries charge before cloning. | Ingress element count/memory bounds and general element adapters remain to implement. Pre-existing activation construction is outside evaluation fuel. |
| Collection equality, map indexing, string indexing, collection construction | Rejected by the profile before dispatch. Tests reject `items == items`, `text[0]`, list/map literals and other macros. | Proposed pilot map/index requirements need a separately reviewed addition; no silent claim of general indexing support. |
| Record projection/cloning | Test records have at most 16 fields, kind length 128, path length 256; immutable field storage is `Arc<BTreeMap<...>>`. Fixed schema lookup and bounded path formatting; measured 43 instrumented Region-record clones, not 100,000 (other record kinds are not included in this counter). | Arbitrary `MessageValue`, `Activation`, `ProtoRegistry` and function implementations can do unbounded work. Restricting/adapting them for production is mandatory; this generic fork API alone is unsafe as a general meter. |
| Lookup/metadata allocations | Fixture source at most 4096 bytes; checked AST at most 256 visited nodes/depth 32; names/field names bounded by the gate; per-iteration bindings bounded by canonical `exists`. | General runtime schemas, diagnostics and provenance storage need explicit limits. |
| Time/functions | Fixed supplied `now()`; literal timestamp at most 64 bytes; test callbacks perform atomic increments only. | No arbitrary user functions, ambient clock/storage/randomness, or broad built-in library is approved. |
| Output formatting | Separate `bounded_format` accepts scalar output; checks string length at 4096 before copying; rejects collection/oversized output without generic recursive formatting. | It is **not** a complete typed interpolation implementation, aggregate output budget, or engine integration. Diagnostic JSON reporters operate on fixed test data. |
| Parsing/checking | Source length is checked before calling upstream compilation; the closed fixture schema is finite. | Depth/node checks run **after** parsing/checking. Adversarial parser/checker stack/time bounds are unproven; a post-parse limit is not sufficient. This blocks GO. |

Other macros (`all`, `map`, `filter`, `exists_one`, `has`), nested/two-variable comprehensions, arithmetic/concatenation, regex, optional syntax, dynamic/map field access, object construction, arbitrary functions and the PRD helpers `success`, `failure`, `always`, `default`, `semver_compare` reject with `expression.unsupported` where checked syntax is otherwise valid. The checker may reject invalid types/syntax earlier. Full CEL support is not claimed.

## Provenance and unchanged flagship scope

Complete expected traces are asserted, including `steps.regions.items[0].error_rate.outputs.rate` and every intermediate projection; item 1/2 fields and unchosen branches are absent. Tests assert the exact row count, so two empty/missing traces cannot pass. A separate large-message fixture shares immutable field storage to measure cloning; its shared nested record is a cost probe, not a full iteration-identity model.

Remaining provenance work includes direct list/scalar-index consumption, step-run/iteration IDs, value revisions, amendment behavior and trace limits. The flagship's original comparison/ternary/exists/verdict expressions work unchanged in the fixture. Full YAML execution, all interpolation sites, `region`, `inputs.regions`, `vars.result`, and typed interpolation have not been integrated or proven. Nothing authorizes modifying the flagship to hide those gaps.

## Smallest defensible next step — separate review, not A4 integration

The small patch is concrete evidence that extending the existing evaluator is worth reviewing before authoring another evaluator. It is not evidence of a complete bounded implementation or a cheaper delivery date. The bespoke evaluator proposal and 5–8-day estimate are withdrawn.

Review this fork's exact patch and limited cost/cancellation model first. A separately scoped subset implementation must then establish: pre-parse/checker resource bounds; an enforced versioned source/AST profile; validated and bounded activation/schema/callback adapters; the complete string/index/output/interpolation budget model; revision-aware read provenance; and unchanged full flagship expression/interpolation coverage on native/Wasm. Assign ownership and an upstreaming/maintained-fork upgrade policy before adoption. The 331 passing upstream unit tests exercise regression behavior, not those missing proofs.

Do not silently promote `eval_with_budget` into production. Reopen the bounded experiment if review finds a specific gap; if the required surfaces cannot be bounded within an accepted separate plan, record STOP before selecting a replacement evaluator. **SUBSET still blocks A4+ until the design and resulting implementation are separately reviewed.**
