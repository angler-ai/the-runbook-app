use runbook_spec::{estimate_step_runs, ir::NormalizedRunbook, normalize, parse};
use serde_json::{Value, json};

fn runbook(body: Value) -> NormalizedRunbook {
    normalize(
        &parse(
            &json!({
                "apiVersion":"therunbook.app/v1", "kind":"Runbook",
                "metadata":{"id":"budget-test", "title":"Budget"}, "spec":body,
            })
            .to_string(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn task(id: &str) -> Value {
    json!({"id":id,"title":id})
}
/// The publish caps (PRD §17, R52) keep source iteration bounds small, so
/// the overflow paths are reached on mutated intermediate trees: every loop
/// and foreach bound is raised to u32::MAX after normalization.
fn saturated(body: Value) -> NormalizedRunbook {
    use runbook_spec::ir::{Node, NodeKind};
    fn raise(nodes: &mut [Node]) {
        for node in nodes {
            match &mut node.kind {
                NodeKind::Loop(l) => {
                    l.max_iterations = u32::MAX;
                    raise(&mut l.children);
                }
                NodeKind::Foreach(f) => {
                    f.max_items = u32::MAX;
                    raise(&mut f.children);
                }
                NodeKind::Group(g) => {
                    raise(&mut g.children);
                    raise(&mut g.catch);
                    raise(&mut g.finally);
                }
                NodeKind::Parallel { children, .. } => raise(children),
                _ => {}
            }
        }
    }
    let mut book = runbook(body);
    raise(&mut book.steps);
    raise(&mut book.finally);
    book
}

#[test]
fn flagship_step_budget_is_twenty() {
    let book = normalize(&parse(include_str!("fixtures/post-deploy.yaml")).unwrap()).unwrap();
    assert_eq!(estimate_step_runs(&book).unwrap(), 20);
}

#[test]
fn leaf_budget_counts_materialized_work() {
    let cases = [
        (
            json!({"id":"work","title":"Work","checklist":["one","two","three"]}),
            1,
        ),
        (
            json!({"id":"vote","approval":{"prompt":"Choose","options":["yes","no"]}}),
            1,
        ),
        (json!({"id":"delay","wait":{"for":"1s"}}), 1),
        (json!({"id":"assign","set":{"vars":{"value":"done"}}}), 0),
        (json!({"id":"finish","end":{"status":"succeeded"}}), 0),
    ];
    for (node, expected) in cases {
        assert_eq!(
            estimate_step_runs(&runbook(json!({"steps":[node]}))).unwrap(),
            expected
        );
    }
    assert_eq!(
        estimate_step_runs(&runbook(json!({"steps":[]}))).unwrap(),
        0
    );
}

#[test]
fn sequences_parallel_and_cleanup_sum() {
    let book = runbook(json!({"steps":[
        task("first"),
        {"id":"parallel","parallel":[task("p_one"),task("p_two")],"fail_fast":true},
        {"id":"group","steps":[task("g_one"),task("g_two")],
            "catch":[task("recover")],"finally":[task("cleanup_one"),task("cleanup_two")]}
    ],"finally":[task("notify")]}));
    // 1 ordinary +2 parallel +2 group +1 catch +2 group cleanup +1 top cleanup.
    assert_eq!(estimate_step_runs(&book).unwrap(), 9);
}

#[test]
fn switch_budget_uses_largest_branch() {
    let book = runbook(json!({"steps":[
        {"id":"choose","switch":{"cases":[
            {"when":"${{ true }}","steps":[task("a")]},
            {"when":"${{ true }}","steps":[task("b"),task("c"),task("d")]}
        ],"default":{"steps":[task("e"),task("f")]}}},task("after")
    ]}));
    assert_eq!(estimate_step_runs(&book).unwrap(), 4);
    let mut default_wins = book;
    let runbook_spec::ir::NodeKind::Switch(branch) = &mut default_wins.steps[0].kind else {
        panic!()
    };
    branch.cases.clear();
    assert_eq!(estimate_step_runs(&default_wins).unwrap(), 3);
}

#[test]
fn bounded_iterations_multiply() {
    let book = runbook(json!({"steps":[{"id":"repeat","loop":{
        "until":"${{ true }}","max_iterations":3,"steps":[{"id":"fanout","foreach":{
            "items":"${{ inputs.items }}","as":"item","max_items":4,"concurrency":1,
            "steps":[task("a"),task("b")]
        }}]
    }}],"inputs":{"items":{"type":"list","items":{"type":"string"},"default":["one"]}}}));
    assert_eq!(estimate_step_runs(&book).unwrap(), 24);
}

#[test]
fn retries_guards_and_fail_fast_do_not_change_budget() {
    let book = runbook(
        json!({"defaults":{"retry":{"max_attempts":5,"backoff":"1s"}},"steps":[
            {"id":"retry","title":"Retry","retry":{"max_attempts":5}},
            {"id":"guarded","title":"Guarded","if":"${{ false }}"},
            {"id":"parallel","parallel":[task("a"),task("b")],"fail_fast":true}
        ]}),
    );
    assert_eq!(estimate_step_runs(&book).unwrap(), 4);
}

#[test]
fn step_budget_overflow_is_diagnostic() {
    let mut node = task("work");
    for id in ["inner", "middle", "outer"] {
        node = json!({"id":id,"loop":{"until":"${{ false }}","max_iterations":2,"steps":[node]}});
    }
    let error = estimate_step_runs(&saturated(json!({"steps":[node]}))).unwrap_err();
    assert_eq!(error.code, "spec.step_budget_overflow");
    assert_eq!(error.path, "/spec/steps/0");
    let pair = |prefix: &str| {
        json!({"id":format!("{prefix}_outer"),"loop":{
            "until":"${{ false }}","max_iterations":2,"steps":[{
                "id":format!("{prefix}_inner"),"loop":{
                    "until":"${{ false }}","max_iterations":2,
                    "steps":[task(&format!("{prefix}_work"))]
                }
            }]
        }})
    };
    // Each branch fits u64 individually; summing them must still be checked.
    let error = estimate_step_runs(&saturated(json!({"steps":[pair("a"),pair("b")]}))).unwrap_err();
    assert_eq!(error.code, "spec.step_budget_overflow");
    assert_eq!(error.path, "/spec/steps/1");
    let error = estimate_step_runs(&saturated(
        json!({"steps":[pair("a")],"finally":[pair("b")]}),
    ))
    .unwrap_err();
    assert_eq!(error.code, "spec.step_budget_overflow");
    assert_eq!(error.path, "/spec/finally/0");
}

#[test]
fn mutated_intermediate_tree_keeps_node_and_depth_limits() {
    use runbook_spec::ir::{Group, NodeKind};
    let mut book = runbook(json!({"steps":[task("leaf")]}));
    let leaf = book.steps[0].clone();
    book.steps = vec![leaf.clone(); 4096];
    assert_eq!(estimate_step_runs(&book).unwrap(), 4096);
    book.steps.push(leaf.clone());
    let error = estimate_step_runs(&book).unwrap_err();
    assert_eq!(error.code, "spec.node_limit");
    let wrap = |child| {
        let mut node = leaf.clone();
        node.kind = NodeKind::Group(Group {
            children: vec![child],
            catch: vec![],
            finally: vec![],
            catch_on: vec![],
        });
        node
    };
    let mut node = leaf.clone();
    for _ in 0..32 {
        node = wrap(node);
    }
    book.steps = vec![node];
    assert_eq!(estimate_step_runs(&book).unwrap(), 1);
    let node = book.steps.pop().unwrap();
    book.steps.push(wrap(node));
    assert_eq!(
        estimate_step_runs(&book).unwrap_err().code,
        "spec.control_depth_limit"
    );
}

#[test]
fn empty_switch_cases_cannot_bypass_structural_work_bound() {
    use runbook_spec::ir::{Case, NodeKind};
    let mut book = runbook(json!({"steps":[{"id":"choose","switch":{"cases":[]}}]}));
    let NodeKind::Switch(branch) = &mut book.steps[0].kind else {
        panic!()
    };
    branch.cases.resize_with(262_144, || Case {
        when: String::new(),
        source_path: String::new(),
        children: vec![],
    });
    assert_eq!(estimate_step_runs(&book).unwrap(), 0);
    let NodeKind::Switch(branch) = &mut book.steps[0].kind else {
        panic!()
    };
    branch.cases.push(Case {
        when: String::new(),
        source_path: String::new(),
        children: vec![],
    });
    let error = estimate_step_runs(&book).unwrap_err();
    assert_eq!(error.code, "spec.source_value_limit");
    assert_eq!(error.path, "/spec/steps/0");
    let NodeKind::Switch(branch) = &mut book.steps[0].kind else {
        panic!()
    };
    branch.cases.truncate(131_072);
    let mut second = book.steps[0].clone();
    second.source_path = "/spec/steps/1".into();
    let NodeKind::Switch(branch) = &mut second.kind else {
        panic!()
    };
    branch.cases.push(Case {
        when: String::new(),
        source_path: String::new(),
        children: vec![],
    });
    book.steps.push(second);
    // Neither switch exceeds the allowance alone; the total still must fit.
    let error = estimate_step_runs(&book).unwrap_err();
    assert_eq!(error.code, "spec.source_value_limit");
    assert_eq!(error.path, "/spec/steps/1");
}

#[test]
fn overflow_does_not_copy_an_unbounded_mutated_source_path() {
    let mut node = task("work");
    for id in ["inner", "middle", "outer"] {
        node = json!({"id":id,"loop":{"until":"${{ false }}","max_iterations":2,"steps":[node]}});
    }
    let mut book = saturated(json!({"steps":[node]}));
    book.steps[0].source_path = "x".repeat(1_048_577);
    let error = estimate_step_runs(&book).unwrap_err();
    assert_eq!(error.code, "spec.step_budget_overflow");
    assert!(error.path.is_empty());
}
