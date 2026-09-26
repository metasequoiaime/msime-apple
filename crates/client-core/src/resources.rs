//! Install trusted, pinned resource sets without replacing active generations.
//! Transport is injected by the host; filenames, lengths and hashes come from a
//! reviewed product lock, never from an untrusted downloaded manifest alone.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    /// Download location. Empty when the artifact is supplied by the Engine tree instead; see
    /// `engine_path`.
    #[serde(default)]
    pub url: String,
    /// Path inside the pinned Engine checkout, for artifacts that ship with the Engine rather than
    /// with the dictionary release. The Engine is already pinned by commit and archive SHA-256 in
    /// engine-lock.json, so republishing the same bytes in the dictionary release would create a
    /// second source of truth for them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub engine_path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSet {
    /// Actual source of the data, independently of the Engine code revision.
    pub source_commit: String,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    #[error("invalid pinned resource set")]
    InvalidManifest,
    #[error("resource length or digest mismatch")]
    Integrity,
    /// Carries what was actually found. This is the one resource error a host cannot reproduce off
    /// the device -- it fires on a directory the host did not stage itself, most often an app bundle
    /// whose contents differ from the staging machine's. Without the listing there is nothing left
    /// to read anywhere on the device.
    #[error("existing resource generation has unexpected files: {0}")]
    ExistingGeneration(String),
    #[error("resource storage or transport failed: {0}")]
    Io(#[from] std::io::Error),
}

impl ResourceSet {
    pub fn validate(&self) -> Result<(), ResourceError> {
        let hex = |text: &str, len| {
            text.len() == len
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !hex(&self.source_commit, 40) || self.artifacts.is_empty() || self.artifacts.len() > 128
        {
            return Err(ResourceError::InvalidManifest);
        }
        let mut names = HashSet::new();
        for artifact in &self.artifacts {
            // A flat, portable resource layout. Reject aliases, traversal and device names.
            let stem = artifact
                .name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
                || (stem.len() == 4
                    && (stem.starts_with("com") || stem.starts_with("lpt"))
                    && stem.as_bytes()[3].is_ascii_digit());
            if artifact.name.is_empty()
                || artifact.name.len() > 128
                || artifact.name.starts_with('.')
                || artifact.name.ends_with('.')
                || !artifact
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || reserved
                || !names.insert(artifact.name.to_ascii_lowercase())
                || !hex(&artifact.sha256, 64)
                || artifact.size > 2 * 1024 * 1024 * 1024
                // Exactly one source, and a download must be HTTPS. An artifact with both, or
                // with neither, is a manifest that cannot be resolved unambiguously.
                || artifact.url.is_empty() == artifact.engine_path.is_empty()
                || (!artifact.url.is_empty() && !artifact.url.starts_with("https://"))
                || !portable_relative_path(&artifact.engine_path)
            {
                return Err(ResourceError::InvalidManifest);
            }
        }
        Ok(())
    }

    pub fn generation(&self) -> Result<String, ResourceError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| ResourceError::InvalidManifest)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

/// An Engine path must stay inside the checkout on every host, so it is checked as text rather
/// than with `Path`, which would read `C:` or `a\b` as ordinary components on Linux: forward-slash
/// segments of the same characters a name allows, none empty, `.` or `..`. Empty is left to the
/// one-source rule.
fn portable_relative_path(path: &str) -> bool {
    path.is_empty()
        || path.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..")
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

/// Remove stages an installer left when it was killed mid-download. Only called under the
/// exclusive `resources.lock`, so none of them can still be in use. Only real directories are
/// removed, and a failure never stops the install.
fn sweep_abandoned_stages(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let is_stage = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("incoming-"));
        if is_stage && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

pub struct ResourceStore {
    root: PathBuf,
}

impl ResourceStore {
    /// root is an application-owned directory, separate from user learning data.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn install(
        &self,
        specification: &ResourceSet,
        mut fetch: impl FnMut(&Artifact) -> Result<Box<dyn Read>, std::io::Error>,
    ) -> Result<PathBuf, ResourceError> {
        let generation = specification.generation()?;
        fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("resources.lock"))?;
        crate::file_lock::exclusive(&lock)?;
        sweep_abandoned_stages(&self.root);
        let destination = self.root.join(generation);
        if fs::symlink_metadata(&destination).is_ok() {
            self.verify(&destination, specification)?;
            return Ok(destination);
        }
        let stage = tempfile::Builder::new()
            .prefix("incoming-")
            .tempdir_in(&self.root)?;
        for artifact in &specification.artifacts {
            let mut source = fetch(artifact)?;
            let mut output = File::create(stage.path().join(&artifact.name))?;
            copy_verified(source.as_mut(), &mut output, artifact)?;
            output.sync_all()?;
        }
        // Published directories are complete. Existing generations are never overwritten.
        fs::rename(stage.path(), &destination)?;
        Ok(destination)
    }

