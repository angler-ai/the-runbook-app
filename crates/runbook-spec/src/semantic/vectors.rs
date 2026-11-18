//! Shared fixed expectations executed natively and inside actual Wasm.
use super::*;
use serde_json::json;

pub(crate) fn corpus() -> Value {
    // Independent framing specification: object(2), string("a"), object(0),
    // string("z"), array(5), null, false, true, number("1"), string("é").
    // Lengths/counts below are u64 LE; é is two UTF-8 bytes. Digest independently
    // calculated with Python hashlib over these bytes and the three prefixes.
    let tiny = json!({"z":[null,false,true,1,"é"],"a":{}});
    let expected_frame = concat!(
        "050200000000000000",
        "03010000000000000061",
        "050000000000000000",
        "0301000000000000007a",
        "040500000000000000",
        "00",
        "0100",
        "0101",
        "02010000000000000031",
        "030200000000000000c3a9"
    );
    let mut bytes = Vec::new();
    frame(&tiny, &mut |part| bytes.extend_from_slice(part));
    assert_eq!(hex(&bytes), expected_frame);
    let tiny_hash = hex(&hash_value(&tiny));
    assert_eq!(
        tiny_hash,
        "6881e568da40a9895bff21c0ed330aa520fa0741c6f569bf1f67b508977e118a"
    );
    let reordered = json!({"a":{},"z":[null,false,true,1,"é"]});
    assert_eq!(hash_value(&tiny), hash_value(&reordered));
    assert_ne!(
        hash_value(&tiny),
        hash_value(&json!({"a":{},"z":[false,null,true,1,"é"]}))
    );
    let ir = crate::normalize(&crate::parse(r#"{"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":[]}}"#).unwrap()).unwrap();
    let pins = SelectedPins {
        skills: &BTreeMap::new(),
        users: &BTreeMap::new(),
        teams: &BTreeMap::new(),
        project_variables: &crate::parse_declarations(&json!({})).unwrap(),
    };
    // Explicit, complete independently written expected semantic fields.
    let expected: Value = serde_json::from_str(include_str!("minimal-projection.json")).unwrap();
    assert_eq!(projection(&ir, &pins), expected);
    let minimal_hash = hex(&hash(&ir, &pins));
    assert_eq!(
        minimal_hash,
        "3319eca0cb3047c64df9d10a99830d49a196bcd1ff37986322a58179c5a2d645"
    );
    let mut rows = vec![
        json!({"name":"tiny_v1","hash":tiny_hash}),
        json!({"name":"minimal_v1","hash":minimal_hash}),
    ];
    for (i, text) in [
        "9223372036854775808",
        "12345678901234567890123456789",
        "1.0",
        "1e-400",
        "-0.0",
        "1e+400",
    ]
    .into_iter()
    .enumerate()
    {
        let value: Value = serde_json::from_str(text).unwrap();
        let mut numeric = ir.clone();
        numeric.agent_params = Some([("n".into(), value.clone())].into_iter().collect());
        assert_eq!(projection(&numeric, &pins)["agent_params"]["n"], value);
        rows.push(json!({"name":format!("number_{i}"),"number":value.to_string(),"hash":hex(&hash(&numeric,&pins))}));
    }
    let canonical_decimal: Value = serde_json::from_str("1.1").unwrap();
    for spelling in ["1.10", "11e-1"] {
        assert_eq!(
            hash_value(&serde_json::from_str(spelling).unwrap()),
            hash_value(&canonical_decimal)
        );
    }
    assert_ne!(
        hash_value(&serde_json::from_str("1").unwrap()),
        hash_value(&serde_json::from_str("1.0").unwrap())
    );
    assert_eq!(crate::decimal::canonical("1e1"), "10.0");
    assert_eq!(crate::decimal::canonical("-0.0"), "0.0");
    assert_eq!(
        crate::decimal::canonical("1e999999999999999999999999999999999999999"),
        crate::decimal::canonical("10e999999999999999999999999999999999999998")
    );
    rows.push(json!({"name":"canonical_decimal","hash":hex(&hash_value(&canonical_decimal))}));
    rows.extend(crate::snapshot::corpus::rows());
    rows.extend(crate::compiler_corpus::rows());
    Value::Array(rows)
}
