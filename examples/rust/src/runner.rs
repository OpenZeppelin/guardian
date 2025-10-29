use miden_processor::{ExecutionOptions, MemAdviceProvider};
use miden_stdlib::StdLibrary;
use miden_vm::{
    AdviceInputs, DefaultHost, Program, ProgramInfo,
    StackInputs, StackOutputs,
    assembly::{Assembler, DefaultSourceManager},
    diagnostics::SourceManagerExt,
    execute, verify,
};
use miden_prover::{ExecutionProof, ProvingOptions, prove};
use std::{path::Path, sync::Arc};

type Result<T> = anyhow::Result<T>;

pub struct MasmRunner {
    program: Program,
    stack_inputs: StackInputs,
    advice_inputs: AdviceInputs,
}

impl MasmRunner {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let stdlib = StdLibrary::default();
        let assembler = Assembler::default()
            .with_library(stdlib.as_ref())
            .map_err(|e| anyhow::anyhow!("Failed to load stdlib: {}", e))?;
        let source_manager = Arc::new(DefaultSourceManager::default());

        let source = source_manager
            .load_file(path.as_ref())
            .map_err(|e| anyhow::anyhow!("Failed to load MASM file {}: {}", path.as_ref().display(), e))?;

        let program = assembler
            .assemble_program(source)
            .map_err(|e| anyhow::anyhow!("Failed to assemble MASM program: {}", e))?;

        Ok(Self {
            program,
            stack_inputs: StackInputs::default(),
            advice_inputs: AdviceInputs::default(),
        })
    }

    pub fn with_stack_inputs(mut self, inputs: StackInputs) -> Self {
        self.stack_inputs = inputs;
        self
    }

    pub fn with_advice_inputs(mut self, inputs: AdviceInputs) -> Self {
        self.advice_inputs = inputs;
        self
    }

    pub fn run(&self) -> Result<StackOutputs> {
        let advice_provider = MemAdviceProvider::from(self.advice_inputs.clone());
        let mut host = DefaultHost::new(advice_provider);

        let trace = execute(
            &self.program,
            self.stack_inputs.clone(),
            &mut host,
            ExecutionOptions::default(),
        )
        .map_err(|e| anyhow::anyhow!("Program execution failed: {}", e))?;

        Ok(trace.stack_outputs().clone())
    }

    pub fn prove(&self) -> Result<(StackOutputs, ExecutionProof)> {
        let advice_provider = MemAdviceProvider::from(self.advice_inputs.clone());
        let host = DefaultHost::new(advice_provider);

        let (outputs, proof) = prove(
            &self.program,
            self.stack_inputs.clone(),
            host,
            ProvingOptions::default(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to generate proof: {}", e))?;

        Ok((outputs, proof))
    }

    pub fn verify(
        &self,
        outputs: StackOutputs,
        proof: ExecutionProof,
    ) -> Result<bool> {
        verify(
            ProgramInfo::from(self.program.clone()),
            self.stack_inputs.clone(),
            outputs,
            proof,
        )
        .map_err(|e| anyhow::anyhow!("Proof verification failed: {}", e))?;

        Ok(true)
    }

    pub fn program(&self) -> &Program {
        &self.program
    }
}