# Bounded expression observer audit

Date: 2026-11-09. Task T8 implements event delivery, not read tracking or public evaluation. Adoption remains blocked on the complete implementation review. The high-level wrapper does not expose observers, activations, registries or executable ASTs.

## Boundary and state

`vendor/cel-core/src/eval/observer.rs` exports the fork-only `EvalObserver` and `ObserverStop::{ReadLimit, CostExceeded, Cancelled}`. All semantic methods borrow their values and return `Result<(), ObserverStop>`. `leave_scope` is a separate infallible cleanup method. Callbacks are not intrinsically bounded: the sealed T9 adapter must meet the read/path/alias/slot bounds. An arbitrary implementation of this low-level trait can allocate or perform unbounded work; no universal CEL sandbox claim is made.

`Program::eval_with_observer(activation, budget, observer)` and `Program::eval_with_runtime_context_and_observer(activation, budget, runtime, observer)` share the original borrowed budget. The latter also preserves immutable runtime registry, supplied event time and precompiled timestamp data. Existing metered context methods use a no-op observer. Legacy unmetered methods use no observer and retain upstream qualification/optional semantics.

`Program::eval_internal` creates one stack-owned `Observation` with a borrowed observer and a `Cell<u64>` counter. Root scope is implicitly 0; root entry/exit events are not emitted. Each child gets a checked, monotonically increasing ID. IDs restart for each Program evaluation, including evaluations sharing a session budget. There is no cumulative scope history in the evaluator. Each loop scope follows a successful loop-entry precharge, and the profile allows at most one canonical exists plus its final-result scope. Therefore scope allocation is bounded by the caller's fuel; the high-level hard fuel cap is 100,000. Integer overflow is independently rejected as CostExceeded.

`child_evaluator` preserves the same observation, budget, runtime, registry, root activation and type information. The child receives its own scope ID. The ordinary CEL final accumulator evaluation has a scope too; it has no item binding. T9 must distinguish compiler synthetic accumulator names from user roots using accepted compiler metadata. The bounded parser's hygienic accumulator is controller-owned; do not assume every source `__result__` is internal.

## Hook coverage and ordering

| Path | Event and boundary | Covered work |
| --- | --- | --- |
| `eval_expr` | Dispatch precharge, then `begin_expr(scope, expr_id)` | Before runtime constants, literals, roots, operators or children. The observer clears stale expression-origin slots here. |
| `eval_ident` / `eval_root_ident` | Resolution precharge, activation lookup, then `resolve(scope, expr_id, name, value)` | Borrowed result includes the ordinary unknown-identifier error for absent roots. Activation is the sealed immutable projection; read recording happens in the observer. |
| `eval_member` metered path | Receiver evaluation, lookup precharge, then `field(scope, expr_id, receiver_expr_id, receiver, name)` | Before map/registry lookup, including a Null receiver. Ordinary error receivers propagate without inventing a child read. |
| `eval_index` metered list+Int path | Receiver and index evaluation, lookup precharge, then `index(scope, expr_id, receiver_expr_id, list, index)` | Before bounds checking or retrieving an element. Receives the attempted signed index; accepted negative indices retain CEL's existing normalization. |
| `eval_ternary` | Condition, chosen branch only, then `select_result(scope, expr_id, selected_expr_id)` | Transfers only chosen result provenance; no discarded-branch events. Also runs for an ordinary error result, but never after a terminal failure. |
| `eval_comprehension` list path | Range evaluation, then `consume_range(scope, receiver_expr_id, list)` | Before accumulator setup or any iteration, including empty lists reached through roots, fields, index or ternary. |
| List iteration | Loop-entry precharge, scope entry, `bind_item(scope, name, list, index)`, then activation binding | Binding announces identity without consuming the item. Local `resolve` determines actual scalar reads. Upstream extra accumulator-recognition iteration is retained. |
| Every child scope | `enter_scope(scope, parent)` / RAII `leave_scope(scope)` | Includes list/map comprehension children, final result and cel.bind children. Only list item origins are reported; map/bind provenance is outside the profile. |

Field and index methods split charged wrappers from inner lookup helpers so their metered profile paths charge exactly once and notify exactly once. The observer is separate from `ProtoRegistry`; no Cell, budget or mutable provenance is placed inside its Send+Sync boundary. T9 may use immutable prepared record metadata to compute the projected field's origin before the registry consumes it.

