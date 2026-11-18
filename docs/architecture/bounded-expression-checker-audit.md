# Closed-profile checker audit

Date: 2026-11-09. Scope: the vendored `cel-core 0.5.1` checker reached through `Env::check`. This is conditional source evidence for `runbook-cel/1`, not a new checker, runtime fuel meter, production input validator, or adoption decision. Task 5 must enforce the preconditions below. The independent implementation review and native/Wasm corpus remain required.

## Preconditions and exact declarations

Before `Env::check`, the portable wrapper must establish a unique-ID tree with at most **N = 256 nodes, D = 32 edges**, bounded source/tokens/names/literals, and only profile forms. Collections, optional operations, type expressions, bind, arbitrary calls, root-qualified identifiers and all macros except one canonical, nonnested, one-variable `exists` are excluded. Unary minus is restricted to a numeric literal. Calls are only `now()` and `timestamp(string-literal)` with their fixed arities. Canonical `exists` contributes its five bounded AST children and only two local names.

The immutable TypeEnv must validate all types, including unreachable nodes: at most 1,024 type nodes, 4,096 fields, 256 fields per record, 256 roots, depth 16, valid bounded names and IDs, and no cycles or dynamic/type-parameter declarations. Records lower to opaque `Message` names; scalar nullability uses `Wrapper`; collection nullability is rejected. No user-supplied resolver/callback/AST is admitted. The resolver's message and field tables are bounded, lookups return precomputed CEL types, unknown messages cannot enter root/field declarations, enum/extension/qualified-type resolution returns no result. Namespace and abbreviations are empty.

Use `Env::new`, not `Env::with_standard_library`. The following **entire** declaration table is required, including candidates considered for rejected inputs. Every overload is standalone. `T` is the sole type parameter where shown; all other declarations have none. There are 14 functions and 31 overloads. The tests compare every upstream signature below against the actual upstream declaration, not just an overload-count cap.

| Function | Overload ID | Parameter types → result |
| --- | --- | --- |
| `_==_` | `equals` | `T, T → Bool` |
| `_!=_` | `not_equals` | `T, T → Bool` |
| `_?_:_` | `conditional` | `Bool, T, T → T` |
| `_[_]` | `index_list` | `List(T), Int → T` |
| `!_` | `logical_not` | `Bool → Bool` |
| `_&&_` | `logical_and` | `Bool, Bool → Bool` |
| `_||_` | `logical_or` | `Bool, Bool → Bool` |
| `-_` | `negate_int64`, `negate_double` | respectively `Int → Int`, `Double → Double` |
| `_<_` | `less_{suffix}` | `S, S → Bool` |
| `_<=_` | `less_equals_{suffix}` | `S, S → Bool` |
| `_>_` | `greater_{suffix}` | `S, S → Bool` |
| `_>=_` | `greater_equals_{suffix}` | `S, S → Bool` |
| `timestamp` | `string_to_timestamp` | `String → Timestamp` |
| `now` | `runbook_now` | `() → Timestamp` |

In each relational row the exact five `(suffix, S)` pairs are `(bool, Bool)`, `(int64, Int)`, `(double, Double)`, `(string, String)`, `(timestamp, Timestamp)`, in that order. No uint, bytes, duration, map-index, conversion, member, or extra error-recovery candidates are present. Canonical upstream `exists` uses unary `!` and binary `||`; it needs no `@not_strictly_false` declaration. `now` is the only repository declaration in this table. This selection is agreed with the Task 5 owner; its production construction and tests must stay synchronized with this audit.

## Type-shape invariant and persistent substitutions

`types/mod.rs:94` stores child types and message names in `Arc`. A `CelType::clone` is a shallow enum/Arc clone, **not a recursive copy of schema records**. Message equality compares only the bounded name. A list/wrapper shape has at most 17 constructors and **H = 18 total type nodes** including its leaf. Use this deliberately conservative height even when TypeEnv's depth convention gives a smaller shape.

