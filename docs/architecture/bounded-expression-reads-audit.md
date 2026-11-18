# Bounded expression read provenance audit

Date: 2026-11-09. T9 implements exact semantic reads and private output-source preservation. Public evaluation and bounded output conversion remain T10. A4 adoption remains blocked on complete implementation review; this is not an engine integration or actual Wasm execution claim.

## State and lookup bounds

`EvalSession` owns `ReadTracker`, whose ordered `Vec<ReadRef>` contains at most 4,096 entries. Dedup compares borrowed `(kind, path, full RecordIdentity)` against those entries in first-consumption order. No `Origin` clone occurs before deciding whether a read is new. The scan is at most 4,096 comparisons; each identity has at most 32 tuples with two at-most-128-byte strings, an ordinal, and bounded execution/step IDs. This is a hard finite ceiling, not a constant-time or wall-clock claim. The hard 100,000 fuel cap additionally bounds total charged new-read string metadata. No global `(node, instance) -> ordinal` restriction is added: the full tuple participates in comparison. Different step runs, ordinals and revisions remain distinct; already returned statistics own their snapshots.

Each `ReadObserver` is constructed afresh for one private `evaluate_raw_with_source` call. Its stack-backed array has 256 optional origin slots, each containing scope ID, AST ID and an immutable `Arc<BoundNode>`. `begin_expr` clears the previous slot for that AST ID; setting a result clears any prior version and occupies one vacant slot. Lookup/clear/set scan at most 256 slots. There is no cumulative scope history. The accepted profile has one nonnested exists, so exactly one current alias is sufficient; its name occupies a fixed 128-byte array, and its scope and canonical node are retained separately. Synthetic `@result` is not a declared root. Ordinary user `__result__` names are handled through actual declarations and scope-specific aliases.

`leave_scope` clears that scope's alias and origin slots by replacing `Option`s with None. It does not allocate, charge, append reads or evaluate. Bindings retains all nodes and their prepared CEL allocations throughout the call, so dropping these temporary Arcs cannot destroy the backing tree. Root slots disappear with the observer after evaluation. No static state or mutable budget is installed in `PreparedRegistry`; it remains immutable and Send + Sync.

## Source inspection paths

| Consumer | Bound and source |
| --- | --- |
| Root resolve | Closed `BTreeMap` of at most 256 roots. A present node is an Arc clone; an absent declared root uses the explicit `root_origin`, never a sibling or guessed revision. |
| Capture source | `capture` inspects one sealed node's declared `TypeId` and the immutable schema (at most 1,024 types), records a scalar Value, and stores its Arc. It does not inspect descendants. BoundNode retains its declared type even when its prepared CEL value is Null. |
| Field | Uses the receiver AST slot, then at most one lookup in its at-most-256-field immutable map. Absent fields use that enclosing node's exact identity. Bound null-record fields use the receiver's retained owner. Literal-derived Null has no slot and cannot fabricate Absence. No traversal of fields or children occurs. |
| Index | `Bindings::cel_list_node` looks up the retained CEL Arc data pointer in the prebuilt bounded BTreeMap (at most 262,144 nodes). It emits Membership before examining index bounds, normalizes the signed index using the list length, then performs one `get`. Indexing a record projects only its Arc; scalar indexing records Value. |
| Range / alias binding | Range consumption uses the same list-identity lookup, including empty lists. Binding looks up that list and exactly one item without recording an unused scalar. Resolving the exact scope/name later captures its canonical item. Equal primitive values in different lists never identify their sources. |
| Record presence comparison | Parent-owned fork `consume_value` identifies each Eq/Ne operand by scope and AST ID before comparison. The tracker inspects that one node and records Value for a bound record (including Null). Scalars were already recorded and literal Null has no source. Intermediate projections stay silent. |
| Ternary | `select_result` copies only the selected expression's slot. Literal branches lack slots. Reused AST IDs and scopes never infer provenance by scalar equality. |
| Final source | `result_source` looks up the program's root AST ID in scope 0 and clones at most one Arc. `evaluate_raw_with_source` returns `(Value, Option<Arc<BoundNode>>)` privately. Nullable record provenance survives for T10, with no whole-value leaf enumeration in T9. |

