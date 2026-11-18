#[path = "support/parity_corpus.rs"]
mod parity_corpus;
use std::path::PathBuf;
use std::process::Command;

// Each invocation owns an empty target directory: a cached/stale Wasm artifact
// can never satisfy this test, and nested Cargo never shares the outer target lock.
struct FreshTarget(PathBuf);
impl FreshTarget {
    fn new() -> Self {
        for attempt in 0..1000 {
            let path = std::env::temp_dir().join(format!(
                "runbook-expr-parity-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fresh Wasm target: {error}"),
            }
        }
        panic!("could not allocate a fresh Wasm target directory");
    }
}
impl Drop for FreshTarget {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove test-owned Wasm target");
    }
}

#[test]
fn native_wasm_agree_on_complete_corpus() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = FreshTarget::new();
    let build = Command::new(env!("CARGO"))
        .current_dir(&manifest)
        .args([
            "build",
            "--locked",
            "--package",
            "runbook-expr",
            "--example",
            "parity",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target.0)
        .output()
        .expect("launch fresh Wasm build");
    assert!(
        build.status.success(),
        "fresh Wasm build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let output = Command::new("node")
        .arg(manifest.join("tests/wasm-runner.mjs"))
        .arg(
            target
                .0
                .join("wasm32-unknown-unknown/debug/examples/parity.wasm"),
        )
        .output()
        .expect("launch Node Wasm runner");
    assert!(
        output.status.success(),
        "Wasm runner failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let wasm: serde_json::Value = serde_json::from_slice(&output.stdout).expect("Wasm JSON");
    parity_corpus::assert_corpus(&wasm);
    let native = parity_corpus::corpus();
    assert_eq!(native, wasm, "complete native/Wasm JSON parity");
    println!(
        "{} complete expression corpus rows agree across native and fresh Wasm",
        native.as_array().unwrap().len()
    );
}
