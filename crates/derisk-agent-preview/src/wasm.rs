//! Runs a `wasm32-wasip2` preview component with this process's stdio.
//!
//! The host starts `derisk-agent preview-wasm <component>` as a child, so a wasm
//! preview speaks the same protocol on the same pipes as a native one. The
//! component gets stdio and nothing else: no files, no network, no environment.

use std::path::Path;

use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

struct State {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

/// wasmtime's errors are `anyhow` errors, which miette cannot take with `?`;
/// their chain goes into the report's message.
fn wasmtime(error: wasmtime::Error) -> miette::Report {
    miette::miette!("{error:#}")
}

/// Runs `component`'s `wasi:cli/run` export to completion.
pub fn run(component: &Path) -> miette::Result<()> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config).map_err(wasmtime)?;
    let component = Component::from_file(&engine, component)
        .map_err(|error| miette::miette!("loading {}: {error:#}", component.display()))?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(wasmtime)?;
    let ctx = WasiCtxBuilder::new()
        .stdin(wasmtime_wasi::cli::stdin())
        .stdout(wasmtime_wasi::cli::stdout())
        .stderr(wasmtime_wasi::cli::stderr())
        .build();
    let mut store = Store::new(
        &engine,
        State {
            ctx,
            table: ResourceTable::new(),
        },
    );
    let command =
        wasmtime_wasi::p2::bindings::sync::Command::instantiate(&mut store, &component, &linker)
            .map_err(wasmtime)?;
    command
        .wasi_cli_run()
        .call_run(&mut store)
        .map_err(wasmtime)?
        .map_err(|()| miette::miette!("the preview component exited with an error"))
}
