use std::path::PathBuf;

use log::debug;

use crate::core::error::{Result, ToolkitError};

/// Get the directory where the app executable lives.
pub fn app_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| ToolkitError::Parse("could not determine exe directory".into()))?;
    debug!("app_dir: {}", dir.display());
    Ok(dir.to_path_buf())
}

/// Path of the optional external `hashes` list, `<exe_dir>/hashes`.
///
/// The hash-file *commands* went away with the dag name table, which names
/// 169,245 of 169,245 assets on the shipped build. The file is still read when
/// it is there, because it also covers a few assets the dag omits, and the
/// level-editor harnesses take it as an argument default.
pub fn hashes_path() -> Result<PathBuf> {
    let path = app_dir()?.join("hashes");
    debug!("hashes_path: {}", path.display());
    Ok(path)
}

/// Get path to the projects directory (`<exe_dir>/projects/`), creating it if needed.
pub fn projects_dir() -> Result<PathBuf> {
    let dir = app_dir()?.join("projects");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        debug!("Created projects directory: {}", dir.display());
    }
    Ok(dir)
}