Induct on `check_expr` (`checker/check.rs:193`): literals, root types and resolver returns have no `TypeParam`/`TypeVar`; member selection returns an existing field type, index removes one list constructor, boolean operators return Bool, timestamp/now return Timestamp, and ternary selects/widens existing parameter-free types. Canonical exists begins with Bool accumulator, introduces the range's element type or recovery Dyn, and returns the accumulator. There is no collection literal or type constructor that can grow a type. Recovery can introduce `Error` or `Dyn`; neither contains parameters. Thus the invariant needed for termination is **parameter-free arguments**, including error paths, not the false assertion that every checker-internal type is concrete. Accepted checked output must subsequently reject Dyn/Error and forbidden types/overloads. In particular, `true ? 1 : 'bad'` checks successfully as Dyn upstream.

In `overload.rs:190`, scoping renames the sole declaration parameter to `T#<u64>` (at most 22 bytes). In `is_assignable` (`:224`), the initial binding is an existing parameter-free argument (or Dyn if a TypeVar were present). Compatibility/widening only replaces it with another parameter-free argument or Dyn. No binding can contain a TypeParam, so `substitute_type` (`:387`) follows at most **one substitution edge**, then a type chain of H nodes. It cannot recurse through a parameter cycle. This also covers an index candidate that binds T before failing its second argument.

`resolve_overload` (`:50`) clones the persistent map once per arity-matching candidate, commits the clone on success and discards it on failure. The comment “only keep non-scoped bindings” at the commit is misleading: there is no filtering operation. The bound must count retained scoped bindings. Only equality, inequality, conditional and list-index are generic, and each has exactly **one candidate with one parameter**. All other candidates insert zero entries, including when Error/Dyn matches all five relational candidates. Every checked operation resolves at most once; there are at most N such operations. Therefore:

- Persistent entries ≤ N = **256**, and any live candidate map ≤ N + 1 = **257** (a conservative bound allowing the current unsuccessful binding).
- Candidate attempts/map clones ≤ 5N = **1,280**.
- Persistent entries copied by those clones ≤ 5N² = **327,680** shallow `(Arc<str>, CelType)` entry clones.
- Only one candidate map is live in addition to the persistent map. Argument subtrees finish before parent overload resolution starts; parent matching maps do not accumulate along the AST stack.

The design's generic caps of eight candidates and two parameters alone do not establish these numbers. Adding `index_map`, custom generic declarations or whole standard-library installation invalidates this exact proof and requires re-audit.

## Recursive edges, loops, and allocations

All paths here refer to `vendor/cel-core/src/`. Bounds include rejected typed inputs that pass the syntax/profile gate.