    pub fn verify(
        &self,
        directory: &Path,
        specification: &ResourceSet,
    ) -> Result<(), ResourceError> {
        specification.validate()?;
        let kind = fs::symlink_metadata(directory)?.file_type();
        if !kind.is_dir() {
            return Err(ResourceError::ExistingGeneration(format!(
                "{} is not a directory ({})",
                directory.display(),
                describe(kind)
            )));
        }
        let expected: HashSet<_> = specification
            .artifacts
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        let mut count = 0;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(ResourceError::ExistingGeneration(format!(
                    "non-UTF-8 entry in {}",
                    directory.display()
                )));
            };
            // The Engine reads its helpcode tables from `helpcodes/` under this same directory. They are an Engine asset rather than part of the pinned dictionary release, so a host that ships them puts them here; only the directory itself is let through, and every pinned file is still checked below.
            if name == HELPCODE_DIRECTORY && kind.is_dir() {
                continue;
            }
            if !kind.is_file() {
                return Err(ResourceError::ExistingGeneration(format!(
                    "{name} in {} is a {}, not a file",
                    directory.display(),
                    describe(kind)
                )));
            }
            if !expected.contains(name) {
                return Err(ResourceError::ExistingGeneration(format!(
                    "{name} in {} is not in the pinned resource set",
                    directory.display()
                )));
            }
            count += 1;
        }
        if count != expected.len() {
            let mut missing: Vec<_> = expected
                .iter()
                .filter(|name| !directory.join(name).is_file())
                .copied()
                .collect();
            missing.sort_unstable();
            return Err(ResourceError::ExistingGeneration(format!(
                "{} holds {count} of the {} pinned resources, missing: {}",
                directory.display(),
                expected.len(),
                missing.join(", ")
            )));
        }
        for artifact in &specification.artifacts {
            let mut input = File::open(directory.join(&artifact.name))?;
            copy_verified(&mut input, &mut std::io::sink(), artifact)?;
        }
        Ok(())
    }
}

/// Where the Engine looks for helpcode tables, relative to the resource directory (`helpcodes/…` in its asset contract).
const HELPCODE_DIRECTORY: &str = "helpcodes";
/// Verification markers are generated locally and contain only the pinned
/// artifact names and metadata. Keep a corrupt or replaced marker from
/// allocating without bound before it is discarded as a cache miss.
const MAX_MARKER_BYTES: u64 = 64 * 1024;

fn describe(kind: std::fs::FileType) -> &'static str {
    if kind.is_dir() {
        "directory"
    } else if kind.is_symlink() {
        "symlink"
    } else if kind.is_file() {
        "file"
    } else {
        "special file"
    }
}

fn copy_verified(
    input: &mut dyn Read,
    output: &mut dyn Write,
    artifact: &Artifact,
) -> Result<(), ResourceError> {
    let mut hash = Sha256::new();
    let mut remaining = artifact.size;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let limit = buffer.len().min((remaining + 1) as usize);
        let count = input.read(&mut buffer[..limit])?;
        if count == 0 {
            break;
        }
        if count as u64 > remaining {
            return Err(ResourceError::Integrity);
        }
        remaining -= count as u64;
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if remaining != 0 || hex::encode(hash.finalize()) != artifact.sha256 {
        return Err(ResourceError::Integrity);
    }
    Ok(())
}

