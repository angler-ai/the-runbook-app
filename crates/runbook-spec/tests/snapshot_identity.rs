//! D21 (revised): snapshots keep values exactly as written (actors receive `with`
//! parameters verbatim), while the semantic hash normalizes number spelling. So
//! an equal semantic hash does NOT imply equal snapshot bytes. Snapshot blobs are
//! therefore addressed by the SHA-256 of their bytes; the semantic hash is the
//! version's identity. This test pins both facts for the B3.1 publish path.
use runbook_spec::{compile, parse, restore};

fn spec(default: &str, threshold: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: numbers, title: Numbers}}\n\
         spec:\n  inputs: {{ratio: {{type: number, default: {default}}}}}\n  steps:\n    \
         - id: check\n      title: Check\n      with: {{threshold: {threshold}}}\n"
    )
}
fn build(default: &str, threshold: &str) -> runbook_spec::CompiledRunbook {
    compile(
        &parse(&spec(default, threshold)).unwrap(),
        &Default::default(),
    )
    .unwrap()
}

#[test]
fn equal_semantic_hash_may_have_different_snapshot_bytes() {
    let base = build("3.0", "0.5");
    for (default, threshold) in [("3.00", "0.50"), ("3e0", "5e-1")] {
        let other = build(default, threshold);
        assert_eq!(other.hash(), base.hash(), "{default} {threshold}: hash");
        assert_ne!(
            other.snapshot_bytes(),
            base.snapshot_bytes(),
            "{default} {threshold}: spelling is preserved, so bytes differ"
        );
        let restored = restore(other.snapshot_bytes()).unwrap();
        assert_eq!(restored.hash(), base.hash());
        assert_eq!(restored.snapshot_bytes(), other.snapshot_bytes());
    }
    assert_eq!(build("3.0", "0.5").snapshot_bytes(), base.snapshot_bytes());
    assert_ne!(build("3.5", "0.5").hash(), base.hash());
}