| Source / operation | Bound and reason |
| --- | --- |
| `env.rs:529`, `check.rs:1097`, `Checker::new`, `Checker::check` | Fixed dispatch into the resolver variant; root-scope initialization copies at most 256 bounded names and shallow types. No source parsing in this entry point. |
| `check.rs:193`, `:375`, `:387`, `:403`, `:429`, `:705`, `:737`, `:929` | `check_expr` follows each child at most once, ≤N dispatches; ≤D+1 nested dispatches. Unary/binary/ternary/member/index helpers recurse only into their children. Calls have ≤1 argument, fixed identifier callee, and the enum resolver returns None before argument checking. No enum-constructor fallback can check an argument twice. |
| `check.rs:929`, `:1140`; `scope.rs` | One comprehension checks range/init/condition/step/result once, with no iteration over runtime data. Bool accumulator makes `contains_type_var_checker` constant here; even a shape walk is ≤H. At most two scopes, 256 global and two local declarations; each scope lookup visits ≤2 maps. No nested comprehensions/bind scopes. |
| `check.rs:646`, `:656`, `:678`, `:530` | `leftmost_ident_resolves` and `try_qualified_name` descend member chains of ≤D edges. They may repeat on successive member parents: ≤N(D+1) visits per helper, rather than a linear-tree claim. Scope/qualified lookup has no namespace loop. Each built name ≤L = 33×129 = **4,257 bytes**. Name assembly/copy is conservatively ≤N(D+1)L bytes per helper. `resolve_proto_qualified` splits at most 33 parts and receives None from the closed resolver. |
| `check.rs:253`, `scope.rs:118` | Identifier resolution uses ≤2 scopes; empty namespace removes container-prefix loop. No constant type declarations or enum values are installed. Reference names are bounded copies. |
| `check.rs:809`, `:830`; `types/decls.rs:95` | Each function lookup clones at most five fixed overloads; ≤5N overload clones. Each has at most three shallow parameter clones, one result clone, one bounded generic name and fixed ID strings. No implementation callback is invoked by checking. |
| `overload.rs:50` | ≤5 candidates, arity ≤3, ≤5 collected IDs. Full argument vectors ≤3. Arity recovery sorts/deduplicates ≤5 small counts, though valid pre-check arities avoid that path. Candidate-map clone/commit bound is above; hash-map collision behavior is still bounded by its finite entry count, not assumed O(1). |
| `overload.rs:147`, `:190` | Rename visits each fixed signature node: maximum four total parameter/result nodes for conditional, four for index. A generous ≤4H visit allowance covers every signature. Each candidate allocates ≤1 scoped name and ≤3 scoped parameter slots. |
| `overload.rs:224`, `:372` | A parameter binding may call compatibility, which tries at most two assignability directions sequentially. Within a concrete direction each recursive call removes a constructor from at least one side; ≤2H calls. Every structural equality encountered visits ≤H type nodes. There is no map/abstract branching or second generic-binding recursion because bound arguments are parameter-free. |
| `overload.rs:334`, `:350` | `contains_type_var` and the widening equality traverse at most H unary type nodes. At most two contains walks and one equality per widening test. Neither can create a binding chain. |
| `overload.rs:387`, `:432`; `check.rs:184` | Candidate result substitution follows ≤1 binding edge and ≤H unary nodes. Finalization makes exactly three type passes per type-map entry (`finalize`, `substitute`, `finalize`), ≤3NH = **13,824** type-node visits/possible constructor allocations. ≤N map entries. No recursion into Message fields. |
| `check.rs:179`, `:403`, `:830`, `:929`; `checker/errors.rs` | Each checked node emits at most two errors locally (conservative; fixed-table ternary resolution normally cannot add the second). ≤2N = **512 structured errors**, each with ≤3 shallow types and bounded names/spans. No aggregate error variants are reachable. Typed diagnostic rendering to 16 messages/4,096 bytes is Task 5's separate obligation. |
| `check.rs:496`; `types/mod.rs:459` | Invalid member receivers can eagerly call `display_name`, even when final diagnostics are truncated. A ≤H list/wrapper chain ends in a ≤128-byte name; its final spelling is <512 bytes. Every intermediate formatting allocation is bounded by that size, ≤H×512 bytes copied per offending node; ≤NH×512 across the tree. This is included before rendering limits. |
| AST, type, error, scope and substitution cleanup | Checker borrows the AST and does not clone it. AST destruction remains ≤33 nested AST levels; accepted types are unary Arc chains of ≤H nodes, not expanded records. Dropping map entries is a bounded loop. Arc-owned last-reference cleanup cannot follow schema cycles. |

Excluded edges are explicit: `check_list`, `check_map`, `join_types`, `type_specificity`, `check_struct`, `get_type_name`, `check_member_test`, `check_bind`, optional wrapping and arbitrary/member-call qualification do not run under the pre-check profile. Their potentially wider loops/recursion are not justified by this audit. The `try_enum_constructor` helper is entered for the two allowed standalone calls but exits at the closed resolver's None result.

## Conservative work and stack accounting

These are traversal/allocation counts, not a latency promise or runtime fuel charge. Hashing/string comparisons also multiply by bounded key lengths; hash-map adversarial collisions can be conservatively treated as scanning the full bounded map. No assumption about expected hashing performance is needed for finiteness.

For one candidate argument, charge two concrete compatibility directions, each ≤2H assignability calls, each containing a structural equality of ≤H nodes plus its own node visit. Add 4H for widening/contains/equality scans. The TypeParam dispatch/compatibility constant overhead and fixed signature/result passes fit within an additional 8H per candidate. Thus a generous bound for three arguments is

