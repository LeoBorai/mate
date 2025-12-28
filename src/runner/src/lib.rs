use anyhow::{Context, Result};
use serde_json::Value;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

/// Fully qualified name of the handler function in the WASM module.
/// Format: `<package>/<interface>#<function>`
const HANDLER_FUNC_FQN: &str = "mate:runtime@0.1.0/api#handler";

pub struct ComponentRunStates {
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
    pub http_ctx: WasiHttpCtx,
}

impl WasiView for ComponentRunStates {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

impl WasiHttpView for ComponentRunStates {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http_ctx
    }

    fn table(&mut self) -> &mut ResourceTable {
        &mut self.resource_table
    }
}

pub struct WasmRunner {
    wasm_module: Vec<u8>,
}

impl WasmRunner {
    pub fn new(wasm_module: Vec<u8>) -> Self {
        Self { wasm_module }
    }

    pub async fn execute(self, input: Vec<u8> /* Bytes? */) -> Result<Value> {
        let mut config = Config::new();
        config
            .async_support(true)
            .wasm_component_model_async(true)
            .wasm_component_model_async_builtins(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);

        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;

        let json_value =
            serde_json::from_slice::<Value>(&input).context("Failed to parse input JSON")?;
        let json = serde_json::to_string(&json_value).context("Failed to serialize input JSON")?;
        let wasi = WasiCtx::builder().build();
        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
            http_ctx: WasiHttpCtx::new(),
        };
        let mut store = Store::new(&engine, state);
        let component = Component::from_binary(&engine, &self.wasm_module)?;
        let instance = linker.instantiate_async(&mut store, &component).await?;
        let func = instance
            .get_typed_func::<(String,), (Result<String, String>,)>(&mut store, HANDLER_FUNC_FQN)
            .context(format!("Function '{HANDLER_FUNC_FQN}' not found"))?;
        let (output,) = func.call_async(&mut store, (json,)).await?;
        println!("OUT: {:?}", output);

        //         // Extract the result
        //         let output = match &results[0] {
        //             Val::String(s) => s.to_string(),
        //             _ => anyhow::bail!("Expected string result"),
        //         };

        //         Ok(output)

        // let command = Command::instantiate_async(&mut store, &component, &linker).await?;
        // let program_result = command.wasi_cli_run().call_run(&mut store).await?;

        // if program_result.is_err() {
        //     anyhow::bail!("WASM module execution failed");
        // }

        // drop(store);

        // let output_bytes = stdout
        //     .try_into_inner()
        //     .context("Failed to retrieve stdout")?
        //     .into_iter()
        //     .collect::<Vec<u8>>();
        // let output_str = String::from_utf8(output_bytes)?;
        // let output: Value = serde_json::from_str(&output_str)?;

        // Ok(output)
        Ok(Value::Null)
    }
}
