//! Cross-process, crash-safe handoff for one cloud dictionary snapshot.
//!
//! The queue owns only bounded metadata and a copied NDJSON file. Engine-specific decoding,
//! staging and activation remain in the native host layer.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

const MAXIMUM_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_STATE_BYTES: u64 = 65_536;
const STATE_NAME: &str = "state.json";
const STATE_LOCK_NAME: &str = "state.lock";
const WORKER_LOCK_NAME: &str = "worker.lock";

#[derive(Debug, Error)]
pub enum SnapshotQueueError {
    #[error("snapshot queue unavailable")]
    Unavailable,
    #[error("snapshot queue busy")]
    Busy,
    #[error("invalid snapshot queue")]
    Invalid,
    #[error("snapshot source changed")]
    Conflict,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRequestStatus {
    Queued,
    Preparing,
    Applied,
    Conflict,
    Failed,
    Cancelled,
}

impl SnapshotRequestStatus {
    pub fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Preparing)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    pub id: Uuid,
    pub account_id: String,
    pub cloud_revision: i64,
    pub expected_local_version: String,
    pub file_sha256: String,
    pub status: SnapshotRequestStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotQueueState {
    #[serde(default = "state_version")]
    version: u8,
    pub local_version: Option<String>,
    pub request: Option<SnapshotRequest>,
}

fn state_version() -> u8 {
    1
}

impl SnapshotQueueState {
    fn validate(&self) -> Result<(), SnapshotQueueError> {
        if self.version != 1
            || self
                .local_version
                .as_deref()
                .is_some_and(|value| !valid_local_version(value))
            || self.request.as_ref().is_some_and(|request| {
                request.id.is_nil()
                    || request.account_id.is_empty()
                    || request.account_id.len() > 128
                    || request.cloud_revision < 0
                    || !valid_local_version(&request.expected_local_version)
                    || !valid_digest(&request.file_sha256)
            })
        {
            return Err(SnapshotQueueError::Invalid);
        }
        Ok(())
    }
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn valid_local_version(value: &str) -> bool {
    let mut fields = value.split(':');
    if fields.next() != Some("local-v1") {
        return false;
    }
    let Some(owner) = fields.next() else {
        return false;
    };
    let Some(digest) = fields.next() else {
        return false;
    };
    if fields.next().is_some() || !valid_digest(digest) {
        return false;
    }
    owner == "legacy" || Uuid::parse_str(owner).is_ok_and(|id| id.to_string() == owner)
}

pub fn local_version(generation: Option<&str>, digest: &str) -> Result<String, SnapshotQueueError> {
    let owner = generation.unwrap_or("legacy");
    let value = format!("local-v1:{owner}:{digest}");
    valid_local_version(&value)
        .then_some(value)
        .ok_or(SnapshotQueueError::Invalid)
}

pub fn local_version_digest(value: &str) -> Result<&str, SnapshotQueueError> {
    if !valid_local_version(value) {
        return Err(SnapshotQueueError::Invalid);
    }
    value.rsplit(':').next().ok_or(SnapshotQueueError::Invalid)
}

pub struct SnapshotWorkerLease {
    _file: File,
    owner: PathBuf,
}

pub struct DictionarySnapshotQueue {
    directory: PathBuf,
}

impl DictionarySnapshotQueue {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self, SnapshotQueueError> {
        let directory = directory.into();
        if !directory.is_absolute() {
            return Err(SnapshotQueueError::Invalid);
        }
        Ok(Self { directory })
    }

    fn existing_root(&self) -> Result<Option<PathBuf>, SnapshotQueueError> {
        let metadata = match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SnapshotQueueError::Unavailable),
        };
        if !metadata.file_type().is_dir() {
            return Err(SnapshotQueueError::Invalid);
        }
        self.directory
            .canonicalize()
            .map(Some)
            .map_err(|_| SnapshotQueueError::Unavailable)
    }

