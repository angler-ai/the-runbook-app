# Bounded parser source audit

Task 2, 2026-11-09. This is an implementation evidence fragment for the coordinator's complete bounded-expression audit. It is not an A4 acceptance decision. Source inspected: maintained `vendor/cel-core/src/parser/{mod,lexer,parse,macros}.rs`, `src/limits.rs`, and AST definitions in `src/types`. The unbounded public APIs remain available; the portable wrapper must exclusively use `parser::parse_bounded` and reject nonempty `ParseResult.errors`.

## Entry and lexical allocation

`CompileLimits::runbook_v1()` has private fixed constants and no caller overrides. The parser admits at most 4,096 source bytes, 512 tokens, 4,096 decoded bytes per literal, 256 active parsing-method bodies, 512 allocated AST IDs including generated macro nodes, 64 tree edges during construction, and one one-variable `exists`. Diagnostic output is capped at 16 messages / 4,096 bytes. Name length / ASCII identifier validation, language exclusions, and the final 256-node / 32-edge AST envelope belong to the wrapper; bounded parsing alone does not assert profile acceptance.

`parser::parse_bounded` checks source length before `Token::lexer` is constructed. `lexer::lex_bounded` uses the existing Logos grammar and checks token count before inserting each token. The 513th token may be scanned and its callback may run, but its bounded token value is never inserted and parsing never starts. Every scan is bounded by the already checked source length. Whitespace and `//` comments do not consume token slots. Literal callback errors are translated to outer resource errors before ordinary lexical-error conversion.

| Lexical source sites | Bound / allocation order |
| --- | --- |
| `lex_decimal_int`, `lex_decimal_uint`, `lex_hex_int`, `lex_hex_uint`, `lex_float` | Scan/convert at most 4,096 bytes; no input-dependent recursive calls. |
| Inline `IntOverflow`, reserved-word, ordinary-identifier callbacks; `lex_backtick_ident` | String copies are slices of the checked source (reserved words and overflow spelling are fixed). Profile name rules are checked by the wrapper. |
| `lex_quoted_string`, `lex_triple_string`, `lex_raw_string` | Every decoded character goes through `push_literal!`; checked addition and limit comparison precede `String::push`. Escape hex/octal temporary strings have at most eight Unicode input characters / 32 bytes. Invalid escape, Unicode scalar, or termination returns ordinary syntax failure. |
| `lex_quoted_bytes`, `lex_triple_bytes` | Every byte push uses the same guard; every UTF-8 `extend_from_slice` checks the encoded length first. UTF-8 encoding uses a four-byte stack array. |
| `lex_raw_triple_string` | Checks content byte length before `to_string`. Search is over the checked source. |
| Single/double/raw/triple string and byte wrapper callbacks | Delegate to the guarded helpers above. Raw byte wrappers consume the string with `into_bytes`, without an additional content copy. |

Decoded literal length cannot exceed source bytes with the accepted CEL escape transformations, so the public 4,096-byte source gate is already a conservative literal-size bound. The explicit callback guards remain independently exercised by private tests at 4,096 / 4,097 decoded bytes. Unsupported raw/triple/bytes forms still use guarded callbacks before the wrapper rejects them. Lexer errors in bounded mode use fixed text instead of formatting an arbitrary token/source value.

## Recursive method graph

All 19 grammar methods below acquire `enter()` before doing grammar work. The guard rejects when 256 bodies are active. `FrameGuard::drop` decrements on normal returns and every `?`/error return. No exception, panic, unwind, timeout, or operating-system stack overflow is used as resource cancellation. Nonrecursive peek/match/expect/allocation helpers add only a fixed call-depth overhead. Each loop consumes a token or returns; total successful token advancement is at most 512.