/// A record that one resource directory was verified, so the next start need not hash it again.
///
/// `verify` reads every artifact to recompute its SHA-256. That is the right thing to do once,
/// and the wrong thing to do on every launch: the desktop set is 169 MB, which costs about half a
/// second of hashing before the first keystroke can be served, every time the Server process
/// starts. The Engine is already handled this way - `scripts/fetch_engine.py` writes a marker
/// naming what it prepared and skips the work when it matches - and this is the same idea for the
/// dictionaries.
///
/// What the marker cannot do is replace the hashes. It records the identity of the *set* and, per
/// file, the size and modification time the verified bytes had. A file whose size or mtime moved is
/// re-hashed; so is one that is missing, and so is the whole set when the specification changes. A
/// replacement crafted to keep both size and mtime would be accepted, which is the trade: the
/// resources sit in the installation directory, so writing there already requires the privileges
/// that hashing at launch would not have stopped anyway.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedMarker {
    /// The specification's generation digest, so a different resource set never matches.
    pub generation: String,
    /// Absolute path of the directory these files were verified in.
    pub directory: String,
    /// `(name, size, modified-nanoseconds)` per artifact, sorted by name.
    pub files: Vec<(String, u64, u128)>,
    /// Every entry in the resource directory, including the Engine-owned `helpcodes` directory.
    /// The fast path must notice an unpinned file appearing after the initial verification; the
    /// full verifier rejects such files, so a marker that does not record the directory shape
    /// would silently skip that check on the next launch.
    pub entries: Vec<String>,
}

