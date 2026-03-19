use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow};
use miden_protocol::{
    account::{AccountComponent, StorageSlot},
    assembly::{
        Assembler, DefaultSourceManager, Library, Module, ModuleKind, Path as LibraryPath,
        SourceManager,
    },
    transaction::TransactionKernel,
};
use miden_standards::StandardsLib;

/// MASM root set by build.rs
fn masm_root() -> PathBuf {
    PathBuf::from(env!("OZ_MASM_DIR"))
}

/// masm/auth folder path
fn auth_dir() -> PathBuf {
    masm_root().join("auth")
}

fn account_components_auth_dir() -> PathBuf {
    masm_root().join("account_components").join("auth")
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

    files.sort();
    Ok(files)
}

fn openzeppelin_library_path(path: &Path, root: &Path) -> Result<String> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|error| anyhow!("failed to strip MASM root prefix: {error}"))?;
    let relative_path = relative_path.with_extension("");
    let path_segments = relative_path
        .iter()
        .map(|segment| segment.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("::");

    Ok(format!("openzeppelin::{path_segments}"))
}

fn compile_component(path: &Path, slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let asm = build_component_assembler()?;
    let code = fs::read_to_string(path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;
    let library = compile_to_library(&code, &asm)?;
    let component = AccountComponent::new(library, slots)
        .map_err(|e| anyhow!("failed to create component: {e}"))?
        .with_supports_all_types();

    Ok(component)
}

fn build_component_assembler() -> Result<Assembler> {
    let oz_lib = build_openzeppelin_library()?;
    let standards_lib: Library = StandardsLib::default().into();

    let mut asm: Assembler = TransactionKernel::assembler();
    let _ = asm.link_static_library(oz_lib);
    let _ = asm.link_static_library(standards_lib);

    Ok(asm)
}

/// Builds the OpenZeppelin library from all MASM files under `masm/`.
fn build_openzeppelin_library() -> Result<Library> {
    let root = masm_root();
    let source_manager: Arc<dyn SourceManager> = Arc::new(DefaultSourceManager::default());

    let masm_files = collect_all_masm_files(&root)?;
    let mut modules = Vec::new();

    for path in masm_files {
        let lib_path = openzeppelin_library_path(&path, &root)?;
        let code = fs::read_to_string(&path)?;

        let module = Module::parser(ModuleKind::Library)
            .parse_str(LibraryPath::new(&lib_path), code, source_manager.clone())
            .map_err(|e| anyhow!("failed to parse module {lib_path}: {e}"))?;

        modules.push(module);
    }

    // Assemble library with miden-standards library linked (provides miden::standards::auth::*)
    let mut assembler: Assembler = TransactionKernel::assembler();
    let standards_lib: Library = StandardsLib::default().into();
    let _ = assembler.link_dynamic_library(&standards_lib);

    let library: Library = assembler
        .clone()
        .assemble_library(modules)
        .map_err(|e| anyhow!("failed to assemble openzeppelin library: {e}"))?;

    Ok(library)
}

// Builds the assembler with the openzeppelin library and miden-standards library linked.
fn build_assembler() -> Result<Assembler> {
    let oz_lib = build_openzeppelin_library()?;
    let standards_lib: Library = StandardsLib::default().into();

    let mut asm: Assembler = TransactionKernel::assembler();
    let _ = asm.link_dynamic_library(&oz_lib);
    let _ = asm.link_dynamic_library(&standards_lib);

    Ok(asm)
}

/// Compiles MASM code into a Library using the given assembler
fn compile_to_library(code: &str, assembler: &Assembler) -> Result<Library> {
    let library = assembler
        .clone()
        .assemble_library([code])
        .map_err(|e| anyhow!("failed to assemble library: {e}"))?;
    Ok(library)
}

// ============================================================================
// COMPONENT BUILDERS
// ============================================================================

/// Build AccountComponent from masm/account_components/auth/multisig.masm.
pub fn build_multisig_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&account_components_auth_dir().join("multisig.masm"), slots)
}