| Method | Calls that can continue the grammar |
| --- | --- |
| `parse_expr` | `parse_ternary` |
| `parse_ternary` | `parse_or`; `parse_expr` for both branches |
| `parse_or` | `parse_and` in a consuming loop |
| `parse_and` | `parse_relation` in a consuming loop |
| `parse_relation` | `parse_addition` in a consuming loop |
| `parse_addition` | `parse_mult` in a consuming loop |
| `parse_mult` | `parse_unary` in a consuming loop |
| `parse_unary` | `parse_unary` for `!`/`-`; `parse_postfix` otherwise; negative i64 minimum is a leaf |
| `parse_postfix` | `parse_atom`, then consuming `parse_call`, `parse_index`, `parse_member`, or `parse_struct_init` |
| `parse_call` | `parse_expr` for arguments |
| `parse_index` | `parse_expr` for the index |
| `parse_member` | No grammar recursion |
| `parse_struct_init` | `parse_struct_field` in a consuming loop |
| `parse_struct_field` | `parse_expr` for the value |
| `parse_atom` | `parse_expr` for grouping; `parse_list` / `parse_map` |
| `parse_list` | `parse_list_element` in a consuming loop |
| `parse_list_element` | `parse_expr` |
| `parse_map` | `parse_map_entry` in a consuming loop |
| `parse_map_entry` | `parse_expr` for key and value |

The parser carries boxed expressions and boxed errors through recursive calls. `node` is a nonrecursive, non-inlined construction helper, keeping large `Expr` temporaries out of the recursive stack frames. This was necessary: the original value-carrying implementation overflowed a 256 KiB debug stack even after adding the logical frame guard. A further combined-stack test exposed the upstream predicate clone, eliminated as described below.

For the inspected aarch64 Darwin Rust 1.99.1 debug build, generated assembly frame sizes range from 128 bytes (`parse_expr`) to 2,944 (`parse_map`). A maximum-weight walk of the actual grammar-call graph of up to 256 admitted methods is 242,160 bytes, before fixed helpers, entry frames and cleanup. This is supporting implementation inspection, not a portable byte-stack proof. The complete 256 KiB subprocess corpus tests actual entry/helper/drop overhead on this native build; Wasm execution and other target builds remain part of the coordinator's separate acceptance evidence.

## Node allocation, containers and destruction

Every bounded grammar node goes through `next_id(children)` then `allocate(height,size)` **before** executing its construction closure. The allocator rejects node 513 before incrementing, and rejects height 65 before creating the parent or its boxes. Children already have metadata in fixed `[u8; 513]` height and `[u16; 513]` size arrays, keyed by private IDs. Child-height maximum plus one gives the new height. Child sizes sum to at most the number of previously allocated nodes; the 512-node cap keeps all arithmetic inside `u16`. Macro reservation uses the same allocator.

Audited constructors: ternary (three children); each of the five binary precedence levels (two); unary (one); call (callee plus arguments); index (base plus index); member (base); struct (type expression plus field values); list (elements); map (keys and values); literal/identifier/root-identifier/negative-minimum leaves (none). Flat postfix and binary loops therefore cannot bypass tree depth. Grouping does not allocate a node. Argument/field/element/entry vectors only receive successfully parsed children, so their lengths and allocated capacities are bounded by the token and node envelopes. Token and name/literal clones inside grammar methods copy only bounded token data, never an AST.

`Budget.work` increments on successful token advancement and ID allocation: at most 512 + 512 = 1,024 such events. The counter is evidence/accounting, not an additional public policy limit. Bounded macro preflight and finite helper work are separately inspected.

On failure, every owned partial AST is already at most 64 edges deep and every container is bounded. Dropping rejected candidates cannot discover an unchecked deeper AST; the parent is rejected before construction. On successful return, unboxing the root transfers the same bounded tree into `ParseResult`. Recursive AST clone is absent from the bounded path. The counter tests confirm zero live admitted frames after resource returns; the subprocess corpus also exercises destruction after both successful and malformed parses.

## Macro edge and direct ID audit

