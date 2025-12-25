use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::*;
use wasmtime_wasi::p2::bindings::Command;
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

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
    wasm_module: PathBuf,
}

impl WasmRunner {
    pub fn new(wasm_module: PathBuf) -> Self {
        Self { wasm_module }
    }

    pub async fn execute(self, input: Vec<u8> /* Bytes? */) -> Result<Value> {
        let mut config = Config::new();
        config.async_support(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);

        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;

        let json_bytes = input;
        let stdin = MemoryInputPipe::new(json_bytes);
        let stdout = MemoryOutputPipe::new(usize::MAX);
        let wasi = WasiCtx::builder()
            .stdin(stdin)
            .inherit_stdout()
            .inherit_stderr()
            .build();
        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
            http_ctx: WasiHttpCtx::new(),
        };
        let mut store = Store::new(&engine, state);
        let component = Component::from_file(&engine, self.wasm_module)?;
        let command = Command::instantiate_async(&mut store, &component, &linker).await?;
        let program_result = command.wasi_cli_run().call_run(&mut store).await?;

        if program_result.is_err() {
            anyhow::bail!("WASM module execution failed");
        }

        drop(store);

        let output_bytes = stdout
            .try_into_inner()
            .context("Failed to retrieve stdout")?
            .into_iter()
            .collect::<Vec<u8>>();
        let output_str = String::from_utf8(output_bytes)?;
        let output: Value = serde_json::from_str(&output_str)?;

        Ok(output)
    }
}
