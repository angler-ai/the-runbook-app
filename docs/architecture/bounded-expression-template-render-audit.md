# Bounded template rendering audit

Date: 2026-11-09. T11b consumes T11a's checked scanner and T10's reviewed private output helpers. This fragment covers rendering only; compiler/scanner and raw-evaluator/output bounds remain documented in their existing audits. It does not authorize A4 adoption or establish actual native/Wasm template parity.

## Entry and type preservation

`template::render_template` checks existing terminal state before schema identity, output charges, or expression dispatch. The template's retained fingerprint is compared with the bindings even when there are no expressions or the source is empty. Schema mismatch is nonterminal and consumes no fuel. The sealed WholeValue invariant is exactly one checked expression; that branch delegates to `evaluate_in`, retaining JSON primitive/list/object types and the existing iterative output/provenance checks.

Text mode charges one node and two encoded quote bytes through `charge_output` before creating its output string. It visits at most 129 checked segments (64 expressions and 65 literals); there is no recursive interpolation and returned strings are only appended. The renderer does not clone checked programs, scan returned data for delimiters, or construct a second source/program.

## Fragments, accounting, and storage

Every literal uses `charge_json_string_content` before `push_str`. Every expression uses `evaluate_raw_with_source`, formats only the actual scalar with `scalar_text`, records its canonical source Value if present, and then uses the same content precharge before `push_str`. No per-segment public conversion, string node, or JSON quotes are added. Encoded content bytes, including escape expansion, are charged once; final ownership transfer into `serde_json::Value::String` neither copies nor serializes the string again.

The helper prepays raw UTF-8 bytes before scanning escape expansion and prepays expansion before copying. Scalar strings are borrowed; numeric/time text uses T10's fixed 64-byte stack buffer. The accumulated string's logical byte length cannot exceed charged encoded bytes, bounded by 65,536 cumulatively across the session. Standard String capacity growth may reserve capacity above logical length, with a finite bound determined by this output ceiling; the encoded JSON limit is not an allocator-byte claim. On failure, any prior partial string is dropped as a single bounded allocation and no partial result is returned.

Actual non-null lists and records fail `scalar_text` with TypeError before source Value recording, descendant enumeration, or generic formatting. A bound nullable record whose actual value is Null is accepted and prints `null`; its retained canonical source is recorded as Value. A selected literal Null has no source and adds no read. Existing evaluator scalar reads deduplicate through the same ReadTracker, so these explicit final consumption calls do not double-charge or duplicate provenance.

## Shared state and stopping

All segments reuse the supplied session. Failed precharges retain prior successful cost/output/read state and do not add attempted units. Each fallible operation propagates immediately with `?`, so no later literal append, expression dispatch, lookup, read or formatting occurs. Subsequent rendering checks sticky terminal failures before its schema or mode. Ordinary TypeError and MissingValue preserve cumulative state but permit later valid calls. The final terminal check precedes the bounded cumulative statistics clone.

Synchronous rendering offers no asynchronous cancellation guarantee. Cancellation is checked at entry and in the shared raw-evaluation/output helpers; explicit cancellation between calls remains sticky. Existing T8/T9 hooks cover interruption inside raw evaluation.

## Evidence

`tests/templates.rs` covers exact typed list/object/scalar results, outer-whitespace collection rejection, literal/empty text, all permitted scalar formatting categories, delimiter/comment and CEL escape rendering, nonrecursive returned strings, and exact encoded-byte fuel (17 units for a mixed quote/newline/UTF-8 fixture). It checks selected bound-null provenance versus selected literal Null, literal-only schema checks at zero cost, and terminal precedence.

Two individually affordable expression segments exhaust their shared budget, and adding a third lookup leaves the exact same cost/read snapshot. Late output/type failures similarly match a failing prefix snapshot. MissingValue preserves earlier Value/Absence reads and permits subsequent successful evaluation. Quote/control/UTF-8 limit and limit-plus-one fixtures check encoded size; repeated calls fill exactly 65,536 bytes. Repeated empty Text output reaches exactly 16,384 nodes at cost 49,152; the next Text fails before its lookup, and subsequent WholeValue reuse remains terminal. Multiple segments/calls reach 4,096 reads, reject the next distinct read, and retain that terminal state. Successful duplicate reads deduplicate, and cancellation after success preserves the complete prior snapshot.

T10 already owns private output-counter, fixed formatter extreme-value, depth, and materialization guard tests; this renderer does not duplicate their implementation. Native/Wasm template differential evidence and independent implementation review remain subsequent gates.