`B = 3 × [2 × (2H) × (H + 1) + 4H] + 8H = 4,464`

type-node/helper visits per candidate. This deliberately overcounts nongeneric candidates and the fixed shallow signatures. Across checking, `5N × B = 5,713,920` such visits, plus 13,824 finalization visits, root/type clones and qualification/diagnostic string-copy bounds above. Persistent map entry clones (327,680) are counted separately rather than misdescribed as deep type copies. Map insertion/lookups, including bound/substitution lookup, each scan at most 257 candidate entries in the worst collision model. The linear unary type invariant is what avoids exponential branching in this bound.

The source-level simultaneous recursion is also bounded separately from total work:

- AST descent: ≤33 `check_expr` frames and ≤33 node-specific helper frames = **66**. The only iterator-based child checking is the fixed literal timestamp argument, so it cannot introduce an iterator layer on every deep AST edge.
- Matching adds `resolve_function_call`, `resolve_with_function`, `resolve_overload`, `try_match_overload` = **4** frames. At a generic parameter, one outer `is_assignable` and one `is_types_compatible` frame may remain while ≤2H = **36** concrete assignability frames descend.
- Equality/contains/widen/substitute recursion has ≤H unary levels; substitution has at most one extra parameter edge. Derived `CelType` equality and Arc forwarding are conservatively accounted as three frames per level (54), rather than counting a deep equality as one operation. These paths are sequential, not multiplicative or cumulative across candidates.
- This gives **66 + 4 + 2 + 36 + 54 = 162** named checker/type/Arc frames on the conservative matching path. Qualification instead adds at most 33 recursive name-helper frames to the ≤66 AST/helper frames. Finalization runs after AST descent and has ≤H type levels. Scope/resolver/collection/allocator/formatting entry plumbing contributes fixed implementation-dependent frames, not recursion dependent on unchecked input.

This is a bound on the relevant source call graph, **not a compiler-independent proof of 256 KiB machine-stack consumption**. Rust compiler stack-slot allocation, inlining and standard-library plumbing affect bytes. The required empirical complement is a fresh subprocess running the actual checker and cleanup on a native thread explicitly sized to 256 KiB; target-specific Wasm execution belongs to the integration evidence. Increasing the tree/type envelope or introducing recursive callback work requires new source and stack evidence.

## Regression evidence and limits of the fixtures

`tests/profile_checker_bound.rs` contains three tests. The initial RED run admitted a depth-33 AST into the checker and failed the max-node assertion; after implementing the test-only schema/envelope fixture helpers, all three passed. The helpers validate fixture dimensions and schema graphs only. They do not implement canonical-macro validation, token/name policy or the production boundary, and must not be imported into production.

The subprocess corpus checks actual `Env::check` result types and structured error variants, not just completion. It covers exactly 256 nodes, AST depth 32, 16 nested lists, an additional nullable wrapper, mismatched deep list generic candidates, depth-32 ternaries, known and unknown-root depth-32 member chains, valid maximum schema member depth, nullable equality/ternary, index success and failure after generic binding, now/timestamp, canonical exists, many equality substitutions, many invalid fields, and all schema capacity maxima together. It verifies bounded type/reference maps and structured errors. Upstream permissive Dyn inference is asserted explicitly to prevent confusing checker success with final profile acceptance.

Fixture parsing occurs on the child's ordinary test thread **before** spawning the small-stack checker thread, using small trusted source strings. This intentionally isolates checker/registry/cleanup evidence from Task 2's parser evidence. All fixture data, schema validation/conversion, checking, result checks and destruction inside the spawned closure must finish before the child completion marker is emitted. Parent checks both successful child exit and the marker, so a stack-overflow abort cannot appear as a pass. Source-language stack bounds and executable-input admission still depend on Task 2/Task 5 production validation and their own tests.

No uncovered recursive checker edge remains under the stated table/profile/resolver preconditions. This conclusion does not apply until the production preconditions and post-check type/overload validation are wired and independently reviewed.
