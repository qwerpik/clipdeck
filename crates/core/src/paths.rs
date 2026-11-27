//! Platform directories and atomic writes.

use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{Error, Result};

/// Where Clipdeck keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Settings directory (`~/.config/clipdeck`, `%APPDATA%\Clipdeck\config`).
    pub config_dir: PathBuf,
    /// Data directory (library, cache, logs).
    pub data_dir: PathBuf,
}

impl Paths {
    /// Resolve platform directories; `CLIPDECK_HOME` overrides both (portable mode / tests).
    pub fn resolve() -> Result<Paths> {
        if let Some(home) = std::env::var_os("CLIPDECK_HOME") {
            let home = PathBuf::from(home);
            return Ok(Paths {
                config_dir: home.join("config"),
                data_dir: home.join("data"),
            });
        }
        // Portable mode: a `portable` file next to the executable keeps everything beside it.
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
            && dir.join("portable").exists()
        {
            return Ok(Paths {
                config_dir: dir.join("config"),
                data_dir: dir.join("data"),
            });
        }
        let dirs = ProjectDirs::from("", "", "Clipdeck").ok_or(Error::NoDirs)?;
        Ok(Paths {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    /// `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    /// `library.toml`.
    pub fn library_file(&self) -> PathBuf {
        self.data_dir.join("library.toml")
    }
    /// Log directory.
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    /// Default folder for recordings: `~/Music/Clipdeck` (`Music\Clipdeck` on Windows), or
    /// `<data dir>/recordings` in portable mode or when there is no music folder.
    pub fn recordings_dir(&self) -> PathBuf {
        let portable = std::env::var_os("CLIPDECK_HOME").is_some()
            || std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join("portable").exists()))
                .unwrap_or(false);
        if !portable
            && let Some(music) =
                directories::UserDirs::new().and_then(|u| u.audio_dir().map(Path::to_path_buf))
        {
            return music.join("Clipdeck");
        }
        self.data_dir.join("recordings")
    }
    /// Backups of the library.
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    /// Create the directories.
    pub fn ensure(&self) -> Result<()> {
        for d in [&self.config_dir, &self.data_dir] {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        Ok(())
    }
}

/// Write `bytes` to `path` via a temporary file and rename, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
        f.sync_all().map_err(|e| Error::io(&tmp, e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}