/// Build AccountComponent from masm/account_components/auth/multisig_ecdsa.masm.
pub fn build_multisig_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(
        &account_components_auth_dir().join("multisig_ecdsa.masm"),
        slots,
    )
}

/// Build AccountComponent from masm/account_components/auth/multisig_psm.masm.
pub fn build_multisig_psm_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(
        &account_components_auth_dir().join("multisig_psm.masm"),
        slots,
    )
}

/// Build AccountComponent from masm/account_components/auth/multisig_psm_ecdsa.masm.
pub fn build_multisig_psm_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(
        &account_components_auth_dir().join("multisig_psm_ecdsa.masm"),
        slots,
    )
}

/// Build AccountComponent from masm/auth/psm.masm.
/// This component provides PSM (Private State Manager) signature verification.
///
/// Storage layout (2 slots):
/// - Slot 0: PSM selector [selector, 0, 0, 0] where selector=1 means ON, 0 means OFF
/// - Slot 1: PSM public key map
pub fn build_psm_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&auth_dir().join("psm.masm"), slots)
}

/// Build AccountComponent from masm/auth/psm_ecdsa.masm.
pub fn build_psm_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&auth_dir().join("psm_ecdsa.masm"), slots)
}

/// Build Access component from masm/account/access.masm.
pub fn build_access_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&masm_root().join("account").join("access.masm"), slots)
}

/// Creates a Library from the given MASM code and library path.
pub fn create_library(
    account_code: String,
    library_path: &str,
) -> Result<Library, Box<dyn std::error::Error>> {
    let assembler: Assembler = TransactionKernel::assembler();
    let source_manager: Arc<dyn SourceManager> = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library).parse_str(
        LibraryPath::new(library_path),
        account_code,
        source_manager,
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
/// The procedures are accessible via `use oz_multisig::multisig` and `call.multisig::procedure_name` syntax.
pub fn get_multisig_library() -> Result<Library> {
    let path = auth_dir().join("multisig.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Build with openzeppelin library linked (for psm dependency)
    let asm = build_assembler()?;

    let source_manager: Arc<dyn SourceManager> = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library)
        .parse_str(
            LibraryPath::new("oz_multisig::multisig"),
            code,
            source_manager,
        )
        .map_err(|e| anyhow!("failed to parse multisig module: {e}"))?;

    let library = asm
        .assemble_library([module])
        .map_err(|e| anyhow!("failed to assemble multisig library: {e}"))?;

    Ok(library)
}

/// Builds an ECDSA multisig library for use in transaction scripts.
/// The procedures are accessible via `use oz_multisig::multisig` and `call.multisig::procedure_name` syntax.
pub fn get_multisig_ecdsa_library() -> Result<Library> {
    let path = auth_dir().join("multisig_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    let asm = build_assembler()?;

    let source_manager: Arc<dyn SourceManager> = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library)
        .parse_str(
            LibraryPath::new("oz_multisig::multisig"),
            code,
            source_manager,
        )
        .map_err(|e| anyhow!("failed to parse multisig ecdsa module: {e}"))?;

    let library = asm
        .assemble_library([module])
        .map_err(|e| anyhow!("failed to assemble multisig ecdsa library: {e}"))?;

    Ok(library)
}

/// Builds a library for PSM procedures for use in transaction scripts.
/// The procedures are accessible via `use oz_psm::psm` and `call.psm::procedure_name` syntax.
pub fn get_psm_library() -> Result<Library> {
    let path = auth_dir().join("psm.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;

    // Build with openzeppelin library and miden-standards linked
    let asm = build_assembler()?;

    let source_manager: Arc<dyn SourceManager> = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library)
        .parse_str(LibraryPath::new("oz_psm::psm"), code, source_manager)
        .map_err(|e| anyhow!("failed to parse psm module: {e}"))?;

    let library = asm
        .assemble_library([module])
        .map_err(|e| anyhow!("failed to assemble PSM library: {e}"))?;

    Ok(library)
}
