//! CLI3 `runbook diff`: a structured diff of two runbook specs.
//!
//! Both sides are parsed into the canonical spec (the source as JSON:
//! formatting, comments and key order do not count). Steps are matched by
//! their `id`, which is unique across a runbook. Each step is compared
//! without its nested steps: a container's child list becomes the list of
//! child IDs, so a change inside a child is reported once, on the child,
//! and the parent shows only membership and order. Everything outside the
//! steps is compared the same way, with `spec.steps` and `spec.finally` as
//! ID lists. A change is `{path, op, old?, new?}` with a JSON Pointer path
//! relative to the step (or to the document); arrays of different lengths
//! change as a whole.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Keys of a step that hold nested step lists (directly, or under
/// `foreach`/`loop`/`switch`), as in `runbook_spec::normalize`.
fn child_lists(node: &Value) -> Vec<(Vec<String>, &Vec<Value>)> {
    let mut out = vec![];
    let key = |k: &str| vec![k.to_string()];
    for k in ["steps", "parallel", "catch", "finally"] {
        if let Some(list) = node.get(k).and_then(Value::as_array) {
            out.push((key(k), list));
        }
    }
    for k in ["foreach", "loop"] {
        if let Some(list) = node
            .get(k)
            .and_then(|v| v.get("steps"))
            .and_then(Value::as_array)
        {
            out.push((vec![k.into(), "steps".into()], list));
        }
    }
    if let Some(switch) = node.get("switch") {
        for (j, case) in switch
            .get("cases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let Some(list) = case.get("steps").and_then(Value::as_array) {
                out.push((
                    vec![
                        "switch".into(),
                        "cases".into(),
                        j.to_string(),
                        "steps".into(),
                    ],
                    list,
                ));
            }
        }
        if let Some(list) = switch
            .get("default")
            .and_then(|v| v.get("steps"))
            .and_then(Value::as_array)
        {
            out.push((
                vec!["switch".into(), "default".into(), "steps".into()],
                list,
            ));
        }
    }
    out
}

fn ids(list: &[Value]) -> Value {
    Value::Array(
        list.iter()
            .map(|n| n.get("id").cloned().unwrap_or_else(|| n.clone()))
            .collect(),
    )
}

fn set_at(value: &mut Value, path: &[String], new: Value) {
    let mut at = value;
    for (i, key) in path.iter().enumerate() {
        let last = i + 1 == path.len();
        let next = match at {
            Value::Object(map) => map.get_mut(key),
            Value::Array(list) => key.parse::<usize>().ok().and_then(|i| list.get_mut(i)),
            _ => None,
        };
        match next {
            Some(slot) if last => {
                *slot = new;
                return;
            }
            Some(slot) => at = slot,
            None => return,
        }
    }
}

/// A step without its nested steps (each child list as child IDs).
fn shallow(node: &Value) -> Value {
    let mut copy = node.clone();
    for (path, list) in child_lists(node) {
        set_at(&mut copy, &path, ids(list));
    }
    copy
}

pub struct Step {
    pub path: String,
    pub title: Value,
    pub shallow: Value,
}

/// Every step by ID, depth first, with its location.
fn collect(list: &[Value], path: &str, out: &mut BTreeMap<String, Step>) {
    for (i, node) in list.iter().enumerate() {
        let here = format!("{path}/{i}");
        if let Some(id) = node.get("id").and_then(Value::as_str) {
            out.insert(
                id.to_owned(),
                Step {
                    path: here.clone(),
                    title: node.get("title").cloned().unwrap_or(Value::Null),
                    shallow: shallow(node),
                },
            );
        }
        for (sub, children) in child_lists(node) {
            collect(children, &format!("{here}/{}", sub.join("/")), out);
        }
    }
}

/// The document's steps, and the document with its step lists as IDs.
pub fn split(doc: &Value) -> (BTreeMap<String, Step>, Value) {
    let mut steps = BTreeMap::new();
    let mut rest = doc.clone();
    for key in ["steps", "finally"] {
        if let Some(list) = doc["spec"].get(key).and_then(Value::as_array) {
            collect(list, &format!("/spec/{key}"), &mut steps);
            rest["spec"][key] = ids(list);
        }
    }
    (steps, rest)
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Field-level changes from `a` to `b` under `path`.
pub fn changes(path: &str, a: Option<&Value>, b: Option<&Value>, out: &mut Vec<Value>) {
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                changes(&format!("{path}/{}", escape(k)), x.get(k), y.get(k), out);
            }
        }
        (Some(Value::Array(x)), Some(Value::Array(y))) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                changes(&format!("{path}/{i}"), Some(p), Some(q), out);
            }
        }
        (x, y) if x == y => {}
        (None, Some(y)) => out.push(json!({"path": path, "op": "added", "new": y})),
        (Some(x), None) => out.push(json!({"path": path, "op": "removed", "old": x})),
        (Some(x), Some(y)) => out.push(json!({"path": path, "op": "changed", "old": x, "new": y})),
        (None, None) => {}
    }
}

