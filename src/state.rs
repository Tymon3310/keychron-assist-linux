use crate::model::GlobalJson;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::sync::RwLock;

pub struct AppState {
    pub root: PathBuf,
    pub config: RwLock<GlobalJson>,
    pub language: RwLock<String>,
    pub translations: RwLock<serde_json::Value>,
}

impl AppState {
    pub fn load() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("Cannot resolve home directory")?;
        let root = home.join(".Keychron_Screen");
        fs::create_dir_all(&root).map_err(|e| format!("create config directory: {e}"))?;
        Ok(Self {
            config: RwLock::new(read_json(&root.join("assist.json")).unwrap_or_default()),
            language: RwLock::new(
                read_json(&root.join("language.json")).unwrap_or_else(|_| "en-US".into()),
            ),
            translations: RwLock::new(
                read_json(&root.join("i18n_cache.json")).unwrap_or_else(|_| serde_json::json!({})),
            ),
            root,
        })
    }

    pub async fn save_config(&self) -> Result<(), String> {
        write_json(&self.root.join("assist.json"), &*self.config.read().await)
            .map_err(|e| e.to_string())
    }
    pub async fn save_translations(&self) -> Result<(), String> {
        write_json(
            &self.root.join("i18n_cache.json"),
            &*self.translations.read().await,
        )
        .map_err(|e| e.to_string())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, data)?;
    fs::rename(temp, path)
}
