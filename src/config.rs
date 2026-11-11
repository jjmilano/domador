use crate::model::Config;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOMADOR_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let base = std::env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string()));
    PathBuf::from(base).join("Domador").join("config.json")
}

pub fn load(path: &Path) -> Config {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(config) => config,
            Err(err) => {
                let bad = path.with_extension("bad.json");
                let _ = fs::rename(path, &bad);
                eprintln!("config inválida ({err}); usando valores por defecto");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("no pude crear {parent:?}: {e}"))?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| format!("no pude escribir config: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("no pude reemplazar config: {e}"))?;
    Ok(())
}
