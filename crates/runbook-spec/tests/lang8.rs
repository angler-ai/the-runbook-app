//! LANG8: multiple skills per step (`skills: [a, b]`, at most 8, no duplicates).
use runbook_spec::ir::NodeKind;
use runbook_spec::{CompileContext, compile, normalize, parse, restore};
use serde_json::{Value, json};

fn errors(source: &str) -> Vec<(String, String)> {
    match parse(source) {
        Err(errors) => errors,
        Ok(spec) => normalize(&spec).expect_err("must not normalize"),
    }
    .into_iter()
    .map(|d| (d.code, d.path))
    .collect()
}

fn document(body: Value) -> String {
    json!({
        "apiVersion": "therunbook.app/v1",
        "kind": "Runbook",
        "metadata": {"id": "test", "title": "Test"},
        "spec": body
    })
    .to_string()
}

#[test]
fn valid_multiple_skills_compile_and_roundtrip() {
    let source = r#"
metadata: {id: multi-skill}
spec:
  skills:
    deploy: ./skills/deploy
    rollback: ./skills/rollback
  steps:
    - id: step_one
      skills: [deploy, rollback]
"#;
    let spec = parse(source).unwrap();
    let mut ctx = CompileContext::default();
    ctx.skills.insert("./skills/deploy".into(), [1; 32]);
    ctx.skills.insert("./skills/rollback".into(), [2; 32]);

    let compiled = compile(&spec, &ctx).unwrap();
    let ir = compiled.ir();
    let NodeKind::Task(task) = &ir.steps[0].kind else {
        panic!("expected task");
    };
    assert_eq!(task.skill.as_deref(), Some("deploy"));
    assert_eq!(task.skills, vec!["deploy", "rollback"]);

    // Both skills are bound
    assert_eq!(compiled.skill_hashes().len(), 2);
    assert_eq!(compiled.skill_hashes()["deploy"], [1; 32]);
    assert_eq!(compiled.skill_hashes()["rollback"], [2; 32]);

    // Snapshot restore preserves skills
    let restored = restore(compiled.snapshot_bytes()).unwrap();
    assert_eq!(restored.hash(), compiled.hash());
    let NodeKind::Task(restored_task) = &restored.ir().steps[0].kind else {
        panic!("expected task");
    };
    assert_eq!(restored_task.skill.as_deref(), Some("deploy"));
    assert_eq!(restored_task.skills, vec!["deploy", "rollback"]);
}

#[test]
fn single_skill_shorthand_keeps_pre_lang8_projection_and_hash() {
    let source_shorthand = r#"
metadata: {id: single-skill}
spec:
  skills:
    deploy: ./skills/deploy
  steps:
    - id: step_one
      skill: deploy
"#;
    let spec = parse(source_shorthand).unwrap();
    let mut ctx = CompileContext::default();
    ctx.skills.insert("./skills/deploy".into(), [1; 32]);
    let compiled = compile(&spec, &ctx).unwrap();
    let NodeKind::Task(task) = &compiled.ir().steps[0].kind else {
        panic!("expected task");
    };
    assert_eq!(task.skill.as_deref(), Some("deploy"));
    assert_eq!(task.skills, vec!["deploy"]);
}

#[test]
fn both_skill_and_skills_fields_are_refused() {
    let doc = document(json!({
        "steps": [{
            "id": "t",
            "skill": "deploy",
            "skills": ["rollback"]
        }]
    }));
    let errs = errors(&doc);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "spec.both_skill_and_skills");
    assert_eq!(errs[0].1, "/spec/steps/0/skills");
}

#[test]
fn too_many_skills_over_eight_are_refused() {
    let doc = document(json!({
        "steps": [{
            "id": "t",
            "skills": ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9"]
        }]
    }));
    let errs = errors(&doc);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "spec.too_many_skills");
    assert_eq!(errs[0].1, "/spec/steps/0/skills");
}

