//! Workstation presentation preferences, independent of every learner profile.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) struct UiPreferences(Connection);
impl UiPreferences {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS ui_preferences(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
        )?;
        Ok(Self(db))
    }
    pub(crate) fn get(&self) -> Result<Value> {
        let language: Option<String> = self
            .0
            .query_row(
                "SELECT value FROM ui_preferences WHERE key='ui_language'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let language = language
            .filter(|v| ["zh", "en"].contains(&v.as_str()))
            .unwrap_or_else(|| "zh".into());
        Ok(json!({"ui_language":language}))
    }
    pub(crate) fn set_language(&self, language: &str) -> Result<Value> {
        ensure!(["zh", "en"].contains(&language), "无效界面语言");
        self.0.execute("INSERT INTO ui_preferences VALUES('ui_language',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [language])?;
        self.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_language_persists_and_rejects_invalid_values() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("ui.sqlite3");
        let prefs = UiPreferences::open(&path)?;
        assert_eq!(prefs.get()?["ui_language"], "zh");
        prefs.set_language("en")?;
        assert!(prefs.set_language("auto").is_err());
        drop(prefs);
        assert_eq!(UiPreferences::open(&path)?.get()?["ui_language"], "en");
        Ok(())
    }
}