The CEL list identity index is built once during accepted binding conversion. Runtime never rebuilds it and never traverses a whole collection before checking its budget. The registry separately clones a prepared field value after the observer has completed its read precharge. Collection clones share Arcs. No I/O, clock, storage, randomness, external callback, raw public CEL result or executable injection surface is added.

## Charge and failure order

Before recording: check the existing budget terminal reason, then the tracker's sticky read limit; compute the virtual path length from bounded borrowed pieces; reject lengths above 1,024; scan for a duplicate; reject a new entry when already at 4,096; precharge `1 + canonical path bytes + all copied identity string bytes`; only then allocate/copy path, identity and trace entry. Fixed revision/ordinal fields and the at-most-32-entry iteration vector have a structural bound; variable string bytes are explicitly charged. Duplicates allocate nothing and add no read units.

Absent paths are compared as borrowed prefix/separator/field pieces before materialization. The controller explicitly resolved a valid enclosing path whose absent child would exceed 1,024 bytes as sticky `ReadLimit` (trace representation overflow), before append or path allocation. Tests cover exactly 1,024 and 1,025 bytes. MissingValue itself is held as a trace index during CEL evaluation; only after checking terminal state does the wrapper precharge its owned diagnostic path copy. A failed diagnostic-copy charge keeps already consumed reads and takes precedence over MissingValue.

An unsuccessful charge leaves used fuel unchanged. The observer returns the exact terminal stop; the fork prevents subsequent semantic callbacks. ReadLimit is also remembered in the tracker so T10's direct `record` calls remain sticky without needing a public budget setter. Resource failures/cancellation persist across later calls. Ordinary missing data does not persist: a later valid expression in the same session succeeds and retains the previous trace. Explicit scalar Null yields a Value read; a missing optional root/field yields Absence and MissingValue even if CEL could otherwise suppress its error.

## T10 handoff

- `EvalSession.reads.record(&Origin, ReadKind, &EvaluationBudget)` is the private shared recording entry point. Check `session.terminal_error()` before output work and after errors.
- `runtime::evaluate_raw_with_source` supplies the raw value and optional canonical final node; `evaluate_raw` remains a private test seam. Neither is exported publicly.
- `Bindings::cel_list_node(&Value)` returns an immutable canonical node through the retained list index. `BoundNode` exposes crate-private `ty`, `value`, `origin`, `cel`; list/record children are already shared immutable nodes. `PreparedRecord` exposes immutable fields/origin/type for bounded output traversal.
- T10 must meter its own per-node/output-byte traversal and record returned scalar leaves/list membership. The controller additionally requires Membership for whole-returned record shape, including empty records; this does not add a T9 intermediate-record projection read. Engine producers later must advance container revisions when that shape/presence changes.
- Statistics clone only the bounded cumulative trace; output conversion must not reset it. No public arbitrary CEL evaluation feature should be added for testing.

## Evidence

The private `reads` unit module exercises complete ordered vectors (including error traces), actual checked programs and sealed bindings: one nested leaf, skipped branches, repeated reads, explicit iteration/step identities, amended revisions, direct and unsuccessful index, first-success/unused/empty exists, equal scalars from distinct lists, derived empty ranges, null/absent roots and fields, null records through index/alias/ternary, valid `__result__` roots and aliases, record/null Eq/Ne presence consumption, repeated calls, fuel cutoffs, cleanup after terminal cancellation, diagnostic-copy precharge, 4,097th-read rejection, and 1,024/1,025-byte virtual absence paths. T9-report records fresh commands and remaining gates. Full-output returned leaves belong to T10; engine schema/revision lowering remains A5/later engine work.