#[test]
fn duplicate_skills_are_refused() {
    let doc = document(json!({
        "steps": [{
            "id": "t",
            "skills": ["deploy", "rollback", "deploy"]
        }]
    }));
    let errs = errors(&doc);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "spec.duplicate_skill");
    assert_eq!(errs[0].1, "/spec/steps/0/skills/2");
}

#[test]
fn unresolved_alias_in_multi_skills_is_refused() {
    let source = r#"
metadata: {id: unresolved-multi}
spec:
  skills:
    deploy: ./skills/deploy
  steps:
    - id: step_one
      skills: [deploy, missing]
"#;
    let spec = parse(source).unwrap();
    let mut ctx = CompileContext::default();
    ctx.skills.insert("./skills/deploy".into(), [1; 32]);
    let errs = compile(&spec, &ctx).unwrap_err();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].code, "spec.unresolved_skill");
    assert_eq!(errs[0].path, "/spec/steps/0/skills/1");
}

#[test]
fn legacy_stored_ir_without_skills_deserializes_with_skills_populated() {
    let source = r#"
metadata: {id: legacy-test}
spec:
  skills:
    deploy: ./skills/deploy
  steps:
    - id: step_one
      skill: deploy
"#;
    let spec = parse(source).unwrap();
    let mut ctx = CompileContext::default();
    ctx.skills.insert("./skills/deploy".into(), [1; 32]);
    let compiled = compile(&spec, &ctx).unwrap();
    let NodeKind::Task(original_task) = &compiled.ir().steps[0].kind else {
        panic!("expected task");
    };

    let mut task_val = serde_json::to_value(original_task).unwrap();
    // Simulate legacy Task JSON serialized before LANG8: remove `skills`
    task_val.as_object_mut().unwrap().remove("skills");
    assert!(task_val.get("skill").is_some());
    assert!(task_val.get("skills").is_none());

    let task: runbook_spec::ir::Task = serde_json::from_value(task_val).unwrap();
    assert_eq!(task.skill.as_deref(), Some("deploy"));
    assert_eq!(task.skills, vec!["deploy"]);
}

#[test]
fn single_skill_stored_ir_and_task_contain_no_skills_key() {
    let source = r#"
metadata: {id: single-skill-wire}
spec:
  steps:
    - id: step_one
      skill: skill://test
"#;
    let spec = parse(source).unwrap();
    let mut ctx = CompileContext::default();
    ctx.skills.insert("skill://test".into(), [1; 32]);
    let compiled = compile(&spec, &ctx).unwrap();

    // 1. Stored IR Task does not contain `"skills"` key
    let ir_json = serde_json::to_value(compiled.ir()).unwrap();
    let step_task = &ir_json["steps"][0]["kind"]["Task"];
    assert!(step_task.get("skill").is_some(), "expected 'skill' field");
    assert_eq!(step_task["skill"], "skill://test");
    assert!(
        step_task.get("skills").is_none(),
        "single-skill stored IR task must not contain 'skills' key"
    );

    // 2. Wire snapshot restores and re-serializes byte-identical
    let snapshot_bytes = compiled.snapshot_bytes();
    let restored = restore(snapshot_bytes).unwrap();
    assert_eq!(restored.snapshot_bytes(), snapshot_bytes);
    assert_eq!(restored.hash(), compiled.hash());

    let NodeKind::Task(restored_task) = &restored.ir().steps[0].kind else {
        panic!("expected task");
    };
    assert_eq!(restored_task.skill.as_deref(), Some("skill://test"));
    assert_eq!(restored_task.skills, vec!["skill://test"]);

    // 3. Serializing restored task omits "skills" because len < 2
    let re_task_json = serde_json::to_value(restored_task).unwrap();
    assert!(re_task_json.get("skills").is_none());
    assert_eq!(re_task_json.get("skill").unwrap(), "skill://test");
}
