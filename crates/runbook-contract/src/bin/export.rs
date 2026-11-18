fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: export <output.json>")?;
    let mut json = serde_json::to_string_pretty(&runbook_contract::openapi())?;
    json.push('\n');
    std::fs::write(path, json)?;
    Ok(())
}
