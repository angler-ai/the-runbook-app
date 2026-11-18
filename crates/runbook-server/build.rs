//! UPD1 review #2: bake the rollback floor (src/rollback_floor.rs) into the
//! binary, so `runbook-server --version` reports it for any build: a
//! release image, a checkout's `docker compose build`, `cargo build`.
include!("src/rollback_floor.rs");

fn main() {
    // The native edition's stores: the server's tables and the sign-in
    // service's (applied when `serve` starts it). A missing directory is an
    // error, so a build can never report a floor computed from less.
    let dirs = [
        std::path::Path::new("../runbook-store-postgres/migrations"),
        std::path::Path::new("../../auth/drizzle/pg"),
    ];
    let cargo = std::path::Path::new("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", cargo.display());
    let mut files: Vec<String> = Vec::new();
    for dir in dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("rollback floor: cannot read {}: {e}", dir.display()));
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sql") {
                files.push(std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("rollback floor: cannot read {}: {e}", path.display())
                }));
            }
        }
    }
    files.sort();
    let toml = std::fs::read_to_string(cargo)
        .expect("rollback floor: cannot read the workspace Cargo.toml");
    let floor = rollback_floor(files.iter().map(String::as_str), &toml);
    println!(
        "cargo:rustc-env=RUNBOOK_ROLLBACK_FLOOR={}",
        floor.unwrap_or_default()
    );
}
