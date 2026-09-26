//! The explicit, bounded local library shared with the Android IME process.

use crate::community::resource::{CommunityResource, CommunityResourceKind};
use crate::file_lock;
use serde_json::from_slice;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

const MAXIMUM_BYTES: u64 = 4_000_000;
const MAXIMUM_ITEMS: usize = 50;

#[derive(Debug, Error)]
pub enum CommunityResourceLibraryError {
    #[error("community resource library storage failed")]
    Io(#[from] std::io::Error),
    #[error("community resource library is malformed")]
    Json(#[from] serde_json::Error),
    #[error("community resource library is invalid")]
    Invalid,
}

#[derive(Clone, Debug)]
pub struct CommunityResourceLibraryStore {
    file: PathBuf,
}

impl CommunityResourceLibraryStore {
    pub fn new(file: impl AsRef<Path>) -> Self {
        Self {
            file: file.as_ref().to_path_buf(),
        }
    }

    pub fn load(&self) -> Result<Vec<CommunityResource>, CommunityResourceLibraryError> {
        let _lock = self.lock()?;
        self.read_locked()
    }

    pub fn save_reply(&self, item: CommunityResource) -> Result<(), CommunityResourceLibraryError> {
        if item.kind != CommunityResourceKind::Reply
            || item.id == Uuid::nil()
            || !item.content.entries.is_empty()
            || item.content.prompt.as_deref().is_none_or(str::is_empty)
        {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let _lock = self.lock()?;
        let mut items = self.read_locked()?;
        if let Some(existing) = items.iter_mut().find(|value| value.id == item.id) {
            *existing = item;
        } else {
            if items.len() >= MAXIMUM_ITEMS {
                return Err(CommunityResourceLibraryError::Invalid);
            }
            items.push(item);
        }
        self.write_locked(&items)
    }

    pub fn remove(&self, id: Uuid) -> Result<(), CommunityResourceLibraryError> {
        let _lock = self.lock()?;
        let mut items = self.read_locked()?;
        items.retain(|item| item.id != id);
        self.write_locked(&items)
    }

    fn lock(&self) -> Result<File, CommunityResourceLibraryError> {
        let Some(parent) = self.file.parent() else {
            return Err(CommunityResourceLibraryError::Invalid);
        };
        fs::create_dir_all(parent)?;
        if !fs::symlink_metadata(parent)?.file_type().is_dir() {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let lock_path = self.file.with_extension("json.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn read_locked(&self) -> Result<Vec<CommunityResource>, CommunityResourceLibraryError> {
        let metadata = match fs::symlink_metadata(&self.file) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_BYTES {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(&self.file)?
            .take(MAXIMUM_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_BYTES {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let items: Vec<CommunityResource> = from_slice(&bytes)?;
        if items.len() > MAXIMUM_ITEMS
            || items.iter().any(|item| {
                item.id == Uuid::nil()
                    || item.kind != CommunityResourceKind::Reply
                    || !item.content.entries.is_empty()
                    || item.content.prompt.as_deref().is_none_or(str::is_empty)
            })
        {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let mut ids = std::collections::BTreeSet::new();
        if items.iter().any(|item| !ids.insert(item.id)) {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        Ok(items)
    }

    fn write_locked(
        &self,
        items: &[CommunityResource],
    ) -> Result<(), CommunityResourceLibraryError> {
        let Some(parent) = self.file.parent() else {
            return Err(CommunityResourceLibraryError::Invalid);
        };
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(items)?;
        if bytes.len() as u64 > MAXIMUM_BYTES {
            return Err(CommunityResourceLibraryError::Invalid);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.file).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::resource::{CommunityResourceContent, SharedWord};

    fn reply() -> CommunityResource {
        CommunityResource {
            id: Uuid::parse_str("10000000-0000-4000-8000-000000000001").unwrap(),
            kind: CommunityResourceKind::Reply,
            name: "礼貌回复".into(),
            description: "公开说明".into(),
            author: "示例作者".into(),
            content: CommunityResourceContent {
                entries: Vec::new(),
                prompt: Some("请礼貌回复。".into()),
            },
            revision: 1,
            saves: 0,
            saved: true,
            owned: false,
            rating_count: 0,
            rating_average: 0.0,
            my_rating: 0,
        }
    }

    #[test]
    fn stores_only_reply_resources_and_replaces_by_publication_id() {
        let root = tempfile::tempdir().unwrap();
        let store =
            CommunityResourceLibraryStore::new(root.path().join("files/CommunityLibrary.json"));
        store.save_reply(reply()).unwrap();
        let mut updated = reply();
        updated.revision = 2;
        updated.content.prompt = Some("请更简洁地回复。".into());
        store.save_reply(updated).unwrap();
        let values = store.load().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].revision, 2);
        assert!(store
            .save_reply(CommunityResource {
                content: CommunityResourceContent {
                    entries: vec![SharedWord {
                        kind: crate::cloud::dictionary::DictionaryKind::Quick,
                        code: "x".into(),
                        word: "y".into(),
                        weight: 1
                    }],
                    prompt: None,
                },
                ..reply()
            })
            .is_err());

        let corrupt = CommunityResource {
            id: Uuid::nil(),
            ..reply()
        };
        std::fs::write(
            root.path().join("files/CommunityLibrary.json"),
            serde_json::to_vec(&[corrupt]).unwrap(),
        )
        .unwrap();
        assert!(store.load().is_err(), "nil publication IDs are not usable");
    }
}
