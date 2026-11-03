//! Where the app keeps its files, and how it writes them safely.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use directories::ProjectDirs;

const APP_NAME: &str = "osu-offset-suggester";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP_NAME).context("could not determine the home directory")
}

/// Linux: `~/.config/osu-offset-suggester/settings.toml`
/// Windows: `%APPDATA%\osu-offset-suggester\config\settings.toml`
pub fn settings_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("settings.toml"))
}

/// Linux: `~/.local/share/osu-offset-suggester/history.json`
/// Windows: `%APPDATA%\osu-offset-suggester\data\history.json`
pub fn history_path() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("history.json"))
}

/// Write to a temp file next to `path`, then rename it over `path`.
/// A crash mid-write leaves the old file intact instead of a half-written one.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let temp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp)
            .with_context(|| format!("failed to create {}", temp.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp.display()))?;
    }
    fs::rename(&temp, path).with_context(|| format!("failed to replace {}", path.display()))
}

/// Read a file, treating "does not exist" as `None` rather than an error.
pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}
