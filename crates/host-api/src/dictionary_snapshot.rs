//! Native-only snapshot preparation. Staged paths stay private until a future
//! activation transaction can own publication and session coordination.
use super::{response, DictionaryAccess, HostOptions, HOST_OPTIONS_DOCUMENT_LIMIT};
use msime_client_core::account::{
    AccountDictionarySnapshotRestore, AccountError, BackendAccountClient,
};
use msime_client_core::cloud::snapshot_queue::{
    local_version, local_version_digest, DictionarySnapshotQueue, SnapshotQueueError,
};
use msime_client_core::resources::{ResourceSet, ResourceStore};
use msime_engine_bridge::{
    dictionary_state_revision, stage_dictionary_state, EngineOptions, Session, SnapshotReadError,
};
use serde::{
    de::{DeserializeSeed, MapAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::{c_char, c_void},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

mod record;

const BUFFER_LIMIT: usize = 65536;
const REQUEST_LIMIT: usize = HOST_OPTIONS_DOCUMENT_LIMIT;
const HANDLE_LIMIT: usize = 8;
const ACTIVATION_RECEIPT_NAME: &str = ".msime-snapshot-activation";
const MAX_ACTIVATION_RECEIPT_BYTES: u64 = 36;
const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SNAPSHOT_LINE_BYTES: usize = 65_536;
const MAX_SNAPSHOT_RECORDS: usize = 500_000;
// The guard's own file, which stays put while everything around it is swapped.
const DICTIONARY_ACCESS_LOCK_NAME: &str = ".msime-dictionary-access.lock";
static NEXT: AtomicU64 = AtomicU64::new(1);
static PREPARED: OnceLock<Mutex<HashMap<u64, Prepared>>> = OnceLock::new();
fn registry() -> &'static Mutex<HashMap<u64, Prepared>> {
    PREPARED.get_or_init(Default::default)
}

struct Prepared {
    directory: tempfile::TempDir,
    active_options: EngineOptions,
    options: EngineOptions,
    source_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest {
    options: HostOptions,
    staging_root: String,
    expected_version: String,
    records: usize,
    #[serde(default)]
    activation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreRequest {
    revision: i64,
    expected_sha256: String,
    access_token: String,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SnapshotQueueAction {
    State {
        directory: String,
        options: HostOptions,
        #[serde(default)]
        acknowledge: bool,
    },
    Enqueue {
        directory: String,
        source: String,
        account_id: String,
        cloud_revision: i64,
        expected_local_version: String,
        file_sha256: String,
    },
    Cancel {
        directory: String,
        account_id: String,
    },
    Process {
        directory: String,
        staging_root: String,
        options: HostOptions,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SnapshotMetadata {
    cloud_revision: i64,
    sha256: String,
    file_sha256: String,
    bytes: u64,
    records: usize,
    entries: usize,
    overlays: usize,
    positions: usize,
    selections: usize,
    engine_records: usize,
}

struct StrictSnapshotValue {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictSnapshotValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SnapshotValueVisitor {
            depth: usize,
        }

        impl<'de> Visitor<'de> for SnapshotValueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a strict JSON object value")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(Value::Number(value.into()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Value::Number(value.into()))
            }

            fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Err(E::custom("floating point values are not allowed"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(Value::String(value))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(Value::Null)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Value::Null)
            }

            fn visit_seq<A>(self, _sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                Err(serde::de::Error::custom("arrays are not allowed"))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                if self.depth > 1 {
                    return Err(serde::de::Error::custom("nested objects are not allowed"));
                }
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                    let value = map.next_value_seed(StrictSnapshotValue {
                        depth: self.depth + 1,
                    })?;
                    object.insert(key, value);
                }
                Ok(Value::Object(object))
            }
        }

        deserializer.deserialize_any(SnapshotValueVisitor { depth: self.depth })
    }
}

fn parse_snapshot_object(bytes: &[u8]) -> Result<serde_json::Map<String, Value>, &'static str> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictSnapshotValue { depth: 0 }
        .deserialize(&mut deserializer)
        .map_err(|_| "invalid snapshot document")?;
    deserializer
        .end()
        .map_err(|_| "invalid snapshot document")?;
    value
        .as_object()
        .cloned()
        .ok_or("invalid snapshot document")
}

fn snapshot_has_keys(map: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key))
}

fn snapshot_text<'a>(
    data: &'a serde_json::Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<&'a str, &'static str> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= maximum
                && !value
                    .bytes()
                    .any(|byte| matches!(byte, 0 | b'\t' | b'\n' | b'\r'))
        })
        .ok_or("invalid snapshot document")
}

fn snapshot_integer(data: &serde_json::Map<String, Value>, key: &str) -> Result<i64, &'static str> {
    data.get(key)
        .and_then(Value::as_i64)
        .ok_or("invalid snapshot document")
}

