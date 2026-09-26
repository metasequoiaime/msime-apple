//! Bounded named custom keyboard skins, stored separately from hot-path preferences.

use crate::file_lock;
use crate::preferences::TouchKeyboardSkinDesign;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

const MAXIMUM_ITEMS: usize = 12;
const MAXIMUM_BYTES: u64 = 9_000_000;
const MAXIMUM_NAME_GRAPHEMES: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTouchKeyboardSkin {
    pub id: Uuid,
    pub name: String,
    pub design: TouchKeyboardSkinDesign,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum CustomSkinLibraryAction {
    Create {
        name: String,
        design: TouchKeyboardSkinDesign,
    },
    Rename {
        id: Uuid,
        name: String,
    },
    Update {
        id: Uuid,
        design: TouchKeyboardSkinDesign,
    },
    Delete {
        id: Uuid,
    },
}

#[derive(Debug, Error)]
pub enum CustomSkinLibraryError {
    #[error("custom skin library storage failed")]
    Io(#[from] std::io::Error),
    #[error("custom skin library is malformed")]
    Json(#[from] serde_json::Error),
    #[error("custom skin library is invalid")]
    Invalid,
    #[error("custom skin library is full")]
    Full,
    #[error("custom skin name is invalid")]
    InvalidName,
    #[error("custom skin name already exists")]
    DuplicateName,
    #[error("custom skin was not found")]
    NotFound,
}

#[derive(Clone, Debug)]
pub struct CustomSkinLibraryStore {
    directory: PathBuf,
}

impl CustomSkinLibraryStore {
    pub fn new(state_directory: impl AsRef<Path>) -> Self {
        Self {
            directory: state_directory.as_ref().join("CustomSkins"),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join("library.json")
    }

    pub fn load(&self) -> Result<Vec<SavedTouchKeyboardSkin>, CustomSkinLibraryError> {
        if !self.path().exists() {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        self.read_locked()
    }

    pub fn mutate(
        &self,
        action: CustomSkinLibraryAction,
    ) -> Result<Vec<SavedTouchKeyboardSkin>, CustomSkinLibraryError> {
        let _lock = self.lock()?;
        let mut items = self.read_locked()?;
        match action {
            CustomSkinLibraryAction::Create { name, design } => {
                if items.len() >= MAXIMUM_ITEMS {
                    return Err(CustomSkinLibraryError::Full);
                }
                let name = normalized_name(&name)?;
                reject_duplicate_name(&items, None, &name)?;
                items.push(SavedTouchKeyboardSkin {
                    id: Uuid::new_v4(),
                    name,
                    design: design.normalized(),
                });
            }
            CustomSkinLibraryAction::Rename { id, name } => {
                let name = normalized_name(&name)?;
                reject_duplicate_name(&items, Some(id), &name)?;
                let item = items
                    .iter_mut()
                    .find(|item| item.id == id)
                    .ok_or(CustomSkinLibraryError::NotFound)?;
                item.name = name;
            }
            CustomSkinLibraryAction::Update { id, design } => {
                let item = items
                    .iter_mut()
                    .find(|item| item.id == id)
                    .ok_or(CustomSkinLibraryError::NotFound)?;
                item.design = design.normalized();
            }
            CustomSkinLibraryAction::Delete { id } => {
                let previous = items.len();
                items.retain(|item| item.id != id);
                if items.len() == previous {
                    return Err(CustomSkinLibraryError::NotFound);
                }
            }
        }
        self.write_locked(&items)?;
        Ok(items)
    }

    /// Import a community download under its stable publication id. Repeated
    /// downloads refresh the design without renaming the user's local copy.
    pub fn import_download(
        &self,
        id: Uuid,
        name: &str,
        design: TouchKeyboardSkinDesign,
    ) -> Result<SavedTouchKeyboardSkin, CustomSkinLibraryError> {
        if id.is_nil() {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let _lock = self.lock()?;
        let mut items = self.read_locked()?;
        if let Some(item) = items.iter_mut().find(|item| item.id == id) {
            item.design = design.normalized();
            let imported = item.clone();
            self.write_locked(&items)?;
            return Ok(imported);
        }
        if items.len() >= MAXIMUM_ITEMS {
            return Err(CustomSkinLibraryError::Full);
        }
        let name = unique_import_name(&items, normalized_name(name)?)?;
        let imported = SavedTouchKeyboardSkin {
            id,
            name,
            design: design.normalized(),
        };
        items.push(imported.clone());
        self.write_locked(&items)?;
        Ok(imported)
    }

    fn lock(&self) -> Result<File, CustomSkinLibraryError> {
        fs::create_dir_all(&self.directory)?;
        if !fs::symlink_metadata(&self.directory)?.file_type().is_dir() {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("library.lock"))?;
        file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn read_locked(&self) -> Result<Vec<SavedTouchKeyboardSkin>, CustomSkinLibraryError> {
        let path = self.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_BYTES {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)?
            .take(MAXIMUM_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_BYTES {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let mut items: Vec<SavedTouchKeyboardSkin> = serde_json::from_slice(&bytes)?;
        if items.len() > MAXIMUM_ITEMS {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for item in &mut items {
            if item.id.is_nil() {
                return Err(CustomSkinLibraryError::Invalid);
            }
            item.name = normalized_name(&item.name)?;
            item.design = item.design.clone().normalized();
            if !ids.insert(item.id) || !names.insert(item.name.clone()) {
                return Err(CustomSkinLibraryError::Invalid);
            }
        }
        Ok(items)
    }

    fn write_locked(&self, items: &[SavedTouchKeyboardSkin]) -> Result<(), CustomSkinLibraryError> {
        let bytes = serde_json::to_vec_pretty(items)?;
        if bytes.len() as u64 > MAXIMUM_BYTES {
            return Err(CustomSkinLibraryError::Invalid);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path())
            .map_err(|error| error.error)?;
        Ok(())
    }
}

fn normalized_name(name: &str) -> Result<String, CustomSkinLibraryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CustomSkinLibraryError::InvalidName);
    }
    Ok(name.graphemes(true).take(MAXIMUM_NAME_GRAPHEMES).collect())
}

fn reject_duplicate_name(
    items: &[SavedTouchKeyboardSkin],
    excluding: Option<Uuid>,
    name: &str,
) -> Result<(), CustomSkinLibraryError> {
    if items
        .iter()
        .any(|item| Some(item.id) != excluding && item.name == name)
    {
        return Err(CustomSkinLibraryError::DuplicateName);
    }
    Ok(())
}

fn unique_import_name(
    items: &[SavedTouchKeyboardSkin],
    name: String,
) -> Result<String, CustomSkinLibraryError> {
    if !items.iter().any(|item| item.name == name) {
        return Ok(name);
    }
    for index in 2..=MAXIMUM_ITEMS + 1 {
        let suffix = format!(" ({index})");
        let prefix_length = MAXIMUM_NAME_GRAPHEMES.saturating_sub(suffix.graphemes(true).count());
        let candidate = format!(
            "{}{}",
            name.graphemes(true).take(prefix_length).collect::<String>(),
            suffix
        );
        if !items.iter().any(|item| item.name == candidate) {
            return Ok(candidate);
        }
    }
    Err(CustomSkinLibraryError::DuplicateName)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(name: &str, design: TouchKeyboardSkinDesign) -> CustomSkinLibraryAction {
        CustomSkinLibraryAction::Create {
            name: name.to_owned(),
            design,
        }
    }

    #[test]
    fn apple_library_round_trips_mutations_and_bounds() {
        let root = tempfile::tempdir().unwrap();
        let store = CustomSkinLibraryStore::new(root.path());
        assert!(store.load().unwrap().is_empty());
        assert!(!store.path().exists());

        let mut design = TouchKeyboardSkinDesign {
            background: 0xFF151022,
            corner_radius: 99.0,
            pattern: 99,
            pattern_opacity: Some(2.0),
            photo_position: Some(-4.0),
            ..TouchKeyboardSkinDesign::default()
        };
        let created = store
            .mutate(create("  我的夜色  ", design.clone()))
            .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].name, "我的夜色");
        assert_eq!(created[0].design.background, 0x151022);
        assert_eq!(created[0].design.corner_radius, 20.0);
        assert_eq!(created[0].design.pattern, 0);
        assert_eq!(created[0].design.pattern_opacity, Some(0.5));
        assert_eq!(created[0].design.photo_position, Some(0.0));
        let id = created[0].id;

        assert!(matches!(
            store.mutate(create("我的夜色", TouchKeyboardSkinDesign::default())),
            Err(CustomSkinLibraryError::DuplicateName)
        ));
        let renamed = store
            .mutate(CustomSkinLibraryAction::Rename {
                id,
                name: "晨雾".into(),
            })
            .unwrap();
        assert_eq!(renamed[0].name, "晨雾");
        design.key_shape = Some(crate::preferences::TouchSkinKeyShape::Pebble);
        let updated = store
            .mutate(CustomSkinLibraryAction::Update { id, design })
            .unwrap();
        assert_eq!(
            updated[0].design.key_shape,
            Some(crate::preferences::TouchSkinKeyShape::Pebble)
        );
        assert_eq!(
            serde_json::to_value(&updated[0]).unwrap()["design"]["keyShape"],
            "pebble"
        );
        assert!(store
            .mutate(CustomSkinLibraryAction::Delete { id })
            .unwrap()
            .is_empty());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn community_downloads_use_stable_ids_and_collision_safe_names() {
        let root = tempfile::tempdir().unwrap();
        let store = CustomSkinLibraryStore::new(root.path());
        store
            .mutate(create("星空", TouchKeyboardSkinDesign::default()))
            .unwrap();
        let publication = Uuid::parse_str("10000000-0000-4000-8000-000000000042").unwrap();
        let first = store
            .import_download(
                publication,
                "星空",
                TouchKeyboardSkinDesign {
                    background: 0x112233,
                    ..TouchKeyboardSkinDesign::default()
                },
            )
            .unwrap();
        assert_eq!(first.id, publication);
        assert_eq!(first.name, "星空 (2)");
        let refreshed = store
            .import_download(
                publication,
                "服务端新名称",
                TouchKeyboardSkinDesign {
                    background: 0x445566,
                    ..TouchKeyboardSkinDesign::default()
                },
            )
            .unwrap();
        assert_eq!(refreshed.name, "星空 (2)");
        assert_eq!(refreshed.design.background, 0x445566);
        assert_eq!(store.load().unwrap().len(), 2);
        assert!(matches!(
            store.import_download(Uuid::nil(), "无效作品", TouchKeyboardSkinDesign::default()),
            Err(CustomSkinLibraryError::Invalid)
        ));
    }

    #[test]
    fn failures_preserve_the_library_and_twelve_item_limit() {
        let root = tempfile::tempdir().unwrap();
        let store = CustomSkinLibraryStore::new(root.path());
        for index in 0..MAXIMUM_ITEMS {
            store
                .mutate(create(
                    &format!("皮肤 {}", index + 1),
                    TouchKeyboardSkinDesign::default(),
                ))
                .unwrap();
        }
        let before = fs::read(store.path()).unwrap();
        assert!(matches!(
            store.mutate(create("超额", TouchKeyboardSkinDesign::default())),
            Err(CustomSkinLibraryError::Full)
        ));
        assert!(matches!(
            store.mutate(CustomSkinLibraryAction::Delete { id: Uuid::nil() }),
            Err(CustomSkinLibraryError::NotFound)
        ));
        assert_eq!(fs::read(store.path()).unwrap(), before);

        fs::write(store.path(), b"not json").unwrap();
        let corrupt = fs::read(store.path()).unwrap();
        assert!(matches!(store.load(), Err(CustomSkinLibraryError::Json(_))));
        assert!(store
            .mutate(create("不会覆盖", TouchKeyboardSkinDesign::default()))
            .is_err());
        assert_eq!(fs::read(store.path()).unwrap(), corrupt);

        let oversized = (0..=MAXIMUM_ITEMS)
            .map(|index| SavedTouchKeyboardSkin {
                id: Uuid::new_v4(),
                name: format!("外部皮肤 {}", index + 1),
                design: TouchKeyboardSkinDesign::default(),
            })
            .collect::<Vec<_>>();
        fs::write(store.path(), serde_json::to_vec(&oversized).unwrap()).unwrap();
        let before = fs::read(store.path()).unwrap();
        assert!(matches!(store.load(), Err(CustomSkinLibraryError::Invalid)));
        assert!(store
            .mutate(CustomSkinLibraryAction::Delete {
                id: oversized[0].id,
            })
            .is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);

        let nil_id = vec![SavedTouchKeyboardSkin {
            id: Uuid::nil(),
            name: "损坏皮肤".into(),
            design: TouchKeyboardSkinDesign::default(),
        }];
        fs::write(store.path(), serde_json::to_vec(&nil_id).unwrap()).unwrap();
        assert!(matches!(store.load(), Err(CustomSkinLibraryError::Invalid)));
    }

    #[test]
    fn concurrent_writers_merge_against_the_latest_file_and_bound_graphemes() {
        let root = tempfile::tempdir().unwrap();
        let store = CustomSkinLibraryStore::new(root.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers: Vec<_> = ["晨雾", "夜航"]
            .into_iter()
            .map(|name| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .mutate(create(name, TouchKeyboardSkinDesign::default()))
                        .unwrap();
                })
            })
            .collect();
        barrier.wait();
        for writer in writers {
            writer.join().unwrap();
        }
        let items = store.load().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["夜航", "晨雾"])
        );

        let long_name = format!("  {}尾部  ", "👩‍👩‍👧‍👦".repeat(40));
        let created = store
            .mutate(create(&long_name, TouchKeyboardSkinDesign::default()))
            .unwrap();
        let name = &created.last().unwrap().name;
        assert_eq!(name.graphemes(true).count(), MAXIMUM_NAME_GRAPHEMES);
        assert_eq!(name, &"👩‍👩‍👧‍👦".repeat(MAXIMUM_NAME_GRAPHEMES));
    }
}
