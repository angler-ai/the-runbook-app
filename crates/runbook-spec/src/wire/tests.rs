use super::*;
use serde_json::json;

fn limits() -> Limits {
    Limits {
        bytes: 1_000_000,
        values: 10_000,
        depth: 256,
    }
}

#[test]
fn wire_roundtrips_exact_values() {
    let mut value =
        json!({"nested":[null,true,false,"é",{"$serde_json::private::Number":"1"}],"empty":{}});
    for (i, text) in [
        "9223372036854775808",
        "12345678901234567890123456789",
        "1.0",
        "1e-400",
        "-0.0",
        "1e+400",
    ]
    .iter()
    .enumerate()
    {
        value[format!("n{i}")] = Value::Number(text.parse().unwrap());
    }
    let bytes = encode(&value, limits()).unwrap();
    assert_eq!(decode(&bytes, limits()).unwrap().get(), &value);
    assert_eq!(encode(&value, limits()).unwrap(), bytes);
}

#[test]
fn wire_roundtrips_deep_tree_without_serde_limit_changes() {
    let mut value = json!("leaf");
    for _ in 0..191 {
        value = json!([value]);
    }
    assert!(serde_json::from_slice::<Value>(&serde_json::to_vec(&value).unwrap()).is_err());
    let bytes = encode(&value, limits()).unwrap();
    assert!(serde_json::from_slice::<Value>(&bytes).is_ok());
    assert_eq!(decode(&bytes, limits()).unwrap().get(), &value);
}

#[test]
fn wire_limits_are_checked_before_bulk_allocation() {
    let value = json!({"a":[1,2]}); // four values, deepest value at depth two.
    let bytes = encode(&value, limits()).unwrap();
    let exact = Limits {
        bytes: bytes.len(),
        values: 4,
        depth: 2,
    };
    assert_eq!(encode(&value, exact).unwrap(), bytes);
    assert_eq!(decode(&bytes, exact).unwrap().get(), &value);
    for bad in [
        Limits {
            bytes: bytes.len() - 1,
            ..exact
        },
        Limits { values: 3, ..exact },
        Limits { depth: 1, ..exact },
    ] {
        assert_eq!(encode(&value, bad), Err(Error::Limit));
        assert!(matches!(decode(&bytes, bad), Err(Error::Limit)));
    }
}

#[test]
fn wire_rejects_ambiguous_or_malformed_data() {
    for text in [
        r#"{"version":1,"version":1,"tokens":["Null"]}"#,
        r#"{"version":1,"tokens":["Null"],"unknown":true}"#,
        r#"{"version":2,"tokens":["Null"]}"#,
        r#"{"version":1,"tokens":["Future"]}"#,
        r#"{"version":1,"tokens":[{"Object":2},{"Key":"a"},"Null",{"Key":"a"},"Null"]}"#,
        r#"{"version":1,"tokens":[{"Key":"outside"}]}"#,
        r#"{"version":1,"tokens":[{"Number":"01"}]}"#,
        r#"{"version":1,"tokens":[{"Number":"NaN"}]}"#,
        r#"{"version":1,"tokens":[{"Array":18446744073709551615}]}"#,
        r#"{"version":1,"tokens":[{"Array":2},"Null"]}"#,
        r#"{"version":1,"tokens":["Null","Null"]}"#,
        r#"{"version":1,"tokens":["Null"]} false"#,
    ] {
        assert!(
            decode(text.as_bytes(), limits()).is_err(),
            "accepted {text}"
        );
    }
}

#[test]
fn wire_rejection_cleanup_is_bounded() {
    let mut value = json!("leaf");
    for _ in 0..191 {
        value = json!([value]);
    }
    let bytes = encode(&value, limits()).unwrap();
    let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
    envelope["tokens"]
        .as_array_mut()
        .unwrap()
        .push(json!("Null"));
    assert!(matches!(
        decode(&serde_json::to_vec(&envelope).unwrap(), limits()),
        Err(Error::Invalid)
    ));
}