fn snapshot_timestamp(value: &str) -> bool {
    fn digits(bytes: &[u8], start: usize, end: usize) -> Option<u32> {
        (end <= bytes.len() && bytes[start..end].iter().all(u8::is_ascii_digit)).then(|| {
            bytes[start..end]
                .iter()
                .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
        })
    }
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || digits(bytes, 0, 4).is_none()
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let year = digits(bytes, 0, 4).unwrap();
    let month = match digits(bytes, 5, 7) {
        Some(value) => value,
        None => return false,
    };
    let day = match digits(bytes, 8, 10) {
        Some(value) => value,
        None => return false,
    };
    let hour = match digits(bytes, 11, 13) {
        Some(value) => value,
        None => return false,
    };
    let minute = match digits(bytes, 14, 16) {
        Some(value) => value,
        None => return false,
    };
    let second = match digits(bytes, 17, 19) {
        Some(value) => value,
        None => return false,
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year == 0
        || !(1..=12).contains(&month)
        || day == 0
        || day > days[month as usize - 1]
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return false;
    }
    let mut offset = 19;
    if matches!(bytes.get(offset), Some(b'.' | b',')) {
        offset += 1;
        let start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == start {
            return false;
        }
    }
    let zone = &bytes[offset..];
    if zone == b"Z" {
        return true;
    }
    if zone.len() != 6
        || !matches!(zone[0], b'+' | b'-')
        || !zone[1].is_ascii_digit()
        || !zone[2].is_ascii_digit()
        || zone[3] != b':'
        || !zone[4].is_ascii_digit()
        || !zone[5].is_ascii_digit()
    {
        return false;
    }
    let zone_hour = u32::from(zone[1] - b'0') * 10 + u32::from(zone[2] - b'0');
    let zone_minute = u32::from(zone[4] - b'0') * 10 + u32::from(zone[5] - b'0');
    zone_hour < 24 && zone_minute < 60
}

#[derive(Default)]
struct SnapshotIdentities {
    entry_keys: HashMap<(String, String, String), i64>,
    entry_ids: HashSet<String>,
    overlays: HashMap<(String, String, String), (bool, bool, i64)>,
    positions: HashSet<(String, String, String)>,
    position_slots: HashSet<(String, i64)>,
    selections: HashSet<(String, String, String)>,
}

fn inspect_snapshot_record(
    map: &serde_json::Map<String, Value>,
    revision: i64,
    identities: &mut SnapshotIdentities,
) -> Result<usize, &'static str> {
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or("invalid snapshot document")?;
    let data = map
        .get("data")
        .and_then(Value::as_object)
        .ok_or("invalid snapshot document")?;
    let code = snapshot_text(data, "code", 512)?.to_owned();
    let word = snapshot_text(data, "word", 2048)?.to_owned();
    match kind {
        "entry" | "overlay" => {
            let outer_keys_valid = if kind == "overlay" {
                snapshot_has_keys(map, &["type", "data", "deleted"])
            } else {
                snapshot_has_keys(map, &["type", "data"])
            };
            if !outer_keys_valid {
                return Err("invalid snapshot document");
            }
            let deleted = if kind == "overlay" {
                map.get("deleted")
                    .and_then(Value::as_bool)
                    .ok_or("invalid snapshot document")?
            } else {
                false
            };
            let expected = [
                "id",
                "kind",
                "code",
                "word",
                "weight",
                "revision",
                "updated_at",
            ];
            let mut data_keys = data.keys().map(String::as_str).collect::<HashSet<_>>();
            if data_keys.remove("user_inserted") {
                let user_inserted = data
                    .get("user_inserted")
                    .and_then(Value::as_bool)
                    .ok_or("invalid snapshot document")?;
                if kind == "entry" && !user_inserted {
                    return Err("invalid snapshot document");
                }
            }
            if data_keys.len() != expected.len()
                || expected.iter().any(|key| !data_keys.contains(key))
            {
                return Err("invalid snapshot document");
            }
            let dictionary_kind = data
                .get("kind")
                .and_then(Value::as_str)
                .filter(|value| matches!(*value, "pinyin" | "wubi" | "english" | "quick"))
                .ok_or("invalid snapshot document")?
                .to_owned();
            let id = data
                .get("id")
                .and_then(Value::as_str)
                .ok_or("invalid snapshot document")?
                .to_owned();
            if kind == "entry"
                && (id.is_empty()
                    || id.len() > 128
                    || id
                        .bytes()
                        .any(|byte| matches!(byte, 0 | b'\t' | b'\n' | b'\r')))
            {
                return Err("invalid snapshot document");
            }
            let weight = snapshot_integer(data, "weight")?;
            let record_revision = snapshot_integer(data, "revision")?;
            if !(0..=100_000_000).contains(&weight)
                || (weight == 0 && !deleted)
                || !(1..=revision).contains(&record_revision)
                || !snapshot_timestamp(
                    data.get("updated_at")
                        .and_then(Value::as_str)
                        .ok_or("invalid snapshot document")?,
                )
            {
                return Err("invalid snapshot document");
            }
            let identity = (dictionary_kind, code, word);
            if kind == "entry" {
                if identities.entry_keys.insert(identity, weight).is_some()
                    || !identities.entry_ids.insert(id)
                {
                    return Err("invalid snapshot document");
                }
            } else {
                let user_inserted = data
                    .get("user_inserted")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if identities
                    .overlays
                    .insert(identity, (deleted, user_inserted, weight))
                    .is_some()
                {
                    return Err("invalid snapshot document");
                }
            }
            Ok(if kind == "entry" { 1 } else { 2 })
        }
        "position" | "selection" => {
            if !snapshot_has_keys(map, &["type", "data"]) {
                return Err("invalid snapshot document");
            }
            let value_key = if kind == "position" {
                "position"
            } else {
                "count"
            };
            if !snapshot_has_keys(data, &["context", "code", "word", value_key]) {
                return Err("invalid snapshot document");
            }
            let context = snapshot_text(data, "context", 512)?.to_owned();
            if context.len() + code.len() + word.len() > 2048 {
                return Err("invalid snapshot document");
            }
            let identity = (context.clone(), code, word);
            if kind == "position" {
                let position = snapshot_integer(data, "position")?;
                if !(1..=5).contains(&position)
                    || !identities.positions.insert(identity)
                    || !identities.position_slots.insert((context, position))
                {
                    return Err("invalid snapshot document");
                }
                Ok(3)
            } else {
                let count = snapshot_integer(data, "count")?;
                if !(0..=10).contains(&count) || !identities.selections.insert(identity) {
                    return Err("invalid snapshot document");
                }
                Ok(4)
            }
        }
        _ => Err("invalid snapshot document"),
    }
}

