# Maintained cel-core fork

Repository backend maintainers own this local fork, its patch, upgrades, and review. No upstream publication or external maintainer assignment is required.

## Exact upstream and licenses

Base: `cel-core 0.5.1`, repository <https://github.com/ponix-dev/cel-core>, packaged commit `77277ec6a336646d946ee549a5012116378b82e4`, path `crates/cel-core`. The original `.cargo_vcs_info.json` is retained verbatim as upstream provenance, not a claim that the local tree is unmodified.

The crates.io archive <https://static.crates.io/crates/cel-core/cel-core-0.5.1.crate> has SHA-256:

`6b317c80c3b45ab6c8a636feaf0e51798a2c7394cee9a1f54ed83d2addb1ecb5`

All packaged files are retained, including both manifest forms, the upstream `Cargo.lock`, examples, and VCS metadata. The published crate declares `MIT OR Apache-2.0` but does not bundle license texts. The two added license files come from the exact upstream commit and retain its notices:

- [LICENSE-MIT](https://raw.githubusercontent.com/ponix-dev/cel-core/77277ec6a336646d946ee549a5012116378b82e4/LICENSE-MIT), SHA-256 `d611016d5f19df41427f68511735672d14319aceb85d9119eb9d469ac1254e60`.
- [LICENSE-APACHE](https://raw.githubusercontent.com/ponix-dev/cel-core/77277ec6a336646d946ee549a5012116378b82e4/LICENSE-APACHE), SHA-256 `87f4be1ace1f3fe7b3bf000ed0a80c7d54d8a0e8a29f07ab0fb4310ed1bcc2bf`.

`cel-core-0.5.1.patch` is the complete exact patch from that archive. At establishment it is byte-identical to the reviewed experimental patch: 169 added lines, one removed line, six source files. It adds `src/eval/fuel.rs` and changes five existing modules; manifests and examples remain upstream. Subsequent fork tasks must regenerate this same patch with all cumulative changes. Since B3 (`426c2b1`) the patch also changes the manifest and its lock: `chrono` is a dependency with `default-features = false` and `std`, so the lock drops `iana-time-zone` and the `windows-*` packages; the patch was regenerated for that on 2026-11-13. The added license texts are verified by the independent hashes above rather than represented as upstream source changes.

## Reproduce and verify

Run from the repository root. The verifier reads the Cargo cache (`CARGO_HOME` is respected), checks the archive hash, extracts to a temporary directory, applies the patch with zero fuzz, compares the exact file inventory and bytes, and checks both license hashes. It performs no network requests or repository writes. Only Cargo's generated root `target/` directory is excluded from the file inventory; `Cargo.lock` is checked.

```sh
python3 vendor/cel-core-provenance/verify.py --self-test
python3 vendor/cel-core-provenance/verify.py
cargo test --manifest-path vendor/cel-core/Cargo.toml --lib
```

If the pinned crate is not cached, obtain it separately and pass its path. The verifier enforces the same checksum for an explicit archive:

```sh
curl --fail --location https://static.crates.io/crates/cel-core/cel-core-0.5.1.crate \
  --output /tmp/cel-core-0.5.1.crate
python3 vendor/cel-core-provenance/verify.py --archive /tmp/cel-core-0.5.1.crate
python3 vendor/cel-core-provenance/verify.py --self-test --archive /tmp/cel-core-0.5.1.crate
```

The self-test independently reconstructs upstream plus the recorded patch, confirms that pristine reconstruction and the maintained tree pass, changes one byte in `src/lib.rs`, and requires a nonzero CLI exit naming `src/lib.rs`. It also exercises manifest, lockfile, example, VCS and license mutations, missing source, and unrecorded files. `--tree PATH` allows verification of a separate candidate tree without editing the maintained copy.

## Scope and maintenance policy

The inherited opt-in `Program::eval_with_budget` is a low-level experimental meter. It shares evaluator fuel, charges dispatch/loop work and selected string operations, and reports terminal exhaustion. It does not bound general parser/checker work, arbitrary callbacks, expensive standard functions, or output formatting. Unmetered upstream entry points retain upstream semantics. Merely establishing this maintained copy does not make it safe for untrusted production use.

The bounded entry point, sealed production wrapper, resource audit and native/Wasm differential evidence are separate reviewed tasks (see the bounded-expression audits in `docs/architecture/`). The SUBSET implementation was independently accepted at c49ac93; A4 integration is permitted under [the adoption decision](../../docs/architecture/cel-decision.md). This does not approve the generic upstream API for untrusted use. No delivery estimate is established here.

For every upstream upgrade or fork change, backend maintainers review the exact diff, licenses and dependency notices; record the upstream version, commit and archive hash; rebase and regenerate the complete patch from the verified archive; and run this verifier, the original upstream regression suite, and the complete bounded native/Wasm corpus once implemented. Changes to observable language or accounting behavior require review and a profile/cost version bump as appropriate (`runbook-cel/1`, `runbook-cost/1`). A workspace-wide crates.io override is prohibited: the maintained crate stays excluded from workspace membership and the eventual wrapper uses a fixed local path dependency.