    fn root(&self) -> Result<PathBuf, SnapshotQueueError> {
        fs::create_dir_all(&self.directory).map_err(|_| SnapshotQueueError::Unavailable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
                .map_err(|_| SnapshotQueueError::Unavailable)?;
        }
        self.existing_root()?.ok_or(SnapshotQueueError::Unavailable)
    }

    fn state_path(root: &Path) -> PathBuf {
        root.join(STATE_NAME)
    }

    fn lock(&self, name: &str) -> Result<(File, PathBuf), SnapshotQueueError> {
        let root = self.root()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(name))
            .map_err(|_| SnapshotQueueError::Unavailable)?;
        match crate::file_lock::try_exclusive_with_grace(&file) {
            Ok(true) => Ok((file, root)),
            Ok(false) => Err(SnapshotQueueError::Busy),
            Err(_) => Err(SnapshotQueueError::Unavailable),
        }
    }

    fn read_from(root: &Path) -> Result<SnapshotQueueState, SnapshotQueueError> {
        let path = Self::state_path(root);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SnapshotQueueState {
                    version: 1,
                    ..SnapshotQueueState::default()
                });
            }
            Err(_) => return Err(SnapshotQueueError::Unavailable),
        };
        if !metadata.file_type().is_file()
            || metadata.len() == 0
            || metadata.len() > MAXIMUM_STATE_BYTES
        {
            return Err(SnapshotQueueError::Invalid);
        }
        let bytes = fs::read(path).map_err(|_| SnapshotQueueError::Unavailable)?;
        let state: SnapshotQueueState =
            serde_json::from_slice(&bytes).map_err(|_| SnapshotQueueError::Invalid)?;
        state.validate()?;
        Ok(state)
    }

    fn write_to(root: &Path, state: &SnapshotQueueState) -> Result<(), SnapshotQueueError> {
        state.validate()?;
        let bytes = serde_json::to_vec(state).map_err(|_| SnapshotQueueError::Invalid)?;
        if bytes.is_empty() || bytes.len() as u64 > MAXIMUM_STATE_BYTES {
            return Err(SnapshotQueueError::Invalid);
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(root).map_err(|_| SnapshotQueueError::Unavailable)?;
        temporary
            .write_all(&bytes)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|_| SnapshotQueueError::Unavailable)?;
        temporary
            .persist(Self::state_path(root))
            .map(|_| ())
            .map_err(|_| SnapshotQueueError::Unavailable)
    }

    fn update<T>(
        &self,
        action: impl FnOnce(&Path, &mut SnapshotQueueState) -> Result<T, SnapshotQueueError>,
    ) -> Result<T, SnapshotQueueError> {
        let (_lock, root) = self.lock(STATE_LOCK_NAME)?;
        let mut state = Self::read_from(&root)?;
        let result = action(&root, &mut state)?;
        Self::write_to(&root, &state)?;
        Ok(result)
    }

    fn update_wait<T>(
        &self,
        action: impl FnOnce(&Path, &mut SnapshotQueueState) -> Result<T, SnapshotQueueError>,
    ) -> Result<T, SnapshotQueueError> {
        let root = self.root()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(STATE_LOCK_NAME))
            .map_err(|_| SnapshotQueueError::Unavailable)?;
        crate::file_lock::exclusive(&lock).map_err(|_| SnapshotQueueError::Unavailable)?;
        let mut state = Self::read_from(&root)?;
        let result = action(&root, &mut state)?;
        Self::write_to(&root, &state)?;
        Ok(result)
    }

    /// Read-only status inspection does not create the queue directory or state file.
    pub fn read(&self) -> Result<SnapshotQueueState, SnapshotQueueError> {
        let Some(root) = self.existing_root()? else {
            return Ok(SnapshotQueueState {
                version: 1,
                ..SnapshotQueueState::default()
            });
        };
        Self::read_from(&root)
    }

    pub fn file_path(&self, id: Uuid) -> Result<PathBuf, SnapshotQueueError> {
        Ok(self.root()?.join(format!("{id}.ndjson")))
    }

    pub fn publish_local_version(&self, version: &str) -> Result<(), SnapshotQueueError> {
        if !valid_local_version(version) {
            return Err(SnapshotQueueError::Invalid);
        }
        let applied = self.update(|_, state| {
            state.local_version = Some(version.to_owned());
            let owner = version
                .split(':')
                .nth(1)
                .ok_or(SnapshotQueueError::Invalid)?;
            if let Some(request) = state
                .request
                .as_mut()
                // A receipt can arrive after logout or another terminal
                // transition. Do not resurrect a cancelled/failed/conflicted
                // request merely because its generation id appears in the
                // version string.
                .filter(|request| request.status.active() && request.id.to_string() == owner)
            {
                request.status = SnapshotRequestStatus::Applied;
                return Ok(Some(request.id));
            }
            Ok(None)
        })?;
        if let Some(id) = applied {
            self.delete_snapshot(id)?;
        }
        Ok(())
    }

    pub fn enqueue(
        &self,
        source: &Path,
        account_id: &str,
        cloud_revision: i64,
        expected_local_version: &str,
        file_sha256: &str,
    ) -> Result<Uuid, SnapshotQueueError> {
        let source_metadata =
            fs::symlink_metadata(source).map_err(|_| SnapshotQueueError::Invalid)?;
        if !source.is_absolute()
            || !source_metadata.file_type().is_file()
            || account_id.is_empty()
            || account_id.len() > 128
            || cloud_revision < 0
            || !valid_local_version(expected_local_version)
            || !valid_digest(file_sha256)
        {
            return Err(SnapshotQueueError::Invalid);
        }
        let root = self.root()?;
        let mut incoming =
            tempfile::NamedTempFile::new_in(&root).map_err(|_| SnapshotQueueError::Unavailable)?;
        let mut input = File::open(source).map_err(|_| SnapshotQueueError::Unavailable)?;
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 65_536];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|_| SnapshotQueueError::Unavailable)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .ok_or(SnapshotQueueError::Invalid)?;
            if total > MAXIMUM_SNAPSHOT_BYTES {
                return Err(SnapshotQueueError::Invalid);
            }
            hash.update(&buffer[..count]);
            incoming
                .write_all(&buffer[..count])
                .map_err(|_| SnapshotQueueError::Unavailable)?;
        }
        if total == 0 || hex::encode(hash.finalize()) != file_sha256 {
            return Err(SnapshotQueueError::Invalid);
        }
        incoming
            .as_file()
            .sync_all()
            .map_err(|_| SnapshotQueueError::Unavailable)?;
        let id = Uuid::new_v4();
        let destination = root.join(format!("{id}.ndjson"));
        let result = self.update(|locked_root, state| {
            if locked_root != root {
                return Err(SnapshotQueueError::Invalid);
            }
            if state
                .request
                .as_ref()
                .is_some_and(|request| request.status.active())
            {
                return Err(SnapshotQueueError::Busy);
            }
            if state.local_version.as_deref() != Some(expected_local_version) {
                return Err(SnapshotQueueError::Conflict);
            }
            incoming
                .persist(&destination)
                .map_err(|_| SnapshotQueueError::Unavailable)?;
            let previous = state.request.as_ref().map(|request| request.id);
            state.request = Some(SnapshotRequest {
                id,
                account_id: account_id.to_owned(),
                cloud_revision,
                expected_local_version: expected_local_version.to_owned(),
                file_sha256: file_sha256.to_owned(),
                status: SnapshotRequestStatus::Queued,
            });
            Ok(previous)
        });
        let previous = match result {
            Ok(previous) => previous,
            Err(error) => {
                let _ = fs::remove_file(&destination);
                return Err(error);
            }
        };
        if let Some(previous) = previous {
            let _ = self.delete_snapshot(previous);
        }
        Ok(id)
    }

    pub fn acquire_worker_lease(&self) -> Result<SnapshotWorkerLease, SnapshotQueueError> {
        let (file, owner) = self.lock(WORKER_LOCK_NAME)?;
        Ok(SnapshotWorkerLease { _file: file, owner })
    }

    fn check_lease(&self, lease: &SnapshotWorkerLease) -> Result<PathBuf, SnapshotQueueError> {
        let root = self.root()?;
        if lease.owner != root {
            return Err(SnapshotQueueError::Invalid);
        }
        Ok(root)
    }

    pub fn claim(
        &self,
        lease: &SnapshotWorkerLease,
    ) -> Result<Option<SnapshotRequest>, SnapshotQueueError> {
        self.check_lease(lease)?;
        self.update(|_, state| {
            let Some(request) = state
                .request
                .as_mut()
                .filter(|request| request.status.active())
            else {
                return Ok(None);
            };
            request.status = SnapshotRequestStatus::Preparing;
            Ok(Some(request.clone()))
        })
    }

    pub fn complete(
        &self,
        id: Uuid,
        lease: &SnapshotWorkerLease,
        current_version: &str,
        already_applied: bool,
        apply: impl FnOnce() -> Result<String, SnapshotQueueError>,
    ) -> Result<bool, SnapshotQueueError> {
        self.check_lease(lease)?;
        if !valid_local_version(current_version) {
            return Err(SnapshotQueueError::Invalid);
        }
        let applied = self.update(|_, state| {
            let request = state.request.as_mut().ok_or(SnapshotQueueError::Conflict)?;
            if request.id != id || (!already_applied && !request.status.active()) {
                return Err(SnapshotQueueError::Conflict);
            }
            if already_applied {
                request.status = SnapshotRequestStatus::Applied;
                state.local_version = Some(current_version.to_owned());
            } else if request.expected_local_version != current_version {
                request.status = SnapshotRequestStatus::Conflict;
                state.local_version = Some(current_version.to_owned());
            } else {
                let next = apply()?;
                if !valid_local_version(&next) {
                    return Err(SnapshotQueueError::Invalid);
                }
                request.status = SnapshotRequestStatus::Applied;
                state.local_version = Some(next);
            }
            Ok(request.status == SnapshotRequestStatus::Applied)
        })?;
        self.delete_snapshot(id)?;
        Ok(applied)
    }

    pub fn fail(&self, id: Uuid, lease: &SnapshotWorkerLease) -> Result<(), SnapshotQueueError> {
        self.check_lease(lease)?;
        self.transition(id, SnapshotRequestStatus::Failed)
    }

    pub fn cancel(&self, account_id: &str) -> Result<(), SnapshotQueueError> {
        if account_id.is_empty() || account_id.len() > 128 {
            return Err(SnapshotQueueError::Invalid);
        }
        // Account changes must order against activation, whose callback runs under the state lock.
        // Waiting here ensures logout either cancels first or observes a request already applied.
        let cancelled = self.update_wait(|_, state| {
            let Some(request) = state
                .request
                .as_mut()
                .filter(|request| request.account_id == account_id && request.status.active())
            else {
                return Ok(None);
            };
            request.status = SnapshotRequestStatus::Cancelled;
            Ok(Some(request.id))
        })?;
        if let Some(id) = cancelled {
            self.delete_snapshot(id)?;
        }
        Ok(())
    }

    fn transition(
        &self,
        id: Uuid,
        status: SnapshotRequestStatus,
    ) -> Result<(), SnapshotQueueError> {
        if status.active() {
            return Err(SnapshotQueueError::Invalid);
        }
        let changed = self.update(|_, state| {
            let Some(request) = state
                .request
                .as_mut()
                .filter(|request| request.id == id && request.status.active())
            else {
                return Ok(false);
            };
            request.status = status;
            Ok(true)
        })?;
        if changed {
            self.delete_snapshot(id)?;
        }
        Ok(())
    }

    /// Return a terminal state once, then acknowledge it while retaining the local version.
    pub fn take_state(&self) -> Result<SnapshotQueueState, SnapshotQueueError> {
        self.update(|root, state| {
            let result = state.clone();
            if let Some(request) = state
                .request
                .as_ref()
                .filter(|request| !request.status.active())
            {
                let path = root.join(format!("{}.ndjson", request.id));
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(SnapshotQueueError::Unavailable),
                }
                state.request = None;
            }
            Ok(result)
        })
    }

    fn delete_snapshot(&self, id: Uuid) -> Result<(), SnapshotQueueError> {
        let path = self.file_path(id)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SnapshotQueueError::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(owner: &str, digest: char) -> String {
        format!("local-v1:{owner}:{}", digest.to_string().repeat(64))
    }

    #[test]
    fn queue_survives_restart_and_reconciles_activation_receipts() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let source = parent.path().join("snapshot.ndjson");
        fs::write(&source, b"synthetic snapshot\n").unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&source).unwrap()));
        let initial = version("legacy", 'a');
        let queue = DictionarySnapshotQueue::new(root.clone()).unwrap();
        assert_eq!(queue.read().unwrap().request, None);
        assert!(!root.exists(), "a read must not create queue files");
        queue.publish_local_version(&initial).unwrap();
        let id = queue
            .enqueue(&source, "fixture-account", 7, &initial, &digest)
            .unwrap();
        drop(queue);

        let restored = DictionarySnapshotQueue::new(root).unwrap();
        let lease = restored.acquire_worker_lease().unwrap();
        let request = restored.claim(&lease).unwrap().unwrap();
        assert_eq!(request.id, id);
        assert_eq!(request.status, SnapshotRequestStatus::Preparing);
        let applied = version(&id.to_string(), 'b');
        restored.publish_local_version(&applied).unwrap();
        assert_eq!(
            restored.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Applied
        );
        assert!(!restored.file_path(id).unwrap().exists());
        assert_eq!(restored.take_state().unwrap().request.unwrap().id, id);
        assert!(restored.read().unwrap().request.is_none());
    }

    #[test]
    fn queue_checks_hash_conflict_account_and_corrupt_state() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let source = parent.path().join("snapshot.ndjson");
        fs::write(&source, b"synthetic snapshot\n").unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&source).unwrap()));
        let initial = version("legacy", 'a');
        let queue = DictionarySnapshotQueue::new(root.clone()).unwrap();
        queue.publish_local_version(&initial).unwrap();
        assert!(matches!(
            queue.enqueue(&source, "fixture", 1, &initial, &"b".repeat(64)),
            Err(SnapshotQueueError::Invalid)
        ));
        let id = queue
            .enqueue(&source, "fixture", 1, &initial, &digest)
            .unwrap();
        queue.cancel("other-account").unwrap();
        assert_eq!(
            queue.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Queued
        );
        queue.cancel("fixture").unwrap();
        assert_eq!(
            queue.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Cancelled
        );
        assert!(!queue.file_path(id).unwrap().exists());
        fs::write(root.join(STATE_NAME), b"{not-json").unwrap();
        assert!(matches!(queue.read(), Err(SnapshotQueueError::Invalid)));
        assert_eq!(fs::read(root.join(STATE_NAME)).unwrap(), b"{not-json");
    }

    #[test]
    fn queue_rejects_a_persisted_request_with_a_nil_id() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let initial = version("legacy", 'a');
        let queue = DictionarySnapshotQueue::new(root.clone()).unwrap();
        queue.publish_local_version(&initial).unwrap();
        let state = SnapshotQueueState {
            version: 1,
            local_version: Some(initial.clone()),
            request: Some(SnapshotRequest {
                id: Uuid::nil(),
                account_id: "fixture".into(),
                cloud_revision: 1,
                expected_local_version: initial,
                file_sha256: "a".repeat(64),
                status: SnapshotRequestStatus::Queued,
            }),
        };
        fs::write(root.join(STATE_NAME), serde_json::to_vec(&state).unwrap()).unwrap();

        assert!(matches!(queue.read(), Err(SnapshotQueueError::Invalid)));
    }

    #[test]
    fn queue_marks_source_version_conflicts_without_activation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let source = parent.path().join("snapshot.ndjson");
        fs::write(&source, b"synthetic snapshot\n").unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&source).unwrap()));
        let initial = version("legacy", 'a');
        let changed = version("legacy", 'b');
        let queue = DictionarySnapshotQueue::new(root).unwrap();
        queue.publish_local_version(&initial).unwrap();
        let id = queue
            .enqueue(&source, "fixture", 1, &initial, &digest)
            .unwrap();
        let lease = queue.acquire_worker_lease().unwrap();
        queue.claim(&lease).unwrap();
        let mut activated = false;
        assert!(!queue
            .complete(id, &lease, &changed, false, || {
                activated = true;
                Ok(version(&id.to_string(), 'c'))
            })
            .unwrap());
        assert!(!activated);
        assert_eq!(
            queue.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Conflict
        );
    }

    #[test]
    fn a_late_activation_receipt_does_not_resurrect_cancelled_request() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let source = parent.path().join("snapshot.ndjson");
        fs::write(&source, b"synthetic snapshot\n").unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&source).unwrap()));
        let initial = version("legacy", 'a');
        let queue = DictionarySnapshotQueue::new(root).unwrap();
        queue.publish_local_version(&initial).unwrap();
        let id = queue
            .enqueue(&source, "fixture", 1, &initial, &digest)
            .unwrap();
        queue.cancel("fixture").unwrap();

        // The worker's activation receipt may be delivered after cancellation.
        queue
            .publish_local_version(&version(&id.to_string(), 'b'))
            .unwrap();
        assert_eq!(
            queue.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Cancelled
        );
    }

    #[test]
    fn account_cancellation_orders_against_activation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("queue");
        let source = parent.path().join("snapshot.ndjson");
        fs::write(&source, b"synthetic snapshot\n").unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&source).unwrap()));
        let initial = version("legacy", 'a');
        let queue = DictionarySnapshotQueue::new(root.clone()).unwrap();
        queue.publish_local_version(&initial).unwrap();
        let id = queue
            .enqueue(&source, "fixture", 1, &initial, &digest)
            .unwrap();
        let (entered_send, entered_receive) = std::sync::mpsc::channel();
        let (release_send, release_receive) = std::sync::mpsc::channel();
        let worker_root = root.clone();
        let worker_version = initial.clone();
        let worker = std::thread::spawn(move || {
            let worker_queue = DictionarySnapshotQueue::new(worker_root).unwrap();
            let lease = worker_queue.acquire_worker_lease().unwrap();
            worker_queue.claim(&lease).unwrap();
            worker_queue
                .complete(id, &lease, &worker_version, false, || {
                    entered_send.send(()).unwrap();
                    release_receive.recv().unwrap();
                    Ok(version(&id.to_string(), 'b'))
                })
                .unwrap()
        });
        entered_receive.recv().unwrap();
        let cancel_root = root.clone();
        let cancel = std::thread::spawn(move || {
            DictionarySnapshotQueue::new(cancel_root)
                .unwrap()
                .cancel("fixture")
                .unwrap();
        });
        release_send.send(()).unwrap();
        assert!(worker.join().unwrap());
        cancel.join().unwrap();
        assert_eq!(
            queue.read().unwrap().request.unwrap().status,
            SnapshotRequestStatus::Applied,
            "cancellation waits for in-flight publication and cannot relabel it cancelled"
        );
    }
}