/// Validate the complete NDJSON envelope before a host calls the expensive Engine staging path.
/// Header/footer order, exact body checksum, category order and record bounds are all part of the
/// cloud format. Engine records receive their deeper scheme-specific validation during prepare.
fn inspect_snapshot(path: &Path) -> Result<SnapshotMetadata, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "snapshot file unavailable")?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_SNAPSHOT_BYTES
    {
        return Err("invalid snapshot file");
    }
    let file = std::fs::File::open(path).map_err(|_| "snapshot file unavailable")?;
    let mut reader = BufReader::with_capacity(MAX_SNAPSHOT_LINE_BYTES, file);
    let mut line = Vec::with_capacity(MAX_SNAPSHOT_LINE_BYTES);
    let mut body_digest = Sha256::new();
    let mut file_digest = Sha256::new();
    let mut total_bytes = 0u64;
    let mut records = 0usize;
    let mut counts = [0usize; 4];
    let mut category = 0usize;
    let mut revision = None;
    let mut checksum = None;
    let mut identities = SnapshotIdentities::default();
    loop {
        line.clear();
        let complete = loop {
            let chunk = reader.fill_buf().map_err(|_| "snapshot file unavailable")?;
            if chunk.is_empty() {
                break false;
            }
            if let Some(index) = chunk.iter().position(|byte| *byte == b'\n') {
                if line.len() + index + 1 > MAX_SNAPSHOT_LINE_BYTES {
                    return Err("invalid snapshot document");
                }
                line.extend_from_slice(&chunk[..=index]);
                reader.consume(index + 1);
                break true;
            }
            if line.len() + chunk.len() >= MAX_SNAPSHOT_LINE_BYTES {
                return Err("invalid snapshot document");
            }
            line.extend_from_slice(chunk);
            let length = chunk.len();
            reader.consume(length);
        };
        let has_newline = complete;
        file_digest.update(&line);
        if has_newline {
            line.pop();
            if line.ends_with(b"\r") {
                return Err("invalid snapshot document");
            }
        } else if line.is_empty() {
            break;
        }
        total_bytes = total_bytes
            .checked_add(line.len() as u64 + u64::from(has_newline))
            .ok_or("invalid snapshot document")?;
        if total_bytes > MAX_SNAPSHOT_BYTES || line.is_empty() {
            return Err("invalid snapshot document");
        }
        let map = parse_snapshot_object(&line)?;
        let kind = map
            .get("type")
            .and_then(Value::as_str)
            .ok_or("invalid snapshot document")?;
        match kind {
            "header" => {
                if records != 0
                    || revision.is_some()
                    || !snapshot_has_keys(&map, &["type", "format", "version", "revision"])
                    || map.get("format").and_then(Value::as_str)
                        != Some("msime-dictionary-snapshot")
                    || map.get("version").and_then(Value::as_i64) != Some(1)
                {
                    return Err("invalid snapshot document");
                }
                revision = Some(
                    map.get("revision")
                        .and_then(Value::as_i64)
                        .filter(|value| *value >= 0)
                        .ok_or("invalid snapshot document")?,
                );
                records = 1;
                body_digest.update(&line);
                body_digest.update(b"\n");
            }
            "entry" | "overlay" | "position" | "selection" => {
                let snapshot_revision = revision.ok_or("invalid snapshot document")?;
                if checksum.is_some() {
                    return Err("invalid snapshot document");
                }
                let next = inspect_snapshot_record(&map, snapshot_revision, &mut identities)?;
                if next < category {
                    return Err("invalid snapshot document");
                }
                category = next;
                counts[next - 1] = counts[next - 1]
                    .checked_add(1)
                    .ok_or("invalid snapshot document")?;
                records = records.checked_add(1).ok_or("invalid snapshot document")?;
                if records > MAX_SNAPSHOT_RECORDS || counts[0] > 100_000 {
                    return Err("invalid snapshot document");
                }
                body_digest.update(&line);
                body_digest.update(b"\n");
            }
            "footer" => {
                if revision.is_none()
                    || checksum.is_some()
                    || !snapshot_has_keys(&map, &["type", "records", "sha256"])
                {
                    return Err("invalid snapshot document");
                }
                let expected_records = map
                    .get("records")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or("invalid snapshot document")?;
                let expected_sha = map
                    .get("sha256")
                    .and_then(Value::as_str)
                    .filter(|value| {
                        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                    .ok_or("invalid snapshot document")?;
                let actual = lower_hex(&body_digest.clone().finalize());
                if expected_records != records || expected_sha != actual {
                    return Err("invalid snapshot document");
                }
                checksum = Some(expected_sha.to_owned());
            }
            _ => return Err("invalid snapshot document"),
        }
        if !has_newline {
            break;
        }
    }
    let cloud_revision = revision.ok_or("invalid snapshot document")?;
    let sha256 = checksum.ok_or("invalid snapshot document")?;
    for (identity, entry_weight) in &identities.entry_keys {
        let Some((deleted, user_inserted, weight)) = identities.overlays.get(identity) else {
            return Err("invalid snapshot document");
        };
        if *deleted || !*user_inserted || *weight != *entry_weight {
            return Err("invalid snapshot document");
        }
    }
    for (identity, (deleted, user_inserted, weight)) in &identities.overlays {
        if !*deleted && *user_inserted {
            let Some(entry_weight) = identities.entry_keys.get(identity) else {
                return Err("invalid snapshot document");
            };
            if *entry_weight != *weight {
                return Err("invalid snapshot document");
            }
        }
    }
    Ok(SnapshotMetadata {
        cloud_revision,
        sha256,
        file_sha256: lower_hex(&file_digest.finalize()),
        bytes: total_bytes,
        records,
        entries: counts[0],
        overlays: counts[1],
        positions: counts[2],
        selections: counts[3],
        engine_records: counts[1] + counts[2] + counts[3],
    })
}

fn restore_snapshot_with(
    request: RestoreRequest,
    path: &Path,
    upload: impl FnOnce(&Path, i64, &str) -> Result<AccountDictionarySnapshotRestore, AccountError>,
) -> Result<Value, String> {
    if request.revision < 0
        || request.expected_sha256.len() != 64
        || !request
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("account_invalid".to_owned());
    }
    let metadata = inspect_snapshot(path).map_err(|_| "account_invalid".to_owned())?;
    if metadata.file_sha256 != request.expected_sha256 {
        return Err("account_invalid".to_owned());
    }
    let restored = upload(path, request.revision, &request.access_token)
        .map_err(|error| error.code().to_owned())?;
    serde_json::to_value(restored).map_err(|_| "account_unavailable".to_owned())
}