The profile rejects optional selection, map/string/bytes indexing, unsigned/double list indices, map iteration, cel.bind, `has`, arbitrary function calls, collection construction and CEL `in`. Those generic fork paths do not establish a bounded provenance or complete cost guarantee. “Membership” here means the future ReadKind for list index/range use, not the CEL `in` operator. General unmetered behavior is checked by the upstream suite.

## Terminal state and cleanup

`Evaluator::observe` checks live budget with zero-unit charge before every semantic callback and again after it. A returned stop maps to `BudgetError` through the first-error-wins setter; a callback that cancels or exhausts the borrowed budget and returns Ok also stops immediately. No dispatch, later observer hook, scope creation, list binding, overload retry or subsequent evaluation can clear a terminal state. Existing T7 post-dispatch/final-result checks prevent CEL error suppression from converting it to success.

A `ScopeGuard` is created before calling enter. It calls leave exactly once on success, break, ordinary error, terminal error, or failed enter (the callback may have partially initialized its private state). Guard drop itself only calls the cleanup method, with no allocation, charge or evaluation. Cleanup may run after failure. The T9 implementation must remove at most bounded aliases/slots, must not append reads or allocate, and must tolerate a partially initialized scope. Root slots belong to one evaluation's tracker lifecycle; the caller must reset them between programs because expression/scope IDs restart.

No extra fuel is charged for event delivery itself. Dispatch, resolution, lookup and loop-entry precharges remain those of T7. T9 is responsible for read insertion and path-byte precharges inside the observer before constructing paths or appending reads. Returning ReadLimit is sticky in the original budget, and private runtime mappings preserve that exact public error variant.

## Provenance obligations for T9

Keep no more than one slot for each accepted AST node (256 maximum), tagged with its owner scope, rather than a growing `(scope, expression)` history. Begin clears the expression's old slot; resolve/index/field set the exact resulting origin; select copies only the selected slot or clears to no origin. Cleanup removes scope-owned slots and aliases. Aliases use scope/name plus retained collection identity and index, never scalar equality. Immutable Bindings retains canonical CEL list allocations, including distinct empty allocations, through its list-origin map.

A field from binding-derived Null uses the receiver slot and closed schema to emit the actual Absence and MissingValue; a literal-null branch has no origin and yields TypeError without an invented read. Absence of an optional declared root/field must stay distinguishable from explicit Null. Resolve includes absent-root error values, while field hooks precede lookup and let the adapter identify missing fields from immutable storage/schema. T8 itself leaves ordinary CEL error representation unchanged.

ReadTracker, ordered deduplication, read/path charges, all MissingValue translations, read overflow tests, bounded materialization and public evaluate APIs remain T9/T10 work. Nothing in this task claims complete read traces or bounded output.

## Executable evidence

`vendor/cel-core/tests/bounded_observer.rs` exercises ten concrete tests: equal scalars from distinct lists; unused aliases; first-success short-circuit; distinct retained empty allocations; attempted failed/negative indices; selected null-record branches; literal-null clearing across reused IDs; repeated loop expression IDs with fresh scopes; null items reached by index/alias; empty ranges through field/index/ternary; errors for absent roots; every semantic stop reason; cancellation returned through Ok; and a real Send+Sync registry call counter proving lookup never occurs after failed field precharge or observer stop. A budget sweep checks balanced cleanup on each cutoff. A subsequent evaluation observes no leaked local alias.

The initial test failed on absent EvalObserver/ObserverStop imports. Mutation checks remove begin delivery and range consumption independently: null provenance then incorrectly remains `a` across a reused literal ID, and empty-list range events disappear. Restoring hooks returns all tests to green. T7 exact exists costs and runtime-context tests remain green. Full vendor tests, workspace tests, wrapper clippy/format and Wasm compilation are recorded in T8-report.md. Actual Wasm execution and full adoption review are still pending later tasks.

## Equality consumption follow-up
Eq/Ne operands now emit consume_value(scope, expr_id, value) through the same terminal-aware observer gate before comparison, including the constant record/null path. This lets the sealed tracker record record presence without emitting intermediate projection reads. Literal null has no source and records nothing; chosen binding-null keeps its AST origin. Regression observed zero hooks instead of two before integration, then passed identity, short-circuit, first-hook ReadLimit and no-later-callback checks. All11 observer tests pass. T9 review accepted the hook and actual sourced-record tracking.
