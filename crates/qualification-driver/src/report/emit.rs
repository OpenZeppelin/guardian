use std::path::Path;

use super::RunResult;

pub fn write(result: &RunResult, directory: &Path) -> anyhow::Result<std::path::PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{}.json", result.run_id));
    let rendered = serde_json::to_string_pretty(result)?;
    std::fs::write(&path, rendered)?;
    Ok(path)
}

pub fn read(path: &Path) -> anyhow::Result<RunResult> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}