/// Called synchronously: positive UTF-8 JSON length, zero only at verified EOF,
/// negative for cancellation/corruption. It must not throw, unwind or retain buffer.
pub type SnapshotNext = unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> isize;

fn parse_options(bytes: &[u8]) -> Result<EngineOptions, &'static str> {
    if bytes.len() > REQUEST_LIMIT {
        return Err("invalid snapshot options");
    }
    let options: HostOptions =
        serde_json::from_slice(bytes).map_err(|_| "invalid snapshot options")?;
    validate_options(options)
}
fn validate_options(options: HostOptions) -> Result<EngineOptions, &'static str> {
    if options.api_version != 1 || options.preferences.validate().is_err() {
        return Err("invalid snapshot options");
    }
    Ok(options.into_engine_options())
}

fn version(options: &EngineOptions) -> Result<String, &'static str> {
    let _access = DictionaryAccess::try_session(
        Path::new(&options.user_data),
        Path::new(&options.dictionaries),
    )
    .map_err(|_| "snapshot access unavailable")?
    .ok_or("snapshot access busy")?;
    let mut hash = Sha256::new();
    hash.update(b"msime-host-dictionary-version-v1");
    for path in [
        &options.resources,
        &options.user_data,
        &options.cache,
        &options.dictionaries,
    ] {
        if !Path::new(path).is_absolute() {
            return Err("invalid snapshot path");
        }
        let canonical = Path::new(path)
            .canonicalize()
            .map_err(|_| "snapshot path unavailable")?;
        let text = canonical.to_str().ok_or("invalid snapshot path")?;
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(dictionary_state_revision(options).map_err(|_| "snapshot revision unavailable")?);
    Ok(lower_hex(&hash.finalize()))
}

fn valid_activation_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|&index| bytes[index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn activation_receipt(options: &EngineOptions) -> Result<Option<String>, &'static str> {
    let path = Path::new(&options.user_data).join(ACTIVATION_RECEIPT_NAME);
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("snapshot activation receipt unavailable"),
    };
    if file
        .metadata()
        .map_err(|_| "snapshot activation receipt unavailable")?
        .len()
        > MAX_ACTIVATION_RECEIPT_BYTES
    {
        return Err("invalid snapshot activation receipt");
    }
    let mut value = Vec::new();
    file.take(MAX_ACTIVATION_RECEIPT_BYTES + 1)
        .read_to_end(&mut value)
        .map_err(|_| "snapshot activation receipt unavailable")?;
    if value.len() as u64 > MAX_ACTIVATION_RECEIPT_BYTES {
        return Err("invalid snapshot activation receipt");
    }
    let value = std::str::from_utf8(&value).map_err(|_| "invalid snapshot activation receipt")?;
    if !valid_activation_id(value) {
        return Err("invalid snapshot activation receipt");
    }
    Ok(Some(value.to_owned()))
}

fn write_activation_receipt(
    options: &EngineOptions,
    activation_id: &str,
) -> Result<(), &'static str> {
    let directory = Path::new(&options.user_data);
    let temporary = directory.join(format!("{ACTIVATION_RECEIPT_NAME}.tmp"));
    let path = directory.join(ACTIVATION_RECEIPT_NAME);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| "snapshot activation receipt unavailable")?;
    file.write_all(activation_id.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "snapshot activation receipt unavailable")?;
    std::fs::rename(temporary, path).map_err(|_| "snapshot activation receipt unavailable")
}