#[test]
fn wire_token_collection_stops_at_cap() {
    // Count actual visitor requests: two admitted tokens and one ignored probe,
    // never the other 99,997 entries. The excess is not deserialized as Token.
    struct Counting<'a> {
        calls: &'a Cell<usize>,
    }
    impl<'de> SeqAccess<'de> for Counting<'_> {
        type Error = serde::de::value::Error;
        fn next_element_seed<T: DeserializeSeed<'de>>(
            &mut self,
            seed: T,
        ) -> Result<Option<T::Value>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() > 100_000 {
                return Ok(None);
            }
            seed.deserialize(serde::de::value::StrDeserializer::new("Null"))
                .map(Some)
        }
    }
    let calls = Cell::new(0);
    let limited = Cell::new(false);
    assert!(
        TokenSeed {
            cap: 2,
            limited: &limited
        }
        .visit_seq(Counting { calls: &calls })
        .is_err()
    );
    assert_eq!(calls.get(), 3);
    assert!(limited.get());
    assert!(matches!(
        decode(
            b"not even JSON",
            Limits {
                bytes: 0,
                ..limits()
            }
        ),
        Err(Error::Limit)
    ));
    assert!(matches!(
        decode(
            br#"{"version":1,"tokens":["Null","Null","Null"]}"#,
            Limits {
                values: 1,
                ..limits()
            }
        ),
        Err(Error::Limit)
    ));
}

#[test]
fn wire_measured_semantic_fixture_roundtrips() {
    let mut nested = json!({"id":"leaf","title":"Leaf","agent":{"tools":[]}});
    let mut opaque = json!(0);
    for _ in 0..56 {
        opaque = json!([opaque]);
    }
    nested["agent"]["tools"] = opaque;
    for i in 0..32 {
        nested = json!({"id":format!("g{i}"),"steps":[nested]});
    }
    let source = json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":[nested]}});
    let ir = crate::normalize(&crate::parse(&source.to_string()).unwrap()).unwrap();
    let pins = crate::semantic::SelectedPins {
        skills: &Default::default(),
        users: &Default::default(),
        teams: &Default::default(),
        project_variables: &crate::parse_declarations(&json!({})).unwrap(),
    };
    let tree = Tree(crate::semantic::projection(&ir, &pins));
    let depth = Walk::new(tree.get())
        .filter(|(token, _)| !matches!(token, RefToken::Key(_)))
        .map(|(_, depth)| depth)
        .max()
        .unwrap();
    assert_eq!(depth, 191);
    let bytes = encode(tree.get(), limits()).unwrap();
    assert_eq!(decode(&bytes, limits()).unwrap().get(), tree.get());
}

#[test]
fn wire_deep_partial_tree_cleanup_is_iterative() {
    // A completed 20,000-level subtree followed by an invalid extra root tests
    // error cleanup, while a truncated parent tests partial frame cleanup.
    let depth = 20_000;
    let tokens = format!("{}\"Null\",\"Null\"", "{\"Array\":1},".repeat(depth));
    let bytes = format!("{{\"version\":1,\"tokens\":[{tokens}]}}").into_bytes();
    let bounds = Limits {
        bytes: bytes.len(),
        values: depth + 3,
        depth,
    };
    assert!(matches!(decode(&bytes, bounds), Err(Error::Invalid)));
    let truncated = format!("{{\"version\":1,\"tokens\":[{{\"Array\":2}},{tokens}]}}");
    // This version is valid: both children satisfy the outer array.
    let tree = decode(
        truncated.as_bytes(),
        Limits {
            bytes: truncated.len(),
            depth: depth + 1,
            ..bounds
        },
    )
    .unwrap();
    drop(tree);
    let partial = truncated.replace("\"Null\",\"Null\"]}", "\"Null\",{\"Key\":\"invalid\"}]}");
    assert!(matches!(
        decode(
            partial.as_bytes(),
            Limits {
                bytes: partial.len(),
                depth: depth + 1,
                ..bounds
            }
        ),
        Err(Error::Invalid)
    ));
}
