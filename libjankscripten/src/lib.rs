//! the jankscripten system without IO/main

pub mod javascript;
pub mod shared;
pub mod jankierscript;
pub mod jankyscript;
pub mod notwasm;
mod rope;

#[macro_use]
extern crate combine;

pub fn javascript_to_wasm(js_code: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>>  {
    let js_ast = javascript::parse(js_code)?;
    let jankier_ast = jankierscript::from_javascript(js_ast);
    let janky_ast = jankierscript::insert_coercions(jankier_ast)?;
    let notwasm_ast = notwasm::from_jankyscript(janky_ast);
    let wasm_bin = notwasm::compile(notwasm_ast)?;
    Ok(wasm_bin)
}
