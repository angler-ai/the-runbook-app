use runbook_api::search::parse_query;
use std::collections::BTreeMap;
#[test]
fn search_filters_are_typed_and_bounded() {
    let q = BTreeMap::from([
        ("status".into(), "running,queued".into()),
        ("tags".into(), "blue,prod".into()),
        ("field.score.min".into(), "2".into()),
        ("field.score.max".into(), "10".into()),
        ("field.severity".into(), "\"major\"".into()),
        ("from".into(), "1".into()),
        ("to".into(), "20".into()),
        ("limit".into(), "2".into()),
    ]);
    let parsed = parse_query(&q).unwrap();
    assert_eq!(parsed.filters.statuses.len(), 2);
    assert_eq!(parsed.filters.fields.len(), 2);
    assert_eq!(parsed.limit, 2);
    for (key, value) in [
        ("status", "bogus"),
        ("limit", "0"),
        ("limit", "101"),
        ("field.score.min", "\"oops\""),
        ("unknown", "x"),
    ] {
        assert!(parse_query(&BTreeMap::from([(key.into(), value.into())])).is_err());
    }
}