fn prepare(
    request: PrepareRequest,
    specification: &ResourceSet,
    stream: impl Iterator<Item = Result<msime_engine_bridge::DictionaryStateRecord, SnapshotReadError>>
        + 'static,
) -> Result<Prepared, &'static str> {
    if request.records > 500_000 || request.expected_version.len() != 64 {
        return Err("invalid snapshot bounds");
    }
    if request
        .activation_id
        .as_deref()
        .is_some_and(|value| !valid_activation_id(value))
    {
        return Err("invalid snapshot activation id");
    }
    let options = validate_options(request.options)?;
    let current = version(&options)?;
    if current != request.expected_version {
        return Err("snapshot source changed");
    }
    let root = Path::new(&request.staging_root);
    if !root.is_absolute() {
        return Err("invalid snapshot staging root");
    }
    let root = root
        .canonicalize()
        .map_err(|_| "snapshot staging root unavailable")?;
    for path in [
        &options.resources,
        &options.user_data,
        &options.cache,
        &options.dictionaries,
    ] {
        let path = Path::new(path)
            .canonicalize()
            .map_err(|_| "snapshot path unavailable")?;
        if root.starts_with(&path) || path.starts_with(&root) {
            return Err("snapshot staging overlaps active paths");
        }
    }
    ResourceStore::new(&options.resources)
        .verify(Path::new(&options.resources), specification)
        .map_err(|_| "snapshot resources rejected")?;
    let content_id = specification
        .generation()
        .map_err(|_| "snapshot resources rejected")?;
    let directory = tempfile::Builder::new()
        .prefix("snapshot-")
        .tempdir_in(root)
        .map_err(|_| "snapshot staging unavailable")?;
    let generation = directory.path().join("generation");
    let mut count = 0;
    let expected = request.records;
    let mut source = stream;
    let checked = std::iter::from_fn(move || match source.next() {
        Some(Ok(record)) if count < expected => {
            count += 1;
            Some(Ok(record))
        }
        Some(_) => Some(Err(SnapshotReadError)),
        None if count == expected => None,
        None => Some(Err(SnapshotReadError)),
    });
    let staged = stage_dictionary_state(
        &options,
        generation.to_str().ok_or("invalid snapshot path")?,
        &content_id,
        expected.max(1),
        checked,
    )
    .map_err(|_| "snapshot preparation rejected")?;
    if let Some(activation_id) = request.activation_id.as_deref() {
        write_activation_receipt(&staged, activation_id)?;
    }
    // Learning may continue during expensive preparation. Reject a changed preview.
    if version(&options)? != current {
        return Err("snapshot source changed");
    }
    Ok(Prepared {
        directory,
        active_options: options,
        options: staged,
        source_version: current,
    })
}

fn activate(handle: u64, expected: &str) -> Result<Value, &'static str> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "snapshot registry unavailable")?;
    let prepared = entries.get(&handle).ok_or("unknown snapshot handle")?;
    if prepared.source_version != expected {
        return Err("snapshot source changed");
    }
    let active = &prepared.active_options;
    let staged = &prepared.options;
    let _access = DictionaryAccess::try_maintenance(
        Path::new(&active.user_data),
        Path::new(&active.dictionaries),
    )
    .map_err(|_| "snapshot access unavailable")?
    .ok_or("snapshot access busy")?;
    if version_without_access(active)? != expected {
        return Err("snapshot source changed");
    }
    // Construct the replacement engine while the current generation is still
    // untouched.  A malformed or otherwise unusable generation must not make
    // the active dictionaries unavailable after publication.
    let replacement_session = Session::new(staged).map_err(|_| "snapshot engine unavailable")?;
    // Windows cannot rename SQLite files while the probe keeps them open.
    drop(replacement_session);
    let suffix = format!(".msime-snapshot-old-{handle}");
    let pairs = [
        (&active.user_data, &staged.user_data),
        (&active.cache, &staged.cache),
        (&active.dictionaries, &staged.dictionaries),
    ];
    let backups: Vec<std::path::PathBuf> = pairs
        .iter()
        .map(|(current, _)| {
            let current = Path::new(current.as_str());
            current.with_file_name(format!(
                "{}{}",
                current
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("state"),
                suffix
            ))
        })
        .collect();
    // Swap each root's contents rather than the root itself.
    //
    // Renaming the roots cannot work on Windows: the maintenance guard holds
    // an open handle on a lock file inside them, and Windows refuses to rename
    // a directory containing any open handle - share mode does not help. So
    // activation has never succeeded there. Moving the entries leaves the lock
    // files exactly where they are, which is also what they are documented to
    // require: they are stable coordination objects, and renaming a root moved
    // one out from under every other process using it.
    let roots: Vec<&Path> = pairs
        .iter()
        .map(|(current, _)| Path::new(current.as_str()))
        .collect();
    let staged_roots: Vec<&Path> = pairs
        .iter()
        .map(|(_, replacement)| Path::new(replacement.as_str()))
        .collect();
    let mut moved: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    let rollback = |moved: &[(std::path::PathBuf, std::path::PathBuf)]| {
        for (from, to) in moved.iter().rev() {
            let _ = std::fs::rename(to, from);
        }
        for backup in &backups {
            discard_recovered_backup(backup);
        }
    };
    // An entry that leads to another root nested below this one is left alone:
    // that root does its own swap, and it holds its own lock file.
    let leads_to_nested_root = |root: &Path, entry: &Path, all: &[&Path]| {
        all.iter()
            .any(|other| *other != root && other.starts_with(entry))
    };
    for (index, (current, replacement)) in pairs.iter().enumerate() {
        let current = Path::new(current.as_str());
        let replacement = Path::new(replacement.as_str());
        let backup = &backups[index];
        if std::fs::create_dir_all(backup).is_err() {
            rollback(&moved);
            return Err("snapshot activation failed");
        }
        // Out with the old.
        let listing = match std::fs::read_dir(current) {
            Ok(listing) => listing,
            Err(_) => {
                rollback(&moved);
                return Err("snapshot activation failed");
            }
        };
        for entry in listing {
            let Ok(entry) = entry else {
                rollback(&moved);
                return Err("snapshot activation failed");
            };
            let path = entry.path();
            if entry.file_name() == DICTIONARY_ACCESS_LOCK_NAME
                || leads_to_nested_root(roots[index], &path, &roots)
            {
                continue;
            }
            let destination = backup.join(entry.file_name());
            if std::fs::rename(&path, &destination).is_err() {
                rollback(&moved);
                return Err("snapshot activation failed");
            }
            moved.push((path, destination));
        }
        // In with the new.
        let listing = match std::fs::read_dir(replacement) {
            Ok(listing) => listing,
            Err(_) => {
                rollback(&moved);
                return Err("snapshot activation failed");
            }
        };
        for entry in listing {
            let Ok(entry) = entry else {
                rollback(&moved);
                return Err("snapshot activation failed");
            };
            let path = entry.path();
            if entry.file_name() == DICTIONARY_ACCESS_LOCK_NAME
                || leads_to_nested_root(staged_roots[index], &path, &staged_roots)
            {
                continue;
            }
            let destination = current.join(entry.file_name());
            if std::fs::rename(&path, &destination).is_err() {
                rollback(&moved);
                return Err("snapshot activation failed");
            }
            moved.push((path, destination));
        }
    }
    for backup in &backups {
        let _ = std::fs::remove_dir_all(backup);
    }
    entries.remove(&handle);
    Ok(json!({"activated": true}))
}

