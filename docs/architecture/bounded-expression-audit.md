# Bounded expression adoption audit

Implementation and the complete differential corpus are ready for final independent review. SUBSET still blocks A4 until that review accepts the implementation; this index is not the adoption decision.

- [Parser source and resource audit](bounded-expression-parser-audit.md): source-first limits, bounded nodes/frames/diagnostics, moved rather than cloned bounded macro subtrees, safe partial-tree destruction, and native small-stack evidence. Independent task review passed; final target-specific Wasm and whole-subsystem acceptance remain pending.
- [Checker envelope and exact declaration table](bounded-expression-checker-audit.md): independent review accepted the conditional envelope. The reviewed production compiler now enforces the exact audited table, pre-check AST/type limits, and sealed resolver contract; tests also verify no checker entry for rejected trees.
- Closed type environments (`b818cb3`), actual checked compilation, descriptor restore, timestamp constants and nullable-result metadata are implemented and independently reviewed. Nullable string indexing retains the unsupported-operation diagnostic after a reviewed regression fix.
- Binding validation (`840cfd6`) and prepared immutable CEL storage are implemented and reviewed. Accepted collections are converted once under ingress bounds and shared during evaluation.
- [Schema, binding and compiler admission audit](bounded-expression-ingress-audit.md): source inspection of borrowed validation, bounded graph/tree traversal, identity consistency, pre-copy limits, sealed adapters and checked descriptor restoration.
- [Runtime resource audit](bounded-expression-runtime-audit.md): shared terminal fuel, cancellation, fixed event time and precharged lookups/comparisons are implemented and independently reviewed.
- [Observer hook audit](bounded-expression-observer-audit.md): reviewed scoped AST identity, field/index/range/selection hooks, sticky terminal stops and infallible cleanup. The equality-consumption follow-up and actual read tracking are now independently reviewed.
- Checked template scanning is implemented/reviewed (5c78a9d). [Template rendering audit](bounded-expression-template-render-audit.md) records reviewed type preservation, scalar formatting, exact shared output accounting and terminal behavior.
- [Exact read audit](bounded-expression-reads-audit.md): bounded ordered full-identity provenance, missing/null/presence distinctions, virtual-path limits and precharged metadata/diagnostics are implemented and reviewed.
- [Output resource audit](bounded-expression-output-audit.md): typed JSON output, exact encoded-byte/node accounting, cumulative session limits and whole-value provenance are implemented and independently reviewed.
- [Complete native/Wasm evidence](bounded-expression-parity-evidence.md): 364 rows cover admission, evaluation, rendering and the unchanged flagship expression surfaces. Values, structured errors/diagnostics, spans, full ordered provenance, costs and loop entries match in freshly built Wasm under throwing host imports. Final independent implementation review remains required.

The maintained fork's cumulative patch is reconstructed from the checksum-pinned archive by `vendor/cel-core-provenance/verify.py`. The original spike remains unchanged evidence.

The additive A5 [compiler metadata audit](bounded-expression-metadata-audit.md)
covers syntax-only dependencies and checked-template inspection; these do not
replace runtime reads or change the adopted profile.
