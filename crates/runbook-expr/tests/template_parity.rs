#[path = "support/template_parity.rs"]
mod template_parity;

#[test]
fn template_and_ingress_rows_have_hand_checked_outcomes() {
    let rows = template_parity::rows();
    let names: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|row| row["name"].as_str().expect("named row"))
        .collect();
    for required in [
        "template/whole-list",
        "template/whole-record",
        "template/text-list-error",
        "template/text-record-error",
        "template/scalars",
        "template/utf8-escaped-cost",
        "template/quoted-delimiter",
        "template/escaped-delimiter",
        "template/commented-delimiter",
        "template/nonrecursive",
        "template/bound-null",
        "template/literal-null",
        "template/supplied-time",
        "template/fuel-single-a",
        "template/fuel-single-b",
        "template/fuel-prefix",
        "template/fuel-third-unread",
        "template/fuel-sticky",
        "template/late-output-prefix",
        "template/late-output-third-unread",
        "template/late-type-prefix",
        "template/late-type-third-unread",
        "template/late-type-recovery",
        "template/late-missing-prefix",
        "template/late-missing-third-unread",
        "template/late-missing-recovery",
        "template/encoded-boundary",
        "template/encoded-overflow",
        "template/output-reuse-first",
        "template/output-reuse-boundary",
        "template/output-reuse-overflow",
        "template/cancel-first",
        "template/cancel-reuse",
        "template/cancel-empty",
        "template/schema-empty",
        "template/schema-literal",
        "ingress/deep-borrowed-rejection",
        "ingress/list-width-overflow",
        "ingress/aggregate-byte-boundary",
        "ingress/aggregate-byte-overflow",
        "ingress/node-boundary",
        "ingress/node-overflow",
    ] {
        assert!(names.contains(required), "missing required row {required}");
    }
    assert_eq!(names.len(), rows.len(), "unique row names");
    assert!(rows.iter().any(|row| {
        row["outcome"]["stats"]["reads"]
            .as_array()
            .is_some_and(|reads| !reads.is_empty())
    }));
}
