//! Crash-recoverable touch-keyboard skin trials independent of any UI host.
//! Source: MSIME-Apple@9ca823ab40018ced3cb71812503dbc3b94615ac0
//! (`KeyboardSkinTrial.swift`, `KeyboardSkinTrialTests.swift`).

use crate::file_lock;
use crate::preferences::{
    PreferencesError, PreferencesSnapshot, PreferencesStore, TouchKeyboardSkin,
    TouchKeyboardSkinDesign,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

const MAXIMUM_RECORD_BYTES: u64 = 2_000_000;
const MAXIMUM_NAME_GRAPHEMES: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeyboardSkinTrial {
    pub id: Uuid,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrialRecord {
    id: Uuid,
    name: String,
    previous_skin: TouchKeyboardSkin,
    previous_design: TouchKeyboardSkinDesign,
    design: TouchKeyboardSkinDesign,
}

#[derive(Debug, Error)]
pub enum KeyboardSkinTrialError {
    #[error("keyboard skin trial storage failed")]
    Io(#[from] std::io::Error),
    #[error("keyboard skin trial is malformed")]
    Json(#[from] serde_json::Error),
    #[error("keyboard skin trial is invalid")]
    Invalid,
    #[error("keyboard skin trial preferences failed")]
    Preferences(#[from] PreferencesError),
}

#[derive(Clone)]
pub struct KeyboardSkinTrialStore {
    directory: PathBuf,
    preferences: Arc<PreferencesStore>,
}

impl KeyboardSkinTrialStore {
    pub fn new(directory: impl AsRef<Path>, preferences: Arc<PreferencesStore>) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            preferences,
        }
    }

    pub fn begin(
        &self,
        name: &str,
        design: TouchKeyboardSkinDesign,
    ) -> Result<(KeyboardSkinTrial, PreferencesSnapshot), KeyboardSkinTrialError> {
        let _lock = self.lock()?;
        self.restore_locked()?;
        let name = normalized_name(name)?;
        if !design.validate() {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        let design = design.normalized();
        let snapshot = self.preferences.load()?;
        let record = TrialRecord {
            id: Uuid::new_v4(),
            name: name.clone(),
            previous_skin: snapshot.preferences.touch_keyboard_skin,
            previous_design: snapshot.preferences.custom_touch_keyboard_skin.clone(),
            design: design.clone(),
        };
        self.write_record(&record)?;
        let mut preferences = snapshot.preferences;
        preferences.touch_keyboard_skin = TouchKeyboardSkin::Custom;
        preferences.custom_touch_keyboard_skin = design;
        let applied = match self.preferences.save(snapshot.revision, preferences) {
            Ok(applied) => applied,
            Err(error) => {
                let _ = self.remove_record();
                return Err(error.into());
            }
        };
        Ok((
            KeyboardSkinTrial {
                id: record.id,
                name,
            },
            applied,
        ))
    }

    pub fn finish(
        &self,
        id: Uuid,
        keep: bool,
    ) -> Result<PreferencesSnapshot, KeyboardSkinTrialError> {
        let _lock = self.lock()?;
        let Some(record) = self.pending()? else {
            return Ok(self.preferences.load()?);
        };
        if record.id != id {
            return Ok(self.preferences.load()?);
        }
        if keep {
            self.remove_record()?;
            return Ok(self.preferences.load()?);
        }
        self.restore_record(record)
    }

    pub fn restore_pending(&self) -> Result<PreferencesSnapshot, KeyboardSkinTrialError> {
        let _lock = self.lock()?;
        self.restore_locked()
    }

    fn restore_locked(&self) -> Result<PreferencesSnapshot, KeyboardSkinTrialError> {
        match self.pending()? {
            Some(record) => self.restore_record(record),
            None => Ok(self.preferences.load()?),
        }
    }

    fn restore_record(
        &self,
        record: TrialRecord,
    ) -> Result<PreferencesSnapshot, KeyboardSkinTrialError> {
        let snapshot = self.preferences.load()?;
        if snapshot.preferences.touch_keyboard_skin != TouchKeyboardSkin::Custom
            || snapshot.preferences.custom_touch_keyboard_skin != record.design
        {
            self.remove_record()?;
            return Ok(snapshot);
        }
        let mut preferences = snapshot.preferences;
        preferences.touch_keyboard_skin = record.previous_skin;
        preferences.custom_touch_keyboard_skin = record.previous_design;
        let restored = self.preferences.save(snapshot.revision, preferences)?;
        self.remove_record()?;
        Ok(restored)
    }

    fn lock(&self) -> Result<File, KeyboardSkinTrialError> {
        fs::create_dir_all(&self.directory)?;
        if !fs::symlink_metadata(&self.directory)?.file_type().is_dir() {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("KeyboardSkinTrial.lock"))?;
        file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn path(&self) -> PathBuf {
        self.directory.join("KeyboardSkinTrial.json")
    }

    fn pending(&self) -> Result<Option<TrialRecord>, KeyboardSkinTrialError> {
        let metadata = match fs::symlink_metadata(self.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_RECORD_BYTES {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(self.path())?
            .take(MAXIMUM_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_RECORD_BYTES {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        let record: TrialRecord = serde_json::from_slice(&bytes)?;
        if record.id.is_nil()
            || normalized_name(&record.name)? != record.name
            || !record.design.validate()
            || !record.previous_design.validate()
        {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        Ok(Some(record))
    }

    fn write_record(&self, record: &TrialRecord) -> Result<(), KeyboardSkinTrialError> {
        let bytes = serde_json::to_vec(record)?;
        if bytes.len() as u64 > MAXIMUM_RECORD_BYTES {
            return Err(KeyboardSkinTrialError::Invalid);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path())
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn remove_record(&self) -> Result<(), KeyboardSkinTrialError> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn normalized_name(name: &str) -> Result<String, KeyboardSkinTrialError> {
    let name = name.trim();
    if name.is_empty()
        || name.graphemes(true).count() > MAXIMUM_NAME_GRAPHEMES
        || name.chars().any(char::is_control)
    {
        return Err(KeyboardSkinTrialError::Invalid);
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (
        tempfile::TempDir,
        Arc<PreferencesStore>,
        KeyboardSkinTrialStore,
    ) {
        let root = tempfile::tempdir().unwrap();
        let preferences = Arc::new(PreferencesStore::new(root.path()));
        let trials = KeyboardSkinTrialStore::new(root.path(), Arc::clone(&preferences));
        (root, preferences, trials)
    }

    #[test]
    fn trial_restores_exact_previous_design_or_keeps_download() {
        let (_root, preferences, trials) = stores();
        let original = preferences.load().unwrap();
        let design = TouchKeyboardSkinDesign {
            background: 0x123456,
            ..TouchKeyboardSkinDesign::default()
        };
        let (trial, applied) = trials.begin("社区皮肤", design.clone()).unwrap();
        assert_eq!(
            applied.preferences.touch_keyboard_skin,
            TouchKeyboardSkin::Custom
        );
        assert_eq!(applied.preferences.custom_touch_keyboard_skin, design);
        let restored = trials.finish(trial.id, false).unwrap();
        assert_eq!(restored.preferences, original.preferences);

        let (trial, _) = trials.begin("保留皮肤", design.clone()).unwrap();
        let kept = trials.finish(trial.id, true).unwrap();
        assert_eq!(
            kept.preferences.touch_keyboard_skin,
            TouchKeyboardSkin::Custom
        );
        assert_eq!(kept.preferences.custom_touch_keyboard_skin, design);
    }

    #[test]
    fn restart_recovers_but_does_not_undo_a_later_explicit_selection() {
        let (root, preferences, trials) = stores();
        let design = TouchKeyboardSkinDesign {
            background: 0x654321,
            ..TouchKeyboardSkinDesign::default()
        };
        trials.begin("待恢复", design.clone()).unwrap();
        let recovered = KeyboardSkinTrialStore::new(root.path(), Arc::clone(&preferences));
        let restored = recovered.restore_pending().unwrap();
        assert_ne!(restored.preferences.custom_touch_keyboard_skin, design);

        let (trial, applied) = trials.begin("不覆盖后续选择", design).unwrap();
        let mut later = applied.preferences;
        later.touch_keyboard_skin = TouchKeyboardSkin::Ocean;
        preferences.save(applied.revision, later).unwrap();
        let current = trials.finish(trial.id, false).unwrap();
        assert_eq!(
            current.preferences.touch_keyboard_skin,
            TouchKeyboardSkin::Ocean
        );
    }

    #[test]
    fn malformed_pending_record_is_preserved() {
        let (root, _preferences, trials) = stores();
        fs::write(root.path().join("KeyboardSkinTrial.json"), b"not json").unwrap();
        let before = fs::read(root.path().join("KeyboardSkinTrial.json")).unwrap();
        assert!(matches!(
            trials.restore_pending(),
            Err(KeyboardSkinTrialError::Json(_))
        ));
        assert_eq!(
            fs::read(root.path().join("KeyboardSkinTrial.json")).unwrap(),
            before
        );
    }

    #[test]
    fn a_pending_record_with_a_nil_id_is_rejected() {
        let (root, _preferences, trials) = stores();
        let record = TrialRecord {
            id: Uuid::nil(),
            name: "合成试用".into(),
            previous_skin: TouchKeyboardSkin::Forest,
            previous_design: TouchKeyboardSkinDesign::default(),
            design: TouchKeyboardSkinDesign::default(),
        };
        fs::write(
            root.path().join("KeyboardSkinTrial.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            trials.restore_pending(),
            Err(KeyboardSkinTrialError::Invalid)
        ));
    }
}
