use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow};
use miden_lib::transaction::TransactionKernel;
use miden_objects::{
    account::{AccountComponent, StorageSlot},
    assembly::{Assembler, DefaultSourceManager, Library, LibraryPath, Module, ModuleKind},
};

/// MASM root set by build.rs
fn masm_root() -> PathBuf {
    PathBuf::from(env!("OZ_MASM_DIR"))
}

/// masm/auth folder path
fn auth_dir() -> PathBuf {
    masm_root().join("auth")
}

/// Recursively collects all `.masm` files under the given root directory.
fn collect_all_masm_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        if !dir.exists() {
            continue;
        }

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("masm") {
                files.push(path);
            }
        }
    }

    Ok(files)
}

/// Builds the OpenZeppelin library from all MASM files under `masm/`.
///
/// Examples:
/// - masm/auth/multisig.masm           -> openzeppelin::multisig
/// - masm/auth/multisig_ecdsa.masm     -> openzeppelin::multisig_ecdsa
/// - masm/auth/psm.masm                -> openzeppelin::psm
/// - masm/auth/psm_ecdsa.masm          -> openzeppelin::psm_ecdsa
/// - masm/account/access.masm          -> openzeppelin::access
/// - masm/account/utils/example.masm   -> openzeppelin::example
fn build_openzeppelin_library() -> Result<Library> {
    let root = masm_root();
    let source_manager = Arc::new(DefaultSourceManager::default());

    let masm_files = collect_all_masm_files(&root)?;
    let mut modules = Vec::new();
    let mut seen_names = HashSet::<String>::new();

    for path in masm_files {
        let stem = path
            .file_stem()
            .expect("file_stem")
            .to_string_lossy()
            .into_owned();

        // Aynı isimde iki farklı dosya (ör: auth/access.masm & account/access.masm)
        // olursa bunu yakalayalım, çünkü ikisi de openzeppelin::access olurdu.
        if !seen_names.insert(stem.clone()) {
            return Err(anyhow!(
                "duplicate MASM module name '{stem}' under masm/; \
                 this would map to the same 'openzeppelin::{stem}' path"
            ));
        }

        let lib_path = format!("openzeppelin::{stem}");
        let code = fs::read_to_string(&path)?;

        let module = Module::parser(ModuleKind::Library)
            .parse_str(LibraryPath::new(&lib_path)?, code, &source_manager)
            .map_err(|e| anyhow!("failed to parse module {lib_path}: {e}"))?;

        modules.push(module);
    }

    // Assemble library with debug mode enabled
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);

    let library: Library = assembler
        .clone()
        .assemble_library(modules)
        .map_err(|e| anyhow!("failed to assemble openzeppelin library: {e}"))?;

    Ok(library)
}

// Builds the assembler with the openzeppelin library linked.
fn build_assembler() -> Result<Assembler> {
    let oz_lib = build_openzeppelin_library()?;

    let asm: Assembler = TransactionKernel::assembler()
        .with_debug_mode(true)
        .with_dynamic_library(oz_lib)
        .map_err(|e| anyhow!("failed to link openzeppelin library: {e}"))?;

    Ok(asm)
}

// ============================================================================
// COMPONENT BUILDERS
// ============================================================================

/// Build AccountComponent from masm/auth/multisig.masm.
/// This component provides multi-signature authentication.
/// It requires the PSM component to be added separately if PSM verification is needed.
/// Assembler comes with the openzeppelin library (all modules) loaded.
///
/// Storage layout (4 slots):
/// - Slot 0: Threshold config [default_threshold, num_approvers, 0, 0]
/// - Slot 1: Approver public keys map
/// - Slot 2: Executed transactions map
/// - Slot 3: Procedure threshold overrides map
pub fn build_multisig_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_assembler()?;

    let path = auth_dir().join("multisig.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let component = AccountComponent::compile(code, asm, slots)?.with_supports_all_types();

    Ok(component)
}

