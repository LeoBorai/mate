use std::path::Path;

use anyhow::Result;

use mate_runner::WasmRunner;
use serde_json::Value;

fn main() -> Result<()> {
    let wasm = Path::new("complex.wasm");
    let runner = WasmRunner::new(wasm)?;
    let input = r#"{
        "name": "Alice",
        "email": "alice@mate.io",
        "age": 30,
        "tags": ["new user", "beta tester"]
    }"#;

    let output: Value = runner.execute(input.as_bytes().to_vec())?;

    println!("📤 Output:");
    println!("{:#?}\n", output);

    Ok(())
}
