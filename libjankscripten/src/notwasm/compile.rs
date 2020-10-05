use super::syntax::Program;
use super::*;
use parity_wasm::elements::Error;

pub fn compile<G>(mut program: Program, inspect: G) -> Result<Vec<u8>, Error>
where
    G: FnOnce(&Program) -> (),
{
    //label_apps(&mut program);
    //elim_gotos(&mut program);
    let notwasm_rt = parse(include_str!("runtime.notwasm"));
    program.merge_in(notwasm_rt);
    type_checking::type_check(&mut program).expect("type-checking failed");
    intern(&mut program);
    inspect(&program);
    translate(program)
}
