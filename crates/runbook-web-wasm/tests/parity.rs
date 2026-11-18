//! Native/Wasm parity for the browser compiler (B3, A7). A fresh Wasm build of
//! this crate runs in Node over the compiler corpus (`conformance/spec/**`, the
//! flagship, every flagship prefix and the every-kind fixture), with and without
//! the flagship's directory, for every operation. Each Wasm result must be
//! byte-identical to the native result, and every `validate` result must equal
//! the server's `/v1/validate` code path. Skipped, with a notice, without Node.
mod support;
use runbook_spec::authoring::{CompileDirectory, MAX_SOURCE_BYTES, compile_context};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use support::*;

// Each run owns an empty target directory: a stale Wasm artifact can never
// satisfy this test, and nested Cargo never waits on the outer target lock.
struct FreshDir(PathBuf);
impl FreshDir {
    fn new() -> Self {
        for attempt in 0..1000 {
            let path = std::env::temp_dir().join(format!(
                "runbook-web-wasm-parity-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create fresh Wasm target: {e}"),
            }
        }
        panic!("could not allocate a fresh Wasm target directory");
    }
}
impl Drop for FreshDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Every source in the corpus, by name.
fn sources() -> Vec<(String, String)> {
    let root = repo();
    let mut paths = vec![];
    files(&root.join("conformance/spec"), &mut paths);
    paths.extend([
        root.join("crates/runbook-spec/tests/fixtures/post-deploy.yaml"),
        root.join("crates/runbook-engine/tests/fixtures/golden-v2/runbook.yaml"),
        root.join("crates/runbook-web-wasm/tests/fixtures/every-kind.yaml"),
        root.join("crates/runbook-web-wasm/tests/fixtures/lean.yaml"),
    ]);
    let mut out: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            (
                p.strip_prefix(&root).unwrap().display().to_string(),
                std::fs::read_to_string(p).unwrap(),
            )
        })
        .collect();
    // Every line prefix of the flagship: truncated sources produce parse,
    // shape and semantic diagnostics with spans all over the document.
    let flagship = flagship();
    let lines: Vec<&str> = flagship.split_inclusive('\n').collect();
    for n in 1..lines.len() {
        out.push((format!("flagship[..{n}]"), lines[..n].concat()));
    }
    out
}

/// (name, op, input) for the whole corpus plus the ABI error cases.
fn requests() -> Vec<(String, String, String)> {
    let mut out = vec![];
    let directory = flagship_directory();
    for (name, source) in sources() {
        for (context_name, context) in [("bare", None), ("directory", Some(&directory))] {
            let input = match context {
                Some(c) => json!({"source": source, "context": c}),
                None => json!({"source": source}),
            };
            for op in ["validate", "outline", "describe"] {
                out.push((
                    format!("{name} {context_name} {op}"),
                    op.to_owned(),
                    input.to_string(),
                ));
            }
        }
    }
    let flagship = flagship();
    for (name, op, input) in [
        (
            "unknown op",
            "compile",
            json!({"source": flagship}).to_string(),
        ),
        ("invalid json", "validate", "{".to_owned()),
        ("missing source", "outline", "{}".to_owned()),
        (
            "oversize source",
            "validate",
            json!({"source": " ".repeat(MAX_SOURCE_BYTES + 1)}).to_string(),
        ),
        (
            "invalid variables",
            "validate",
            json!({"source": flagship, "context": {"variables": {"x": {"type": "nope"}}}})
                .to_string(),
        ),
    ] {
        out.push((name.to_owned(), op.to_owned(), input));
    }
    out
}

fn native(op: &str, input: &str) -> String {
    String::from_utf8(runbook_web_wasm::call(op.as_bytes(), input.as_bytes())).unwrap()
}

#[test]
fn native_matches_the_server_validate_path() {
    let mut checked = 0;
    for (name, op, input) in requests() {
        let Ok(parsed) = serde_json::from_str::<Value>(&input) else {
            continue;
        };
        let Some(source) = parsed["source"].as_str() else {
            continue;
        };
        if op != "validate" || source.len() > MAX_SOURCE_BYTES || name == "invalid variables" {
            continue;
        }
        let directory: CompileDirectory =
            serde_json::from_value(parsed.get("context").cloned().unwrap_or(json!({}))).unwrap();
        let mut context = compile_context(&directory, source).unwrap();
        // The browser has no catalog: it assumes every call target resolves
        // (UI-2). With the same assumption the server path must agree.
        runbook_web_wasm::assume_calls_resolve(source, &mut context);
        let server = json!({"ok": runbook_api::authoring::validate(source, &context)}).to_string();
        assert_eq!(native(&op, &input), server, "{name}");
        checked += 1;
    }
    assert!(checked > 200, "only {checked} validate rows");
}

#[test]
fn native_and_fresh_wasm_agree_byte_for_byte() {
    if Command::new("node").arg("--version").output().is_err() {
        println!("SKIPPED native/Wasm parity: `node` is not installed");
        return;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = FreshDir::new();
    let build = Command::new(env!("CARGO"))
        .current_dir(&manifest)
        .args([
            "build",
            "--locked",
            "--package",
            "runbook-web-wasm",
            "--lib",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target.0)
        .output()
        .expect("launch fresh Wasm build");
    assert!(
        build.status.success(),
        "fresh Wasm build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let requests = requests();
    let request_file = target.0.join("requests.json");
    std::fs::write(
        &request_file,
        serde_json::to_vec(
            &requests
                .iter()
                .map(|(_, op, input)| json!({"op": op, "input": input}))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
    .unwrap();
    let output = Command::new("node")
        .arg(manifest.join("tests/wasm-runner.mjs"))
        .arg(
            target
                .0
                .join("wasm32-unknown-unknown/debug/runbook_web_wasm.wasm"),
        )
        .arg(&request_file)
        .output()
        .expect("launch Node Wasm runner");
    assert!(
        output.status.success(),
        "Wasm runner failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wasm: Vec<String> = serde_json::from_slice(&output.stdout).expect("runner JSON");
    assert_eq!(wasm.len(), requests.len());
    for ((name, op, input), wasm) in requests.iter().zip(&wasm) {
        assert_eq!(&native(op, input), wasm, "{name}");
    }
    println!(
        "{} browser-compiler requests ({} sources) agree byte for byte across native and fresh Wasm",
        requests.len(),
        sources().len()
    );
}
