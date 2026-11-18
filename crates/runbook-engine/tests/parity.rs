#[path = "support/corpus.rs"]
mod corpus;
mod support;
use serde_json::Value;
#[test]
fn native_wasm_execution_traces_match_from_fresh_build() {
    use std::{path::PathBuf, process::Command};
    struct FreshTarget(PathBuf);
    impl Drop for FreshTarget {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove test-owned Wasm target");
        }
    }
    let target = (0..1000)
        .find_map(|attempt| {
            let path = std::env::temp_dir().join(format!(
                "runbook-engine-parity-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => Some(FreshTarget(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => panic!("create fresh engine Wasm target: {error}"),
            }
        })
        .expect("allocate fresh engine Wasm target");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new(env!("CARGO"))
        .current_dir(&manifest)
        .args([
            "build",
            "--locked",
            "--package",
            "runbook-engine",
            "--example",
            "trace_parity",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target.0)
        .output()
        .expect("build fresh engine Wasm example");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let output = Command::new("node")
        .arg(manifest.join("../runbook-expr/tests/wasm-runner.mjs"))
        .arg(
            target
                .0
                .join("wasm32-unknown-unknown/debug/examples/trace_parity.wasm"),
        )
        .output()
        .expect("execute engine Wasm example");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wasm: Value = serde_json::from_slice(&output.stdout).expect("semantic corpus JSON");
    let native = corpus::rows();
    assert_eq!(native.as_array().unwrap().len(), 41);
    assert_eq!(native, wasm);
}
