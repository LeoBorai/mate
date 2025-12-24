use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use wasmtime::{Config, Engine, Linker, Module, Store};

pub struct WasmRunner {
    engine: Engine,
    wasm_path: PathBuf,
}

impl WasmRunner {
    pub fn new(wasm_path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Config::new();
        config.async_support(true);
        let engine = Engine::new(&config)?;
        let wasm_path = wasm_path.as_ref().to_path_buf();

        if !wasm_path.exists() {
            anyhow::bail!("WASM file not found: {}", wasm_path.display());
        }

        Ok(Self { engine, wasm_path })
    }

    pub async fn execute<O>(&self, input: Vec<u8>) -> Result<O>
    where
        O: DeserializeOwned,
    {
        let mut store = Store::new(&self.engine, ());

        let module = Module::from_file(&self.engine, &self.wasm_path)
            .context("Failed to load WASM module")?;

        let linker = Linker::new(&self.engine);
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .context("Failed to instantiate module")?;

        let allocate = instance
            .get_typed_func::<u32, u32>(&mut store, "allocate")
            .context("Failed to find 'allocate' export")?;

        let process = instance
            .get_typed_func::<(u32, u32), u32>(&mut store, "process")
            .context("Failed to find 'process' export")?;

        let deallocate = instance
            .get_typed_func::<(u32, u32), ()>(&mut store, "deallocate")
            .context("Failed to find 'deallocate' export")?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .context("Failed to find 'memory' export")?;

        let input_len = input.len() as u32;

        println!("Input Bytes: {}", input_len);
        println!("Input JSON: {}", String::from_utf8_lossy(&input));

        let input_ptr = allocate
            .call_async(&mut store, input_len)
            .await
            .context("Failed to allocate input memory")?;

        memory
            .write(&mut store, input_ptr as usize, &input)
            .context("Failed to write input to memory")?;

        let output_ptr = process
            .call_async(&mut store, (input_ptr, input_len))
            .await
            .context("Failed to call process function")?;

        println!("Output Pointer: {}", output_ptr);

        if output_ptr == 0 {
            anyhow::bail!("Process function returned null");
        }

        // Read output length
        let mut len_bytes = [0u8; 4];
        memory
            .read(&store, output_ptr as usize, &mut len_bytes)
            .context("Failed to read output length")?;
        let output_len = u32::from_le_bytes(len_bytes) as usize;

        let mut output_bytes = vec![0u8; output_len];
        memory
            .read(&store, (output_ptr + 4) as usize, &mut output_bytes)
            .context("Failed to read output data")?;

        let output: O =
            serde_json::from_slice(&output_bytes).context("Failed to deserialize output")?;

        deallocate
            .call_async(&mut store, (input_ptr, input_len))
            .await
            .context("Failed to deallocate input")?;

        deallocate
            .call_async(&mut store, (output_ptr, 4 + output_len as u32))
            .await
            .context("Failed to deallocate output")?;

        Ok(output)
    }

    // Execute with fuel limiting to prevent infinite loops

    // Fuel is a mechanism to limit WASM execution time. Each WASM instruction
    // consumes fuel, and execution stops when fuel runs out.

    // # Arguments

    // * `input` - Reference to the input data
    // * `fuel` - Maximum amount of fuel to provide (instructions to execute)

    // # Examples

    // ```no_run
    // use wasm_runner::WasmRunner;
    // use serde::{Serialize, Deserialize};

    // #[derive(Serialize)]
    // struct Input { value: i32 }

    // #[derive(Deserialize)]
    // struct Output { result: i32 }

    // let runner = WasmRunner::new("module.wasm")?;
    // let input = Input { value: 42 };

    // // Limit to 1 million instructions
    // let output: Output = runner.execute_with_fuel(&input, 1_000_000)?;
    // # Ok::<(), anyhow::Error>(())
    // ```
    // pub fn execute_with_fuel<I, O>(&self, input: &I, fuel: u64) -> Result<O>
    // where
    //     I: Serialize,
    //     O: DeserializeOwned,
    // {
    //     let mut config = Config::new();
    //     config.consume_fuel(true);
    //     let engine = Engine::new(&config)?;

    //     let mut store = Store::new(&engine, ());
    //     store.set_fuel(fuel)?;

    //     let module = Module::from_file(&engine, &self.wasm_path)?;
    //     let linker = Linker::new(&engine);
    //     let instance = linker.instantiate(&mut store, &module)?;
    //     let allocate = instance.get_typed_func::<u32, u32>(&mut store, "allocate")?;
    //     let process = instance.get_typed_func::<(u32, u32), u32>(&mut store, "process")?;
    //     let deallocate = instance.get_typed_func::<(u32, u32), ()>(&mut store, "deallocate")?;
    //     let memory = instance
    //         .get_memory(&mut store, "memory")
    //         .context("Failed to get memory")?;

    //     let input_json = serde_json::to_vec(input)?;
    //     let input_len = input_json.len() as u32;
    //     let input_ptr = allocate.call(&mut store, input_len)?;

    //     memory.write(&mut store, input_ptr as usize, &input_json)?;

    //     let output_ptr = process.call(&mut store, (input_ptr, input_len))?;
    //     if output_ptr == 0 {
    //         anyhow::bail!("Process function returned null");
    //     }

    //     let mut len_bytes = [0u8; 4];
    //     memory.read(&store, output_ptr as usize, &mut len_bytes)?;
    //     let output_len = u32::from_le_bytes(len_bytes) as usize;

    //     let mut output_bytes = vec![0u8; output_len];
    //     memory.read(&store, (output_ptr + 4) as usize, &mut output_bytes)?;

    //     let output: O = serde_json::from_slice(&output_bytes)?;

    //     deallocate.call(&mut store, (input_ptr, input_len))?;
    //     deallocate.call(&mut store, (output_ptr, 4 + output_len as u32))?;

    //     Ok(output)
    // }
}