impl VerifiedMarker {
    /// Describe `directory` as it is right now, or `None` when any artifact cannot be read.
    pub fn describe(
        directory: &Path,
        specification: &ResourceSet,
    ) -> Result<Option<Self>, ResourceError> {
        let expected: HashSet<_> = specification
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        let Ok(directory_entries) = fs::read_dir(directory) else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for entry in directory_entries {
            let Ok(entry) = entry else {
                return Ok(None);
            };
            let Ok(kind) = entry.file_type() else {
                return Ok(None);
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Ok(None);
            };
            // Keep the same exception as `verify`: Engine helpcode tables are installed beside
            // the pinned artifacts, but the directory itself must be a real directory.
            if name == HELPCODE_DIRECTORY {
                if !kind.is_dir() {
                    return Ok(None);
                }
            } else if !expected.contains(name.as_str()) || !kind.is_file() {
                return Ok(None);
            }
            entries.push(name);
        }
        entries.sort();
        let mut files = Vec::with_capacity(specification.artifacts.len());
        for artifact in &specification.artifacts {
            // `metadata` follows symlinks. The full verifier rejects them, so use
            // `symlink_metadata` here and force a marker miss instead of letting a symlinked
            // artifact inherit the target file's size and mtime.
            let Ok(metadata) = fs::symlink_metadata(directory.join(&artifact.name)) else {
                return Ok(None);
            };
            if !metadata.file_type().is_file() {
                return Ok(None);
            }
            let Ok(modified) = metadata.modified() else {
                return Ok(None);
            };
            let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
                return Ok(None);
            };
            files.push((
                artifact.name.clone(),
                metadata.len(),
                since_epoch.as_nanos(),
            ));
        }
        files.sort();
        Ok(Some(Self {
            generation: specification.generation()?,
            directory: directory.to_string_lossy().into_owned(),
            files,
            entries,
        }))
    }

    /// Read a marker previously written by [`VerifiedMarker::write`].
    ///
    /// A marker that is absent, unreadable or not the shape this version writes is simply a miss:
    /// the caller hashes, and writes a fresh one.
    pub fn read(path: &Path) -> Option<Self> {
        let mut bytes = Vec::new();
        File::open(path)
            .ok()?
            .take(MAX_MARKER_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_MARKER_BYTES {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&self, path: &Path) -> Result<(), ResourceError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec(self).map_err(|_| ResourceError::InvalidManifest)?;
        fs::write(path, encoded)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn specification() -> ResourceSet {
        ResourceSet {
            source_commit: "a".repeat(40),
            artifacts: vec![Artifact {
                name: "msime.db".into(),
                url: "https://example.invalid/msime.db".into(),
                engine_path: String::new(),
                sha256: hex::encode(Sha256::digest(b"fixture")),
                size: 7,
            }],
        }
    }
    fn source(bytes: &[u8]) -> Box<dyn Read> {
        Box::new(Cursor::new(bytes.to_vec()))
    }
    #[test]
    fn an_artifact_names_exactly_one_source() {
        let with = |url: &str, engine: &str| {
            let mut set = specification();
            set.artifacts[0].url = url.into();
            set.artifacts[0].engine_path = engine.into();
            set.validate()
        };
        assert!(with("https://example.invalid/a", "").is_ok());
        assert!(with("", "googlepinyinime-rev/data/dict_pinyin.dat").is_ok());
        // Neither source, or both, leaves the artifact unresolvable.
        assert!(with("", "").is_err());
        assert!(with("https://example.invalid/a", "data/a.dat").is_err());
        // A download must still be HTTPS, and an Engine path must stay inside the checkout.
        assert!(with("http://example.invalid/a", "").is_err());
        assert!(with("", "../escape.dat").is_err());
        assert!(with("", "/absolute.dat").is_err());
        // Windows would re-root these on join, so they are refused on every host.
        for path in [
            "C:/x.dat", "C:x.dat", "a\\b.dat", "\\x.dat", "a//b", "./a", "a/",
        ] {
            assert!(with("", path).is_err(), "{path}");
        }
        assert!(with("", "data/a..b.dat").is_ok());
    }

    #[test]
    fn publishes_complete_generation_and_verifies_cached_bytes() {
        let root = tempfile::tempdir().unwrap();
        let store = ResourceStore::new(root.path());
        let spec = specification();
        let path = store.install(&spec, |_| Ok(source(b"fixture"))).unwrap();
        assert_eq!(fs::read(path.join("msime.db")).unwrap(), b"fixture");
        assert_eq!(
            store
                .install(&spec, |_| panic!("must not fetch cached resources"))
                .unwrap(),
            path
        );
        fs::write(path.join("msime.db"), b"damaged").unwrap();
        assert!(matches!(
            store.install(&spec, |_| panic!("must not overwrite active resources")),
            Err(ResourceError::Integrity)
        ));
    }
    #[test]
    fn rejects_truncated_oversized_and_wrong_digest_without_publishing() {
        for bytes in [b"short".as_slice(), b"fixture-extra", b"invalid"] {
            let root = tempfile::tempdir().unwrap();
            let store = ResourceStore::new(root.path());
            let spec = specification();
            assert!(matches!(
                store.install(&spec, |_| Ok(source(bytes))),
                Err(ResourceError::Integrity)
            ));
            assert!(!root.path().join(spec.generation().unwrap()).exists());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }
    #[test]
    fn stages_an_interrupted_install_left_are_swept() {
        let root = tempfile::tempdir().unwrap();
        let store = ResourceStore::new(root.path());
        let spec = specification();
        let stale = root.path().join("incoming-abandoned");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("msime.db"), b"fix").unwrap();
        let path = store.install(&spec, |_| Ok(source(b"fixture"))).unwrap();
        assert!(!stale.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        // A cached install sweeps too, and a file of that name is left alone.
        fs::create_dir(&stale).unwrap();
        fs::write(root.path().join("incoming-note"), b"").unwrap();
        assert_eq!(
            store
                .install(&spec, |_| panic!("must not fetch cached resources"))
                .unwrap(),
            path
        );
        assert!(!stale.exists());
        assert!(root.path().join("incoming-note").is_file());
    }
    #[test]
    fn failed_upgrade_preserves_previous_generation() {
        let root = tempfile::tempdir().unwrap();
        let store = ResourceStore::new(root.path());
        let mut spec = specification();
        let old = store.install(&spec, |_| Ok(source(b"fixture"))).unwrap();
        spec.source_commit = "b".repeat(40);
        assert!(store
            .install(&spec, |_| Err(std::io::Error::other("offline")))
            .is_err());
        assert_eq!(fs::read(old.join("msime.db")).unwrap(), b"fixture");
    }
    #[test]
    fn verification_admits_the_engine_helpcode_directory_only() {
        let root = tempfile::tempdir().unwrap();
        let store = ResourceStore::new(root.path());
        let spec = specification();
        let path = store.install(&spec, |_| Ok(source(b"fixture"))).unwrap();
        fs::create_dir(path.join("helpcodes")).unwrap();
        fs::write(path.join("helpcodes/helpcode.txt"), b"a=aa").unwrap();
        assert!(store.verify(&path, &spec).is_ok());
        // A file by that name, or any other extra directory, is still not in the pinned set.
        fs::remove_dir_all(path.join("helpcodes")).unwrap();
        fs::write(path.join("helpcodes"), b"").unwrap();
        assert!(store.verify(&path, &spec).is_err());
        fs::remove_file(path.join("helpcodes")).unwrap();
        fs::create_dir(path.join("extra")).unwrap();
        assert!(store.verify(&path, &spec).is_err());
    }
    #[test]
    fn rejects_path_aliases_and_duplicate_names() {
        for name in [
            "../secret",
            "a/b",
            "a\\b",
            "CON",
            "nul.db",
            "msime.db.",
            ".hidden",
        ] {
            let mut spec = specification();
            spec.artifacts[0].name = name.into();
            assert!(spec.validate().is_err());
        }
        let mut spec = specification();
        let mut duplicate = spec.artifacts[0].clone();
        duplicate.name = "MSIME.DB".into();
        spec.artifacts.push(duplicate);
        assert!(spec.validate().is_err());
    }

    /// What the marker is allowed to skip, and what it must not.
    ///
    /// The point of recording a verification is to not hash 169 MB at every launch. The point of
    /// recording it *this* way is that anything which could mean different bytes puts the hashing
    /// back: a different resource set, a file that grew or shrank, a file written again, a file
    /// that is no longer there.
    #[test]
    fn a_recorded_verification_only_matches_the_files_it_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let spec = specification();
        fs::write(directory.path().join("msime.db"), b"fixture").unwrap();

        let recorded = VerifiedMarker::describe(directory.path(), &spec)
            .unwrap()
            .expect("every artifact is present");
        assert_eq!(
            VerifiedMarker::describe(directory.path(), &spec).unwrap(),
            Some(recorded.clone()),
            "an untouched directory describes identically, which is what lets the hashing be skipped"
        );

        // A different resource set never matches, even over the same bytes.
        let mut other = specification();
        other.source_commit = "b".repeat(40);
        assert_ne!(
            VerifiedMarker::describe(directory.path(), &other).unwrap(),
            Some(recorded.clone()),
            "the generation is part of the record"
        );

        // Same length, written again: the modification time moves and the record stops matching.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(directory.path().join("msime.db"), b"FIXTURE").unwrap();
        assert_ne!(
            VerifiedMarker::describe(directory.path(), &spec).unwrap(),
            Some(recorded.clone()),
            "a rewritten file is re-hashed even when its size is unchanged"
        );

        // A different length is caught whatever the clock did.
        fs::write(directory.path().join("msime.db"), b"fixture-and-more").unwrap();
        let grown = VerifiedMarker::describe(directory.path(), &spec)
            .unwrap()
            .expect("still present");
        assert_ne!(grown.files[0].1, recorded.files[0].1);

        // A missing artifact is not describable, so there is nothing to compare and it is hashed.
        fs::remove_file(directory.path().join("msime.db")).unwrap();
        assert_eq!(
            VerifiedMarker::describe(directory.path(), &spec).unwrap(),
            None
        );
    }

    #[test]
    fn marker_misses_unpinned_entries_and_symlinked_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let spec = specification();
        fs::write(directory.path().join("msime.db"), b"fixture").unwrap();
        assert!(VerifiedMarker::describe(directory.path(), &spec)
            .unwrap()
            .is_some());

        // Resource verification rejects files outside the pinned set. The marker fast path must
        // therefore stop matching when one appears after the initial verification.
        fs::write(directory.path().join("unexpected.db"), b"fixture").unwrap();
        assert_eq!(
            VerifiedMarker::describe(directory.path(), &spec).unwrap(),
            None
        );

        fs::remove_file(directory.path().join("unexpected.db")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = directory.path().join("target.db");
            fs::write(&target, b"fixture").unwrap();
            fs::remove_file(directory.path().join("msime.db")).unwrap();
            symlink(&target, directory.path().join("msime.db")).unwrap();
            assert_eq!(
                VerifiedMarker::describe(directory.path(), &spec).unwrap(),
                None
            );
        }
    }

    /// A marker that cannot be read is a miss, not a failure.
    #[test]
    fn an_unusable_marker_falls_back_to_hashing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("verified-resources.json");
        assert_eq!(VerifiedMarker::read(&path), None, "absent");
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(VerifiedMarker::read(&path), None, "unparseable");
        fs::write(&path, br#"{"generation":"a"}"#).unwrap();
        assert_eq!(VerifiedMarker::read(&path), None, "an older or newer shape");

        let spec = specification();
        let resources = directory.path().join("resources");
        fs::create_dir(&resources).unwrap();
        fs::write(resources.join("msime.db"), b"fixture").unwrap();
        let marker = VerifiedMarker::describe(&resources, &spec)
            .unwrap()
            .unwrap();
        marker.write(&path).unwrap();
        assert_eq!(VerifiedMarker::read(&path), Some(marker), "round trips");

        fs::write(&path, vec![b' '; MAX_MARKER_BYTES as usize + 1]).unwrap();
        assert_eq!(
            VerifiedMarker::read(&path),
            None,
            "oversized markers are cache misses"
        );
    }
}
