# Compiler metadata boundary audit

The A5 additive contract exposes read-only checked-template expression references
with original byte spans and conservative root/first-member dependencies. A
syntax-only summary supports dependency ordering before variable types exist.
It has no checked authority, schema fingerprint, executable program or runtime
read provenance. Full checking remains mandatory before evaluation.

`compile::parse_expression` factors the existing lexical, bounded parser and
error mapping path. Syntax summaries then call the unchanged pre-check profile
gate before collecting metadata; a private checker-entry counter verifies that
the summary does not enter checking. Checked compilation collects the same
summary after admission. Restore recomputes it through checked compilation.
No AST, resolver, program or caller callback escapes the wrapper.

The collector is iterative over the admitted AST (at most 256 nodes/depth32).
Its pending work and borrowed root/member pairs are bounded by that tree; names
are at most 128 bytes. Deduplication scans at most 256 pairs and copies only each
new pair. It neither traverses schemas nor expands paths through collection
aliases. All conditional branches are conservatively visited. Member chains
retain the first member; indexes also visit the index expression. A bare root
is a whole-root dependency. Callee names, comments and string contents are not
binding roots. The sole admitted nonnested exists visits its range outside the
alias scope and original predicate with the alias suppressed; the range already
covers the alias subtree. Synthetic accumulator nodes add no binding dependency.

The sequential template scanner is shared by checking and summaries. The same
65,536-byte source and 64-expression caps apply before processing/copying. Each
segment is processed before scanning the next, preserving earlier checked-error
precedence over later malformed delimiters. Source spans preserve untrimmed UTF8
expression interiors. The checked handle remains immutable; boxing its private
segment variant adds at most 64 bounded allocations and no evaluation operation.

Independent review passed without findings. Public tests cover chains, indexes,
all branches, alias shadowing and scope restoration, descriptor restoration,
exact spans and admission boundaries. Twenty independently expected metadata
rows participate in fresh native/Wasm parity (396 total rows). After the private
boxing correction, 56 affected tests including fresh parity passed; strict Clippy,
Wasm compilation and formatting passed. Prior full expression run passed 127 tests.
No vendor patch, profile, cost, runtime reads or resource constants changed.

## Shared immutable resolver registry

TypeEnv now owns a private OnceLock<Arc<ClosedResolver>>. The first checked
compilation builds the same validated field maps; later checks share them through
SealedProtoRegistry. The cache does not point back to TypeEnv, so programs can
retain it after the environment drops without forming a cycle. Initialized
TypeEnv clones share the cache; clones made before initialization may each build
one, so the compiler still uses shared Arc<TypeEnv> handles per scope.

A test of actual check_expression calls failed with two resolver builds before
the change and passes with one afterward. Lifetime/Weak checks establish release
after the final program drops. Distinct schemas and caller mutations remain
isolated. This removes per-program field-map copies, not per-environment schema
storage or per-program checked AST/type maps. Compiler aggregate admission still
needs its own limits. Independent review passed; 203 workspace tests including
fresh 396-row Wasm parity, strict Clippy, formatting and Wasm checks passed.
No public API, profile, fingerprint, cost or vendor change was made.