`parse_call` branches on bounded mode **before** the upstream `args.clone()` call. It inspects the borrowed callee name and argument count, permits only receiver-style `exists` with two arguments, and routes this to `expand_bounded_exists`. Other names and arities remain ordinary calls for wrapper rejection. The first argument must be an identifier before the callback runs. A second expansion, including an outer expansion after an inner one, returns `MacroCount` before cloning/callback work.

`expand_bounded_exists` computes complete output height `max(receiver_height + 1, predicate_height + 2, 2)` and checks room for seven IDs before invoking the standard callback. It reserves all seven through `allocate` in the exact upstream order:

1. Comprehension root (receiver size + predicate size + 7).
2. False accumulator initialization.
3. Accumulator reference for the loop condition.
4. Unary-not loop condition (height 1).
5. Accumulator reference for the loop step.
6. Predicate-or-accumulator step (predicate height + 1).
7. Accumulator result reference.

The callback's ID provider only returns these seven reserved IDs. `MacroContext` receives no source-retention callback. In `expand_exists_2arg`, absence of that callback takes an ownership-moving branch: it pops the predicate and passes an empty retention-argument slice to the existing `expand_exists_impl`. The receiver is moved out of the parsed callee. Thus bounded mode performs **zero recursive subtree clones**, while preserving canonical AST shape, IDs and source spans. The ordinary retaining path keeps its previous behavior and all original macro tests pass.

The remaining direct `next_id` increments in `try_macro_expansion` (wrong arity, context generator, expansion error) and its receiver/argument/original-call clones are reachable only from `parse_call`'s unbounded branch. `extract_call_info`, stored macro-call construction, and all other standard/custom expanders are likewise unreachable from the bounded entry. No public options or registry can change this fact. New upstream expansion nodes or changes to the seven-ID callback order require updating this audit and its tests.

## Diagnostics and evidence

Bounded parse errors use fixed messages at every offending-token formatting site; a 1,500-byte control-character literal cannot create an expanded Debug diagnostic. The bounded path stops after its first syntax failure, or reports one trailing-token error; macro validation returns ordinary syntax errors directly. The final diagnostic gate enforces 16 / 4,096 defensively. Outer `Err(CompileLimitError)` is reserved for resource failure; `Ok` with nonempty errors never implies a usable AST.

Executed tests: all 331 original library tests; four bounded unit tests (lexical callback boundaries, hook ordering, 513th-ID refusal, complete macro reservation); nine bounded integration tests; and the separately owned three checker tests. All pass. The subprocess starts an actual 256 KiB thread and fails if the child aborts or panics. It includes 2,000 unary tokens, nested unary/group/ternary/index/call/list/map/struct forms, member-depth boundaries, macro height and node reservations, plus 3,780 deterministic combinations of nesting/unary/member-chain depth with valid and malformed forms. Tests contain no `catch_unwind` resource implementation.

The parent owns exact fork patch regeneration, whole-library profile/schema/checker/evaluator evidence, Wasm execution, and independent acceptance. This fragment does not relax any reviewed limit or permit A4 adoption.

## Bounded macro hygiene follow-up
The bounded exists path now gives its internal accumulator the name `@result`; the Runbook source token gate rejects raw `@` and backtick identifiers and TypeEnv accepts ASCII identifiers only, so admitted source cannot capture it. MacroContext defaults retain upstream legacy naming for unbounded parsing. Standalone parse_bounded is only a resource boundary and still accepts out-of-profile syntax; the complete hygiene guarantee belongs to check_expression. Allocation/node/frame counts are unchanged. Regressions prove undeclared source `__result__` fails checking, a declared root remains readable, an iterator can shadow that root intentionally, and the runtime sees the supplied true root. The upstream AST shape comparison is alpha-equivalent with only the three generated accumulator references renamed. All11 bounded-parser tests, including the256KiB stack corpus, pass. Independent review accepted the fix; low-priority comment precision and extra gate tests were incorporated.
