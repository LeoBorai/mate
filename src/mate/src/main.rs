use std::path::Path;

use anyhow::Result;

use mate_runner::WasmRunner;
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<()> {
    let wasm = Path::new("http.wasm");
    let bytes = std::fs::read(wasm)?;
    let runner = WasmRunner::new(bytes);
    let input = r#"{
        "api_url": "https://httpbin.org/post",
        "data": {
            "sample_key": "sample_value"
        }
    }"#;

    let output: Value = runner.execute(input.as_bytes().to_vec()).await?;

    println!("📤 Output:");
    println!("{:#?}\n", output);

    Ok(())
}