/// Build AccountComponent from masm/auth/multisig_ecdsa.masm.
/// This component provides multi-signature authentication using ECDSA signatures.
/// It requires the PSM component to be added separately if PSM verification is needed.
/// Assembler comes with the openzeppelin library (all modules) loaded.
pub fn build_multisig_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_assembler()?;

    let path = auth_dir().join("multisig_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let component = AccountComponent::compile(code, asm, slots)?.with_supports_all_types();

    Ok(component)
}

/// Build AccountComponent from masm/auth/psm.masm.
/// This component provides PSM (Private State Manager) signature verification.
///
/// Storage layout (2 slots):
/// - Slot 0: PSM selector [selector, 0, 0, 0] where selector=1 means ON, 0 means OFF
/// - Slot 1: PSM public key map
pub fn build_psm_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_assembler()?;

    let path = auth_dir().join("psm.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let component = AccountComponent::compile(code, asm, slots)?.with_supports_all_types();

    Ok(component)
}

/// Build AccountComponent from masm/auth/psm_ecdsa.masm.
/// This component provides PSM (Private State Manager) signature verification using ECDSA.
pub fn build_psm_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_assembler()?;

    let path = auth_dir().join("psm_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let component = AccountComponent::compile(code, asm, slots)?.with_supports_all_types();

    Ok(component)
}

/// Build Access component from masm/account/access.masm.
pub fn build_access_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_assembler()?;

    let path = masm_root().join("account").join("access.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let component = AccountComponent::compile(code, asm, slots)?.with_supports_all_types();

    Ok(component)
}

/// Creates a Library from the given MASM code and library path.
pub fn create_library(
    account_code: String,
    library_path: &str,
) -> Result<Library, Box<dyn std::error::Error>> {
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);
    let source_manager = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library).parse_str(
        LibraryPath::new(library_path)?,
        account_code,
        &source_manager,
    )?;
    let library = assembler.clone().assemble_library([module])?;
    Ok(library)
}

/// Builds the OpenZeppelin library for use in transaction scripts.
/// This library contains all MASM modules from the masm/ directory.
pub fn get_openzeppelin_library() -> Result<Library> {
    build_openzeppelin_library()
}

/// Builds a library for multisig procedures for use in transaction scripts.
/// The procedures are accessible via `call.::procedure_name` syntax.
pub fn get_multisig_library() -> Result<Library> {
    let path = auth_dir().join("multisig.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Build with openzeppelin library linked (for psm dependency)
    let asm = build_assembler()?;

    let library = asm
        .assemble_library([code])
        .map_err(|e| anyhow!("failed to assemble multisig library: {e}"))?;

    Ok(library)
}

/// Builds a library for multisig ECDSA procedures for use in transaction scripts.
/// The procedures are accessible via `call.::procedure_name` syntax.
pub fn get_multisig_ecdsa_library() -> Result<Library> {
    let path = auth_dir().join("multisig_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Build with openzeppelin library linked (for psm dependency)
    let asm = build_assembler()?;

    let library = asm
        .assemble_library([code])
        .map_err(|e| anyhow!("failed to assemble multisig ECDSA library: {e}"))?;

    Ok(library)
}

/// Builds a library for PSM procedures for use in transaction scripts.
/// The procedures are accessible via `call.::procedure_name` syntax.
pub fn get_psm_library() -> Result<Library> {
    let path = auth_dir().join("psm.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Pass source code directly to avoid namespace issues
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);

    let library = assembler
        .assemble_library([code])
        .map_err(|e| anyhow!("failed to assemble PSM library: {e}"))?;

    Ok(library)
}

/// Builds a library for PSM ECDSA procedures for use in transaction scripts.
/// The procedures are accessible via `call.::procedure_name` syntax.
pub fn get_psm_ecdsa_library() -> Result<Library> {
    let path = auth_dir().join("psm_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Pass source code directly to avoid namespace issues
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);

    let library = assembler
        .assemble_library([code])
        .map_err(|e| anyhow!("failed to assemble PSM ECDSA library: {e}"))?;

    Ok(library)
}
