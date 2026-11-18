use runbook_expr::{SchemaDef, TypeEnv, check_expression};
use std::collections::BTreeMap;

fn empty() -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap()
}

#[test]
fn syntax_errors_are_not_successful_parse_results() {
    for source in ["", "true &&", "(true", "'unterminated", "true false", "😀"] {
        let error = check_expression(source, &empty()).unwrap_err();
        assert_eq!(error.code, "expression.syntax_error", "{source}: {error:?}");
        assert!(error.span.start <= error.span.end && error.span.end <= source.len());
        assert!(
            source.is_char_boundary(error.span.start) && source.is_char_boundary(error.span.end)
        );
    }
}

fn balanced(n: usize) -> String {
    if n == 1 {
        return "true".into();
    }
    format!("({} && {})", balanced(n / 2), balanced(n - n / 2))
}

#[test]
fn final_tree_limits_apply_before_the_checker() {
    for source in [
        format!("{}true", "!".repeat(33)),
        format!("{} && {}", balanced(64), balanced(65)),
    ] {
        assert_eq!(
            check_expression(&source, &empty()).unwrap_err().code,
            "expression.compile_limit"
        );
    }
    assert!(check_expression(&format!("{}true", "!".repeat(32)), &empty()).is_ok());
    assert!(check_expression(&format!("!{}", balanced(128)), &empty()).is_ok());
}

#[test]
fn source_and_identifier_limits_and_diagnostics_are_bounded() {
    for source in [" ".repeat(4097), "x".repeat(129), "!".repeat(513)] {
        let error = check_expression(&source, &empty()).unwrap_err();
        assert_eq!(error.code, "expression.compile_limit");
        assert!(error.message.len() <= 4096);
        assert!(error.span.end <= source.len());
    }
}
