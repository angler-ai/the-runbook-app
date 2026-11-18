# Closed schema, bindings and compiler admission audit

This source audit closes the integration boundaries between the parser/checker,
runtime and output audits. It covers `runbook-expr/src/{schema,bindings,compile,profile}.rs`
and the prepared activation in `runtime.rs`. It is evidence for the final
implementation review, not permission to start A4 by itself.

## Borrowed schema validation

`TypeEnv::new` borrows the caller's flat `SchemaDef` and calls `validate` before
fingerprinting or cloning it. Constant-time cardinality gates reject more than
1,024 type nodes or 256 roots. The first type pass caps each record at 256 fields
and all declarations, including roots, at 4,096 before allocating adjacency lists.
Names are checked for their 128-byte limit before their ASCII identifier scan;
every referenced TypeId must exist. Nullable lists are rejected explicitly.

The graph contains at most 1,024 list edges plus 4,096 record edges. Iterative
three-color DFS visits every type, including unused types, and detects cycles.
Memoized subtree heights preserve the longest path through shared subgraphs;
height greater than 16 rejects. Its color/height arrays and explicit stack have
at most 1,024 entries, and cleanup is flat. Field declarations contain IDs rather
than owned recursive types, so rejected caller schemas are not recursively
destroyed by this API.

Only validated data reaches the length-prefixed SHA-256 fingerprint and owned
copy. Fingerprinting streams names, flags and IDs directly, without a serialized
temporary buffer. The root/field maps have deterministic ordering. CEL type
lowering recursively follows only validated list chains, at most 16 edges;
records remain opaque message names. Root declarations are capped at 256 and
the closed resolver at 4,096 fields. Resolver construction therefore cannot
expand a record graph recursively. The compiler wraps this resolver in the
narrower `SealedProtoRegistry`, whose qualified/type/enum resolution returns None
as required by the checker proof.

## Borrowed binding validation

`Bindings::new` borrows roots and their explicit root-container origin. It first
runs `validate_shape`, using one borrowed child iterator per active level. Before
descending or copying, this pass enforces 256 roots/record fields, 100,000 list
elements, 65,536 bytes per string, 262,144 nodes and depth 32. A rejected node at
depth 33 is inspected only for that depth gate; no owned subtree or input-width
work list is created. The caller retains ownership of any unvisited tail.

The second iterative pass checks supplied event time, the root-container origin,
declared fields, exact value types/nullability, canonical paths and identities.
It accumulates at most 8,388,608 bytes of root/field names, strings, paths and
identity strings before conversion. Paths are capped at 1,024 bytes, identity
strings at 128 and iteration vectors at 32 before scanning them. Duplicate
iteration-instance checks are bounded by that 32-entry vector. Timestamp seconds
and nanos are restricted to the documented UTC calendar range; doubles must be
finite. There is no numeric or string coercion here.

Field/index path comparisons check lengths before scanning, and index spelling
uses a fixed 20-byte buffer. Scalar aliases are the only roots allowed to use an
external canonical indexed path; that parser receives an already length-bounded
path and validates each identifier/index without allocation. Unknown-field
checks compare map cardinality first, then bounded names against the closed
declaration map. Required absence and explicit nullable Null remain distinct.

The temporary `steps` and `origins` BTreeMaps borrow validated strings, identity
slices and values. Each has at most the admitted node count plus the one root
identity; comparisons operate on bounded keys. Repeated-origin checks reject
collections before value equality, so only bounded scalars can be compared.
Their purpose is consistency, not source discovery by scalar equality. A step
run cannot occur with inconsistent iteration identities; leaf revisions may
differ, while duplicate canonical scalar aliases must agree on revision/value.

Only after both complete passes succeed does iterative postorder `convert`
allocate accepted immutable nodes. Its explicit work/value vectors may grow
with accepted width, but never beyond the admitted node count and a constant
factor of pending finish records. Names, strings and origins are copied from
the fully validated byte envelope. Accepted source trees and partial converted
trees have bounded depth, so ordinary Arc/tree cleanup is bounded too.

Prepared CEL lists are constructed once. Subsequent evaluation shares their
Arcs; records share their field maps, with a bounded generated type-name copy.
List identity indexes contain at most one entry per accepted list and retain
the same nodes. They introduce no parent/back-reference cycles. This is a bound
on logical nodes and copied string/metadata bytes, not an 8 MiB bound on allocator
overhead, map nodes, CEL values, schema storage or total process memory.

## Checked compilation and restoration

`validate_source` uses the source-first bounded lexer, checks names and excludes
unsupported tokens before parsing. Parsing lexes again under the same fixed
limits: this is two bounded scans, not an unbounded fallback. Nonempty recovered
parser errors are rejected before any checker entry.

`profile::validate_parsed` builds a flat borrowed view with at most 256 unique
AST IDs and depth 32 before invoking the recursive checker. It admits only
scalar literals, projections/indexes, permitted operators, typed ternary, fixed
time calls and one canonical nonnested exists. Each admitted form has at most
five children; the pending vector is bounded by the already bounded parser
tree. The exists accumulator is `@result`, inaccessible through admitted source
tokens or schema names. No caller can supply a raw AST, resolver or callback.

The checker is created from `Env::new` with exactly 14 functions/31 overloads,
the validated roots and sealed field resolver. The separate checker audit
accounts for recursive inference, candidate substitutions, recovery and cleanup.
After checking, operations and all value types/overload references are checked
again: Dyn/Error, arbitrary overloads, string indexing and collection equality
cannot reach runtime. A record/null comparison is the explicit exception for
presence consumption. At most 16 fixed error summaries are assembled; arbitrary
CEL types, source strings and error values are never generically formatted into
diagnostics.

Result-type construction iterates through at most 17 list/wrapper constructors
and resolves opaque record IDs back through the validated arena. It does not
expand record fields. The nullable-record scan is bounded by 1,024 types and
4,096 declarations. Reverse AST metadata propagation has at most 256 entries
and conservatively preserves nullable record/timestamp ternaries. Timestamp
literals are at most 64 bytes, parsed once and cached by bounded AST ID, with
calendar/nanosecond validation before program construction. Program/AST copies
and destruction inherit the admitted node/depth limits.

Persisted descriptors contain only source, profile, cost model and schema
fingerprint. `restore_expression` checks those identities and re-enters the
entire bounded compiler; deserialization never manufactures a checked program.

## Runtime boundary and evidence

Only private repository-owned activation/registry adapters can supply values.
Root/field lookup returns a prepared scalar, shared list or bounded record clone.
No schema walk, binding conversion, clock read, storage operation or user callback
occurs during lookup. `evaluate_in` checks terminal state and the schema identity,
uses supplied event time, then materializes through the bounded output helper.
Text rendering uses the same sealed raw evaluation and output accounting. Raw
CEL values and all adapters remain private.

Native schema/binding tests cover graph sharing/cycles, limits, canonical aliases,
identity consistency, exact aggregate bytes, a borrowed deep tree on a small
stack, and counters proving rejection before copying. Compiler tests prove that
rejected syntax/envelopes never enter the real checker. The fresh Wasm corpus
adds target-specific evidence; the final corpus report must distinguish executed
cases from native-only private guards. Source bounds, the other audit fragments,
integrated checks and independent final review are all required for adoption.

Duplicate canonical scalar snapshots compare finite doubles by their bits, so
negative and positive zero cannot share one origin/revision with different
serialized output. This borrowed scalar-only comparison never traverses
collections and does not alter CEL numeric equality.