/// Remove a backup directory only once rollback has emptied it.
///
/// `remove_dir` refuses a directory that still has anything in it, and that refusal is the point.
/// The renames that put the original contents back are best effort - one of them failing is
/// exactly the case where the backup is the only remaining copy of the user's dictionaries, and
/// `remove_dir_all` would delete it on the way out of a failure that had already been survived.
/// Leaving the directory on disk costs some space and keeps the data.
fn discard_recovered_backup(backup: &Path) {
    let _ = std::fs::remove_dir(backup);
}

fn version_without_access(options: &EngineOptions) -> Result<String, &'static str> {
    let mut hash = Sha256::new();
    hash.update(b"msime-host-dictionary-version-v1");
    for path in [
        &options.resources,
        &options.user_data,
        &options.cache,
        &options.dictionaries,
    ] {
        let canonical = Path::new(path)
            .canonicalize()
            .map_err(|_| "snapshot path unavailable")?;
        let text = canonical.to_str().ok_or("invalid snapshot path")?;
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(dictionary_state_revision(options).map_err(|_| "snapshot revision unavailable")?);
    Ok(lower_hex(&hash.finalize()))
}

fn register(prepared: Prepared) -> Result<Value, &'static str> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "snapshot registry unavailable")?;
    if entries.len() >= HANDLE_LIMIT {
        return Err("too many prepared snapshots");
    }
    let handle = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "snapshot handle unavailable")?;
    let output = json!({"handle": handle, "source_version": prepared.source_version});
    entries.insert(handle, prepared);
    Ok(output)
}

fn snapshot_queue_error(error: SnapshotQueueError) -> String {
    match error {
        SnapshotQueueError::Unavailable => "snapshot_unavailable",
        SnapshotQueueError::Busy => "snapshot_busy",
        SnapshotQueueError::Invalid => "snapshot_invalid",
        SnapshotQueueError::Conflict => "snapshot_conflict",
    }
    .to_owned()
}

fn snapshot_queue(directory: &str) -> Result<DictionarySnapshotQueue, String> {
    if directory.len() > 16_384 {
        return Err("snapshot_invalid".to_owned());
    }
    DictionarySnapshotQueue::new(directory).map_err(snapshot_queue_error)
}

fn durable_local_version(options: &EngineOptions) -> Result<String, &'static str> {
    let digest = version(options)?;
    let generation = activation_receipt(options)?;
    local_version(generation.as_deref(), &digest).map_err(|_| "invalid snapshot local version")
}

struct SnapshotFileRecords {
    reader: BufReader<std::fs::File>,
    line: Vec<u8>,
    failed: bool,
}

impl SnapshotFileRecords {
    fn open(path: &Path) -> Result<Self, &'static str> {
        let file = std::fs::File::open(path).map_err(|_| "snapshot file unavailable")?;
        Ok(Self {
            reader: BufReader::with_capacity(MAX_SNAPSHOT_LINE_BYTES, file),
            line: Vec::with_capacity(MAX_SNAPSHOT_LINE_BYTES),
            failed: false,
        })
    }

    fn read_line(&mut self) -> Result<bool, SnapshotReadError> {
        self.line.clear();
        loop {
            let chunk = self.reader.fill_buf().map_err(|_| SnapshotReadError)?;
            if chunk.is_empty() {
                return Ok(!self.line.is_empty());
            }
            if let Some(index) = chunk.iter().position(|byte| *byte == b'\n') {
                if self.line.len() + index + 1 > MAX_SNAPSHOT_LINE_BYTES {
                    return Err(SnapshotReadError);
                }
                self.line.extend_from_slice(&chunk[..index]);
                self.reader.consume(index + 1);
                return (!self.line.is_empty() && !self.line.ends_with(b"\r"))
                    .then_some(true)
                    .ok_or(SnapshotReadError);
            }
            if self.line.len() + chunk.len() >= MAX_SNAPSHOT_LINE_BYTES {
                return Err(SnapshotReadError);
            }
            self.line.extend_from_slice(chunk);
            let length = chunk.len();
            self.reader.consume(length);
        }
    }
}

impl Iterator for SnapshotFileRecords {
    type Item = Result<msime_engine_bridge::DictionaryStateRecord, SnapshotReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            match self.read_line() {
                Ok(false) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
                Ok(true) => {}
            }
            let kind = match parse_snapshot_object(&self.line)
                .ok()
                .and_then(|map| map.get("type").and_then(Value::as_str).map(str::to_owned))
            {
                Some(kind) => kind,
                None => {
                    self.failed = true;
                    return Some(Err(SnapshotReadError));
                }
            };
            match kind.as_str() {
                "overlay" | "position" | "selection" => {
                    return Some(record::decode(&self.line));
                }
                "header" | "entry" | "footer" => continue,
                _ => {
                    self.failed = true;
                    return Some(Err(SnapshotReadError));
                }
            }
        }
    }
}

