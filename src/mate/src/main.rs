use std::{fs::read, path::Path};

use anyhow::Result;
use serde_json::Value;

use mate_executor::Executor;

#[tokio::main]
async fn main() -> Result<()> {
    let wasm = Path::new("http.wasm");
    let wasm = read(wasm)?;
    let executor = Executor::new();
    let input = r#"{
        "api_url": "https://httpbin.org/post",
        "data": {
            "sample_key": "sample_value"
        }
    }"#;

    let output: Value = executor.run(wasm.into(), input.as_bytes().into()).await?;

    println!("📤 Output:");
    println!("{:#?}\n", output);

    Ok(())
}
