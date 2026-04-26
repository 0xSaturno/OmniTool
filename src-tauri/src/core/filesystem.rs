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

/// Get path to the projects directory (`<exe_dir>/projects/`), creating it if needed.
pub fn projects_dir() -> Result<PathBuf> {
    let dir = app_dir()?.join("projects");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        debug!("Created projects directory: {}", dir.display());
    }
    Ok(dir)
}

/// Get path to `hashes` file
pub fn hashes_path() -> Result<PathBuf> {
    let path = app_dir()?.join("hashes");
    debug!("hashes_path: {}", path.display());
    Ok(path)
}