fn snapshot_queue_state(
    queue: &DictionarySnapshotQueue,
    options: HostOptions,
    acknowledge: bool,
) -> Result<Value, String> {
    let options = validate_options(options).map_err(str::to_owned)?;
    let current = durable_local_version(&options).map_err(str::to_owned)?;
    queue
        .publish_local_version(&current)
        .map_err(snapshot_queue_error)?;
    let state = if acknowledge {
        queue.take_state()
    } else {
        queue.read()
    }
    .map_err(snapshot_queue_error)?;
    serde_json::to_value(state).map_err(|_| "snapshot_unavailable".to_owned())
}

fn snapshot_queue_process(
    queue: &DictionarySnapshotQueue,
    staging_root: String,
    options: HostOptions,
) -> Result<Value, String> {
    let engine_options = validate_options(options.clone()).map_err(str::to_owned)?;
    let current = durable_local_version(&engine_options).map_err(str::to_owned)?;
    queue
        .publish_local_version(&current)
        .map_err(snapshot_queue_error)?;
    let lease = match queue.acquire_worker_lease() {
        Ok(lease) => lease,
        Err(SnapshotQueueError::Busy) => {
            return serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
                .map_err(|_| "snapshot_unavailable".to_owned());
        }
        Err(error) => return Err(snapshot_queue_error(error)),
    };
    let Some(request) = queue.claim(&lease).map_err(snapshot_queue_error)? else {
        return serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
            .map_err(|_| "snapshot_unavailable".to_owned());
    };
    if request.expected_local_version != current {
        let _ = queue
            .complete(request.id, &lease, &current, false, || {
                Err(SnapshotQueueError::Conflict)
            })
            .map_err(snapshot_queue_error)?;
        return serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
            .map_err(|_| "snapshot_unavailable".to_owned());
    }
    let path = queue.file_path(request.id).map_err(snapshot_queue_error)?;
    let metadata = match inspect_snapshot(&path) {
        Ok(metadata) if metadata.file_sha256 == request.file_sha256 => metadata,
        _ => {
            queue
                .fail(request.id, &lease)
                .map_err(snapshot_queue_error)?;
            return serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
                .map_err(|_| "snapshot_unavailable".to_owned());
        }
    };
    let raw_expected = local_version_digest(&request.expected_local_version)
        .map_err(snapshot_queue_error)?
        .to_owned();
    let stream = SnapshotFileRecords::open(&path).map_err(str::to_owned)?;
    let specification: ResourceSet = serde_json::from_str(include_str!(
        "../../../resources/desktop-dictionary.lock.json"
    ))
    .map_err(|_| "snapshot resources rejected".to_owned())?;
    let prepared = prepare(
        PrepareRequest {
            options,
            staging_root,
            expected_version: raw_expected.clone(),
            records: metadata.engine_records,
            activation_id: Some(request.id.to_string()),
        },
        &specification,
        stream,
    );
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => {
            let latest = durable_local_version(&engine_options).map_err(str::to_owned)?;
            if latest != current {
                let _ = queue
                    .complete(request.id, &lease, &latest, false, || {
                        Err(SnapshotQueueError::Conflict)
                    })
                    .map_err(snapshot_queue_error)?;
            } else {
                queue
                    .fail(request.id, &lease)
                    .map_err(snapshot_queue_error)?;
            }
            return serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
                .map_err(|_| "snapshot_unavailable".to_owned());
        }
    };
    let registered = register(prepared).map_err(str::to_owned)?;
    let handle = registered
        .get("handle")
        .and_then(Value::as_u64)
        .ok_or_else(|| "snapshot_unavailable".to_owned())?;
    let mut consumed = false;
    let completion = queue.complete(request.id, &lease, &current, false, || {
        activate(handle, &raw_expected).map_err(|_| SnapshotQueueError::Unavailable)?;
        consumed = true;
        durable_local_version(&engine_options).map_err(|_| SnapshotQueueError::Unavailable)
    });
    if !consumed {
        let _ = discard(handle);
    }
    completion.map_err(snapshot_queue_error)?;
    serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
        .map_err(|_| "snapshot_unavailable".to_owned())
}

fn run_snapshot_queue(action: SnapshotQueueAction) -> Result<Value, String> {
    match action {
        SnapshotQueueAction::State {
            directory,
            options,
            acknowledge,
        } => snapshot_queue_state(&snapshot_queue(&directory)?, options, acknowledge),
        SnapshotQueueAction::Enqueue {
            directory,
            source,
            account_id,
            cloud_revision,
            expected_local_version,
            file_sha256,
        } => {
            let queue = snapshot_queue(&directory)?;
            queue
                .enqueue(
                    Path::new(&source),
                    &account_id,
                    cloud_revision,
                    &expected_local_version,
                    &file_sha256,
                )
                .map_err(snapshot_queue_error)?;
            serde_json::to_value(queue.read().map_err(snapshot_queue_error)?)
                .map_err(|_| "snapshot_unavailable".to_owned())
        }
        SnapshotQueueAction::Cancel {
            directory,
            account_id,
        } => {
            let queue = snapshot_queue(&directory)?;
            queue.cancel(&account_id).map_err(snapshot_queue_error)?;
            serde_json::to_value(queue.take_state().map_err(snapshot_queue_error)?)
                .map_err(|_| "snapshot_unavailable".to_owned())
        }
        SnapshotQueueAction::Process {
            directory,
            staging_root,
            options,
        } => snapshot_queue_process(&snapshot_queue(&directory)?, staging_root, options),
    }
}

/// Persist, inspect, process or query the one crash-safe native snapshot queue.
/// # Safety
/// `request` points to `length` readable UTF-8 JSON bytes.
#[no_mangle]
pub unsafe extern "C" fn msime_client_snapshot_queue(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        if request.is_null() || length == 0 || length > REQUEST_LIMIT {
            return Err("snapshot_invalid".to_owned());
        }
        let action: SnapshotQueueAction =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(request, length) })
                .map_err(|_| "snapshot_invalid".to_owned())?;
        run_snapshot_queue(action)
    })
}

