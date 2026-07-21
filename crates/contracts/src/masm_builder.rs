use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow};
use miden_protocol::{
    account::{AccountComponent, AccountComponentMetadata, StorageSlot},
    assembly::{
        Assembler, DefaultSourceManager, Library, Linkage, Module, ModuleKind, ModuleParser,
        Path as LibraryPath, SourceManagerSync,
    },
    transaction::TransactionKernel,
};
use miden_standards::{StandardsLib, code_builder::CodeBuilder};

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

/// CodeBuilder with every `openzeppelin::auth` module statically linked.
fn auth_code_builder() -> Result<CodeBuilder> {
    let mut builder = CodeBuilder::new();

    for path in collect_all_masm_files(&auth_dir())? {
        let lib_path = openzeppelin_library_path(&path, &masm_root())?;
        let code = fs::read_to_string(&path)
            .map_err(|e| anyhow!("failed to read {}: {e}", path.display()))?;
        builder
            .link_module(&lib_path, code)
            .map_err(|e| anyhow!("failed to link auth module {lib_path}: {e}"))?;
    }

    Ok(builder)
}

fn compile_component(path: &Path, slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    let builder = auth_code_builder()?;
    let code = fs::read_to_string(path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;
    let component_path = openzeppelin_library_path(path, &masm_root())?;

    let component_code = builder
        .compile_component_code(&component_path, code)
        .map_err(|e| anyhow!("failed to compile component {component_path}: {e}"))?;

    let metadata = AccountComponentMetadata::new(component_path);
    let component = AccountComponent::new(component_code, slots, metadata)
        .map_err(|e| anyhow!("failed to create component: {e}"))?;

    Ok(component)
}

fn kernel_assembler(source_manager: Arc<dyn SourceManagerSync>) -> Result<Assembler> {
    let mut assembler = TransactionKernel::assembler_with_source_manager(source_manager);
    assembler
        .link_package(Arc::new(StandardsLib::default().into()), Linkage::Dynamic)
        .map_err(|e| anyhow!("failed to link Miden standards library: {e}"))?;
    Ok(assembler)
}

fn parse_module(
    parser_path: &str,
    code: String,
    source_manager: Arc<dyn SourceManagerSync>,
) -> Result<Box<Module>> {
    let mut parser = ModuleParser::new(Some(ModuleKind::Library));
    parser
        .parse_str(Some(LibraryPath::new(parser_path)), code, source_manager)
        .map_err(|e| anyhow!("failed to parse module {parser_path}: {e}"))
}

/// Parses all `openzeppelin::auth` modules with the given source manager.
fn parse_auth_modules(source_manager: Arc<dyn SourceManagerSync>) -> Result<Vec<Module>> {
    let mut modules = Vec::new();

    for path in collect_all_masm_files(&auth_dir())? {
        let lib_path = openzeppelin_library_path(&path, &masm_root())?;
        let code = fs::read_to_string(&path)
            .map_err(|e| anyhow!("failed to read {}: {e}", path.display()))?;
        modules.push(*parse_module(&lib_path, code, source_manager.clone())?);
    }

    Ok(modules)
}

/// Assembles a single-root library with the `openzeppelin::auth` modules available as support.
fn assemble_with_auth_support(
    package_name: &str,
    root_path: &str,
    code: String,
) -> Result<Library> {
    let source_manager: Arc<dyn SourceManagerSync> = Arc::new(DefaultSourceManager::default());
    let assembler = kernel_assembler(source_manager.clone())?;

    let root = parse_module(root_path, code, source_manager.clone())?;
    let support = parse_auth_modules(source_manager)?;

    let library = assembler
        .assemble_library(package_name, root, support)
        .map_err(|e| anyhow!("failed to assemble {package_name} library: {e:?}"))?;

    Ok(*library)
}

/// Builds the reusable OpenZeppelin auth library from canonical MASM sources.
fn build_openzeppelin_library() -> Result<Library> {
    let source_manager: Arc<dyn SourceManagerSync> = Arc::new(DefaultSourceManager::default());
    let assembler = kernel_assembler(source_manager.clone())?;

    let modules = parse_auth_modules(source_manager)?;
    let mut modules = modules.into_iter();
    let root = modules
        .next()
        .ok_or_else(|| anyhow!("no MASM modules found under {}", auth_dir().display()))?;

    let library = assembler
        .assemble_library("openzeppelin", root, modules)
        .map_err(|e| anyhow!("failed to assemble openzeppelin library: {e:?}"))?;

    Ok(*library)
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

/// Build AccountComponent from masm/account_components/auth/multisig_guardian.masm.
pub fn build_multisig_guardian_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(
        &account_components_auth_dir().join("multisig_guardian.masm"),
        slots,
    )
}

/// Build AccountComponent from masm/account_components/auth/multisig_guardian_ecdsa.masm.
pub fn build_multisig_guardian_ecdsa_component(
    slots: Vec<StorageSlot>,
) -> Result<AccountComponent> {
    compile_component(
        &account_components_auth_dir().join("multisig_guardian_ecdsa.masm"),
        slots,
    )
}

/// Build AccountComponent from masm/auth/guardian.masm.
/// This component provides Guardian signature verification.
///
/// Storage layout (2 slots):
/// - Slot 0: GUARDIAN selector [selector, 0, 0, 0] where selector=1 means ON, 0 means OFF
/// - Slot 1: GUARDIAN public key map
pub fn build_guardian_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&auth_dir().join("guardian.masm"), slots)
}

/// Build AccountComponent from masm/auth/guardian_ecdsa.masm.
pub fn build_guardian_ecdsa_component(slots: Vec<StorageSlot>) -> Result<AccountComponent> {
    compile_component(&auth_dir().join("guardian_ecdsa.masm"), slots)
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
    let source_manager: Arc<dyn SourceManagerSync> = Arc::new(DefaultSourceManager::default());
    let assembler = TransactionKernel::assembler_with_source_manager(source_manager.clone());
    let module = parse_module(library_path, account_code, source_manager)?;
    let library = assembler
        .assemble_library(library_path, module, None::<Box<Module>>)
        .map_err(|e| anyhow!("failed to assemble {library_path} library: {e:?}"))?;
    Ok(*library)
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
    assemble_with_auth_support("oz_multisig", "oz_multisig::multisig", code)
}

/// Builds an ECDSA multisig library for use in transaction scripts.
/// The procedures are accessible via `use oz_multisig::multisig` and `call.multisig::procedure_name` syntax.
pub fn get_multisig_ecdsa_library() -> Result<Library> {
    let path = auth_dir().join("multisig_ecdsa.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;
    assemble_with_auth_support("oz_multisig", "oz_multisig::multisig", code)
}

/// Builds a library for GUARDIAN procedures for use in transaction scripts.
/// The procedures are accessible via `use oz_guardian::guardian` and `call.guardian::procedure_name` syntax.
pub fn get_guardian_library() -> Result<Library> {
    let path = auth_dir().join("guardian.masm");
    let code = fs::read_to_string(&path).map_err(|e| anyhow!("failed to read {path:?}: {e}"))?;
    assemble_with_auth_support("oz_guardian", "oz_guardian::guardian", code)
}