/// The structured diff of two canonical documents: `{identical, spec,
/// steps: {added, removed, changed}}`.
pub fn structured(old: &Value, new: &Value) -> Value {
    let (old_steps, old_rest) = split(old);
    let (new_steps, new_rest) = split(new);
    let mut spec = vec![];
    changes("", Some(&old_rest), Some(&new_rest), &mut spec);
    let entry = |id: &str, s: &Step| json!({"id": id, "title": s.title, "path": s.path});
    let added: Vec<Value> = new_steps
        .iter()
        .filter(|(id, _)| !old_steps.contains_key(*id))
        .map(|(id, s)| entry(id, s))
        .collect();
    let removed: Vec<Value> = old_steps
        .iter()
        .filter(|(id, _)| !new_steps.contains_key(*id))
        .map(|(id, s)| entry(id, s))
        .collect();
    let mut changed = vec![];
    for (id, n) in &new_steps {
        let Some(o) = old_steps.get(id) else { continue };
        let mut fields = vec![];
        changes("", Some(&o.shallow), Some(&n.shallow), &mut fields);
        if !fields.is_empty() {
            let mut e = entry(id, n);
            e["changes"] = Value::Array(fields);
            changed.push(e);
        }
    }
    let identical = spec.is_empty() && added.is_empty() && removed.is_empty() && changed.is_empty();
    json!({
        "identical": identical,
        "spec": spec,
        "steps": {"added": added, "removed": removed, "changed": changed},
    })
}

/// A unified diff of two sources, bounded like the server's version diff:
/// past a quadratic budget it is one replacement hunk (still exact).
pub fn unified(a: &str, b: &str, from: &str, to: &str) -> String {
    let lines_a = a.lines().count();
    let lines_b = b.lines().count();
    if a == b {
        return String::new();
    }
    if lines_a.saturating_mul(lines_b) <= 1_000_000 {
        return similar::TextDiff::from_lines(a, b)
            .unified_diff()
            .header(from, to)
            .to_string();
    }
    let mut out = format!(
        "--- {from}\n+++ {to}\n@@ -{},{} +{},{} @@\n",
        usize::from(lines_a > 0),
        lines_a,
        usize::from(lines_b > 0),
        lines_b
    );
    for (prefix, text) in [('-', a), ('+', b)] {
        for line in text.split_inclusive('\n') {
            out.push(prefix);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

/// A value for one line of human output.
fn short(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() > 80 {
        format!("{}…", text.chars().take(79).collect::<String>())
    } else {
        text
    }
}

fn change_line(c: &Value, indent: &str) -> String {
    let path = c["path"].as_str().unwrap_or("");
    match c["op"].as_str() {
        Some("added") => format!("{indent}+ {path}: {}", short(&c["new"])),
        Some("removed") => format!("{indent}- {path}: {}", short(&c["old"])),
        _ => format!(
            "{indent}~ {path}: {} → {}",
            short(&c["old"]),
            short(&c["new"])
        ),
    }
}

/// The structured diff for people.
pub fn render(diff: &Value, from: &str, to: &str) -> String {
    let mut out = format!("--- {from}\n+++ {to}\n");
    if diff["identical"] == json!(true) {
        out.push_str("no differences\n");
        return out;
    }
    let spec = diff["spec"].as_array().cloned().unwrap_or_default();
    if !spec.is_empty() {
        out.push_str("spec:\n");
        for c in &spec {
            out.push_str(&change_line(c, "  "));
            out.push('\n');
        }
    }
    let title = |s: &Value| s["title"].as_str().unwrap_or("").to_owned();
    let steps = &diff["steps"];
    let any = ["added", "removed", "changed"]
        .iter()
        .any(|k| steps[*k].as_array().is_some_and(|a| !a.is_empty()));
    if any {
        out.push_str("steps:\n");
    }
    for (key, mark) in [("added", '+'), ("removed", '-'), ("changed", '~')] {
        for s in steps[key].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "  {mark} {}  {}\n",
                s["id"].as_str().unwrap_or(""),
                title(s)
            ));
            for c in s["changes"].as_array().into_iter().flatten() {
                out.push_str(&change_line(c, "      "));
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_steps_are_compared_once_on_the_child() {
        let old = json!({"spec": {"steps": [
            {"id": "loop", "loop": {"until": "x", "steps": [{"id": "a", "title": "A"}]}},
            {"id": "sw", "switch": {"cases": [{"when": "y", "steps": [{"id": "b"}]}],
                                    "default": {"steps": [{"id": "c"}]}}},
        ]}});
        let mut new = old.clone();
        new["spec"]["steps"][0]["loop"]["steps"][0]["title"] = json!("A2");
        new["spec"]["steps"][1]["switch"]["default"]["steps"] = json!([{"id": "d"}]);
        let d = structured(&old, &new);
        assert_eq!(d["spec"], json!([]));
        assert_eq!(
            d["steps"]["added"],
            json!([{"id": "d", "title": null, "path": "/spec/steps/1/switch/default/steps/0"}])
        );
        assert_eq!(d["steps"]["removed"][0]["id"], "c");
        let changed = d["steps"]["changed"].as_array().unwrap();
        assert_eq!(changed.len(), 2, "{d:#}");
        assert_eq!(changed[0]["id"], "a");
        assert_eq!(
            changed[0]["changes"],
            json!([{"path": "/title", "op": "changed", "old": "A", "new": "A2"}])
        );
        assert_eq!(changed[1]["id"], "sw");
        assert_eq!(
            changed[1]["changes"],
            json!([{"path": "/switch/default/steps/0", "op": "changed", "old": "c", "new": "d"}])
        );
    }

    #[test]
    fn keys_are_escaped_as_json_pointers() {
        let mut out = vec![];
        changes(
            "",
            Some(&json!({"a/b": 1, "c~d": 1})),
            Some(&json!({"a/b": 2})),
            &mut out,
        );
        assert_eq!(out[0]["path"], "/a~1b");
        assert_eq!(out[1]["path"], "/c~0d");
        assert_eq!(out[1]["op"], "removed");
    }
}
