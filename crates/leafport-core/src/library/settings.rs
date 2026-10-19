//! User settings stored in `settings.yaml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{LibraryError, paths, write_atomic};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Settings {
    #[serde(default)]
    pub books_dir: Option<PathBuf>,
    #[serde(default)]
    pub theme: Theme,
}

impl Settings {
    pub fn books_dir_or_default(&self) -> Option<PathBuf> {
        self.books_dir.clone().or_else(paths::default_books_dir)
    }

    pub fn load(path: &Path) -> Result<Settings, LibraryError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_yaml_ng::from_str(&text).map_err(|e| LibraryError::Yaml(e.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), LibraryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_yaml_ng::to_string(self).map_err(|e| LibraryError::Yaml(e.to_string()))?;
        write_atomic(path, text.as_bytes())
    }
}
