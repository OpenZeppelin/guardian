use miden_processor::ExecutionOptions;
use miden_vm::{
    assembly::{debuginfo::SourceManagerExt, DefaultSourceManager}, execute, execute_iter, prove, verify, AdviceInputs, Assembler, DefaultHost, ExecutionError, ExecutionProof, ProgramInfo, ProvingOptions, StackInputs, StackOutputs, VerificationError
};
use std::{path::Path, sync::Arc};

pub fn run() {
    let assembler = Assembler::default();
    let source_manager = Arc::new(DefaultSourceManager::default());
    let source = source_manager.load_file(Path::new("src/masm/add.masm")).unwrap();
    let program = assembler.assemble_program(source).unwrap();

    // execute the program with no inputs
    let _trace = execute(
        &program,
        StackInputs::default(),
        AdviceInputs::default(),
        &mut DefaultHost::default(),
        ExecutionOptions::default(),
        source_manager.clone(),
    )
    .unwrap();

    // now, execute the same program in debug mode and iterate over VM states
    for vm_state in execute_iter(
        &program,
        StackInputs::default(),
        AdviceInputs::default(),
        &mut DefaultHost::default(),
        source_manager,
    ) {
        match vm_state {
            Ok(vm_state) => println!("{:?}", vm_state),
            Err(_) => println!("something went terribly wrong!"),
        }
    }
}


pub fn run_prove() -> Result<(StackOutputs, ExecutionProof), ExecutionError> {
      let assembler = Assembler::default();
      let source_manager = Arc::new(DefaultSourceManager::default());
      let source = source_manager.load_file(Path::new("src/masm/add.masm")).unwrap();
      let program = assembler.assemble_program(source).unwrap();

      let (outputs, proof) = prove(
        &program,
        StackInputs::default(),
        AdviceInputs::default(),
        &mut DefaultHost::default(),
        ProvingOptions::default(),
        source_manager,
    )?;

    println!("outputs: {:?}", outputs);

    Ok((outputs, proof))
}

pub fn run_verify(outputs: StackOutputs, proof: ExecutionProof) -> Result<bool, VerificationError> {
  let assembler = Assembler::default();
  let source_manager = Arc::new(DefaultSourceManager::default());
  let source = source_manager.load_file(Path::new("src/masm/add.masm")).unwrap();
  let program = assembler.assemble_program(source).unwrap();

  let result = verify(
    ProgramInfo::from(program),
    StackInputs::default(),
    outputs,
    proof,
  )?;

  println!("result: {:?}", result);

  Ok(true)
}