/// Inspect one host-private snapshot file without returning its contents.
/// # Safety
/// `path` points to `length` readable UTF-8 bytes naming an absolute file path.
#[no_mangle]
pub unsafe extern "C" fn msime_client_snapshot_inspect(
    path: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        if path.is_null() || length == 0 || length > 16_384 {
            return Err("invalid snapshot path".into());
        }
        let text = std::str::from_utf8(unsafe { std::slice::from_raw_parts(path, length) })
            .map_err(|_| "invalid snapshot path")?;
        let path = Path::new(text);
        if !path.is_absolute() {
            return Err("invalid snapshot path".into());
        }
        inspect_snapshot(path)
            .and_then(|metadata| serde_json::to_value(metadata).map_err(|_| "snapshot unavailable"))
            .map_err(Into::into)
    })
}

/// Reinspect and upload one host-private snapshot file without buffering it in the host bridge.
/// # Safety
/// `request` points to `request_length` readable JSON bytes and `path` points to
/// `path_length` readable UTF-8 bytes naming an absolute private file path.
#[no_mangle]
pub unsafe extern "C" fn msime_client_snapshot_restore(
    request: *const u8,
    request_length: usize,
    path: *const u8,
    path_length: usize,
) -> *mut c_char {
    response(|| {
        if request.is_null()
            || request_length == 0
            || request_length > BUFFER_LIMIT
            || path.is_null()
            || path_length == 0
            || path_length > 16_384
        {
            return Err("account_invalid".to_owned());
        }
        let request: RestoreRequest =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(request, request_length) })
                .map_err(|_| "account_invalid".to_owned())?;
        let path = std::str::from_utf8(unsafe { std::slice::from_raw_parts(path, path_length) })
            .map_err(|_| "account_invalid".to_owned())?;
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err("account_invalid".to_owned());
        }
        let client = BackendAccountClient::new().map_err(|error| error.code().to_owned())?;
        restore_snapshot_with(request, path, |path, revision, access_token| {
            client.restore_dictionary_snapshot_file(path, revision, access_token)
        })
    })
}

/// Read a preview version binding canonical paths and the consistent Engine journal.
/// # Safety
/// `options` points to `length` readable bytes. Trusted native paths only.
#[no_mangle]
pub unsafe extern "C" fn msime_client_snapshot_version(
    options: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        if options.is_null() || length > REQUEST_LIMIT {
            return Err("invalid snapshot buffer".into());
        }
        let options = parse_options(unsafe { std::slice::from_raw_parts(options, length) })?;
        let version = version(&options)?;
        let generation = activation_receipt(&options)?.unwrap_or_else(|| "legacy".to_owned());
        Ok(json!({"version": version, "generation": generation}))
    })
}

/// Prepare from a native callback; holds no registry lock while calling the host.
/// # Safety
/// Request/context remain valid for this synchronous call. The callback obeys
/// SnapshotNext, writes at most capacity bytes, and does not unwind or retain buffer.
#[no_mangle]
pub unsafe extern "C" fn msime_client_snapshot_prepare(
    request: *const u8,
    length: usize,
    next: Option<SnapshotNext>,
    context: *mut c_void,
) -> *mut c_char {
    response(|| {
        if request.is_null() || length > REQUEST_LIMIT {
            return Err("invalid snapshot buffer".into());
        }
        let next = next.ok_or("missing snapshot reader")?;
        let request: PrepareRequest =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(request, length) })
                .map_err(|_| "invalid snapshot request")?;
        let specification: ResourceSet = serde_json::from_str(include_str!(
            "../../../resources/desktop-dictionary.lock.json"
        ))
        .map_err(|_| "snapshot resources rejected")?;
        let mut buffer = vec![0; BUFFER_LIMIT];
        let stream = std::iter::from_fn(move || {
            let length = unsafe { next(context, buffer.as_mut_ptr(), buffer.len()) };
            if length == 0 {
                return None;
            }
            if length < 0 || length as usize > buffer.len() {
                return Some(Err(SnapshotReadError));
            }
            Some(record::decode(&buffer[..length as usize]))
        });
        let prepared = prepare(request, &specification, stream)?;
        register(prepared).map_err(Into::into)
    })
}

/// Discard only a process-owned, unpublished preparation. Unknown/consumed IDs fail.
fn discard(handle: u64) -> Result<Value, &'static str> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "snapshot registry unavailable")?;
    let prepared = entries.get(&handle).ok_or("unknown snapshot handle")?;
    // A prepared directory is outside all active roots (validated by prepare),
    // so cleaning it never touches the live journal or dictionaries. Requiring
    // the maintenance lock here made cancellation fail while an input session
    // held its normal shared lock, leaking the process-owned handle.
    std::fs::remove_dir_all(prepared.directory.path()).map_err(|_| "snapshot cleanup failed")?;
    entries.remove(&handle);
    Ok(json!({"discarded": true}))
}

#[no_mangle]
pub extern "C" fn msime_client_snapshot_discard(handle: u64) -> *mut c_char {
    response(|| discard(handle).map_err(|error| error.to_string()))
}

#[no_mangle]
pub extern "C" fn msime_client_snapshot_activate(
    handle: u64,
    expected: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        if expected.is_null() || length != 64 {
            return Err("invalid snapshot version".into());
        }
        let expected = std::str::from_utf8(unsafe { std::slice::from_raw_parts(expected, length) })
            .map_err(|_| "invalid snapshot version")?;
        activate(handle, expected).map_err(Into::into)
    })
}

// sha2 0.11 digests no longer implement `LowerHex`, and this crate has no hex dependency for a handful of call sites.
fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
