//! Private aggregate typing statistics shared by native hosts and settings UI.
//! Committed text is classified in memory and is never serialized.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

/// Only a guard against loading a hostile or garbage file, not a retention limit. It must stay far above anything `Forever` can produce, because a document over it cannot be read at all and the whole history is lost with it; a day costs a few hundred bytes, so 64 MiB covers centuries.
const MAX_DOCUMENT_BYTES: u64 = 64 * 1_048_576;
const MAX_COMMIT_BYTES: usize = 40_000;
const MAX_COMMIT_SCALARS: usize = 10_000;
const MAX_COUNT: u64 = 9_000_000_000_000_000;
/// Buckets in a day, one per local hour.
pub const HOURS: usize = 24;
/// A day cannot hold more active time than it has milliseconds.
const MAX_ACTIVE_MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
/// A pause of at most this much between two consecutive commits counts as active typing time.
///
/// Taken from the Windows baseline, which calibrated it on real input: at five seconds ordinary
/// thinking pauses were counted as typing and the speed reading came out too low. It is the one
/// number here that decides what "active" means, so it is a constant with a reason rather than a
/// literal in the middle of `record`.
const ACTIVE_GAP_LIMIT_MS: u64 = 10_000;

/// Statistics are off until the user turns them on.
///
/// The Windows baseline ships them disabled and says so in its own feature list, and it is the
/// right way round for something that counts what a person types: a feature like this should be
/// asked for rather than opted out of. A document written before this field existed keeps
/// whatever it says; only a fresh profile gets the default.
fn enabled_by_default() -> bool {
    false
}

/// How long recorded days are kept.
///
/// Copied from the Windows baseline's `[statistics] retention`, including that an unrecognised
/// value is read as `Forever`: a preference this side does not understand must not be taken as
/// permission to delete anything.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatisticsRetention {
    #[default]
    #[serde(rename = "forever")]
    Forever,
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "90d")]
    Days90,
    #[serde(rename = "180d")]
    Days180,
    #[serde(rename = "365d")]
    Days365,
}

impl StatisticsRetention {
    /// The window in days, or `None` for "keep everything".
    pub fn days(self) -> Option<u32> {
        match self {
            Self::Forever => None,
            Self::Days30 => Some(30),
            Self::Days90 => Some(90),
            Self::Days180 => Some(180),
            Self::Days365 => Some(365),
        }
    }

    /// Parse the stored spelling. Anything else is `Forever`, never a shorter window.
    pub fn parse(value: &str) -> Self {
        match value {
            "30d" => Self::Days30,
            "90d" => Self::Days90,
            "180d" => Self::Days180,
            "365d" => Self::Days365,
            _ => Self::Forever,
        }
    }
}

/// An unknown retention value is `Forever` rather than a parse failure: a damaged or newer
/// preference must not make the whole document unreadable, and must never delete more.
fn retention_or_forever<'de, D>(deserializer: D) -> Result<StatisticsRetention, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer).unwrap_or_default();
    Ok(StatisticsRetention::parse(&value))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TypingSource {
    Quanpin,
    NineKey,
    Shuangpin,
    Ziranma,
    Microsoft,
    Shoudao,
    Wubi,
    Japanese,
    Handwriting,
    English,
    Local,
    Ai,
    Reply,
    Voice,
    Unknown,
}

impl TypingSource {
    fn id(self) -> &'static str {
        match self {
            Self::Quanpin => "quanpin",
            Self::NineKey => "nineKey",
            Self::Shuangpin => "shuangpin",
            Self::Ziranma => "ziranma",
            Self::Microsoft => "microsoft",
            Self::Shoudao => "shoudao",
            Self::Wubi => "wubi",
            Self::Japanese => "japanese",
            Self::Handwriting => "handwriting",
            Self::English => "english",
            Self::Local => "local",
            Self::Ai => "ai",
            Self::Reply => "reply",
            Self::Voice => "voice",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypingBreakdown {
    #[serde(default)]
    pub characters: BTreeMap<String, u64>,
    #[serde(default)]
    pub sources: BTreeMap<String, u64>,
}

impl TypingBreakdown {
    fn add(&mut self, character: &str, source: TypingSource) -> Result<(), TypingStatisticsError> {
        checked_increment(&mut self.characters, character, 1)?;
        checked_increment(&mut self.sources, source.id(), 1)
    }

    fn merge(&mut self, other: &Self) -> Result<(), TypingStatisticsError> {
        for (key, count) in &other.characters {
            checked_increment(&mut self.characters, key, *count)?;
        }
        for (key, count) in &other.sources {
            checked_increment(&mut self.sources, key, *count)?;
        }
        Ok(())
    }

    pub fn including_unclassified(&self, total: u64) -> Self {
        let mut value = self.clone();
        let character_total = value
            .characters
            .values()
            .fold(0_u64, |sum, count| sum.saturating_add(*count));
        let source_total = value
            .sources
            .values()
            .fold(0_u64, |sum, count| sum.saturating_add(*count));
        *value.characters.entry("unknown".to_owned()).or_default() +=
            total.saturating_sub(character_total);
        *value.sources.entry("unknown".to_owned()).or_default() +=
            total.saturating_sub(source_total);
        value
    }
}

/// Where in the candidate list a commit came from, counted and nothing else.
///
/// This is the field counterpart of the evaluation sets' top-1: `ranks[0]` over the total is how
/// often the first candidate was the one wanted, measured on what the user actually types rather
/// than on 60 hand-written sentences. No text, no pinyin and no context are involved, which is
/// what makes it safe to keep — the same rule the rest of this module follows.
///
/// A candidate page holds nine, so ranks past that are counted together: beyond the first page the
/// distinction between the eleventh and the twelfth says nothing anyone would act on.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectionCounts {
    /// Commits from positions 1 through `RANKS`, `ranks[0]` being the first candidate.
    #[serde(default)]
    pub ranks: Vec<u64>,
    /// Commits from further down the list than `RANKS`.
    #[serde(default)]
    pub beyond: u64,
}

/// One candidate page. Positions past this are counted in `beyond`.
pub const RANKS: usize = 9;

impl SelectionCounts {
    fn add(&mut self, position: usize, count: u64) -> Result<(), TypingStatisticsError> {
        if position == 0 {
            return Err(TypingStatisticsError::InvalidPosition);
        }
        if position > RANKS {
            self.beyond = self
                .beyond
                .checked_add(count)
                .filter(|count| *count <= MAX_COUNT)
                .ok_or(TypingStatisticsError::CountExhausted)?;
            return Ok(());
        }
        if self.ranks.len() < RANKS {
            self.ranks.resize(RANKS, 0);
        }
        let slot = &mut self.ranks[position - 1];
        *slot = slot
            .checked_add(count)
            .filter(|count| *count <= MAX_COUNT)
            .ok_or(TypingStatisticsError::CountExhausted)?;
        Ok(())
    }

    /// Commits counted here, which is the denominator for any rate drawn from `ranks`.
    pub fn total(&self) -> u64 {
        self.ranks
            .iter()
            .fold(self.beyond, |sum, count| sum.saturating_add(*count))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypingStatistics {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub days: BTreeMap<String, u64>,
    #[serde(default)]
    pub detail: TypingBreakdown,
    #[serde(default)]
    pub daily_details: BTreeMap<String, TypingBreakdown>,
    /// Absent from files written before this existed, which `default` turns into an empty
    /// histogram rather than a parse failure.
    ///
    /// Aggregate only, with no per-day axis, and that is a decision rather than an omission. A
    /// day here would have to be the user's day to sit beside `daily_details`, and the host is
    /// the only thing that knows which day that is — `record` takes one as an argument for
    /// exactly that reason. Candidate selection reaches this crate through a call that carries no
    /// day and would need six platform signatures changed to carry one, and a UTC day quietly
    /// disagreeing with the local day next to it is worse than no axis at all. The rate this
    /// exists to give — how often the first candidate was the right one — does not need one.
    #[serde(default)]
    pub selections: SelectionCounts,
    /// Active typing time per day, in milliseconds.
    ///
    /// Active means the gap to the previous commit was positive and no longer than
    /// [`ACTIVE_GAP_LIMIT_MS`]; anything longer is a break and contributes nothing. It exists to
    /// be a denominator: characters alone say how much was typed, not how fast, and wall-clock
    /// time between the first and last commit of a day would divide by the whole working day.
    ///
    /// A day absent here has no measured active time, which is not the same as zero characters —
    /// documents written before this existed have counts for their days and no entry here, and
    /// every metric derived from it has to treat that as "unknown" rather than "instant".
    #[serde(default)]
    pub daily_active_ms: BTreeMap<String, u64>,
    /// Characters per local hour, [`HOURS`] buckets per day.
    ///
    /// The hour comes from the host for the same reason the day does: only the host knows which
    /// timezone the user is in, and an hour axis quietly disagreeing with the day beside it would
    /// be worse than no axis. A host that does not send one still records characters; its days
    /// simply have no hourly breakdown.
    #[serde(default)]
    pub daily_hours: BTreeMap<String, Vec<u64>>,
    /// Milliseconds since the Unix epoch of the last counted commit.
    ///
    /// State, not history: every commit overwrites it, so it says when typing last happened and
    /// nothing about what was typed or when anything before it was. It has to be in the file
    /// because the store is constructed per call and has nowhere else to keep the previous
    /// commit's instant, which is the only thing the gap can be measured against.
    #[serde(default)]
    pub last_commit_ms: u64,
    /// How long recorded days are kept.
    #[serde(default, deserialize_with = "retention_or_forever")]
    pub retention: StatisticsRetention,
    /// The last day the retention window was applied.
    ///
    /// The baseline prunes on the first write of each day rather than on every write, so this is
    /// what "first" is measured against. It is a day key, not a clock reading.
    #[serde(default)]
    pub last_pruned_day: String,
}

impl Default for TypingStatistics {
    fn default() -> Self {
        Self {
            // Same answer as the serde default, and it has to be: this is what a missing file
            // returns, which is exactly the fresh profile the default is about.
            enabled: enabled_by_default(),
            total: 0,
            days: BTreeMap::new(),
            detail: TypingBreakdown::default(),
            daily_details: BTreeMap::new(),
            selections: SelectionCounts::default(),
            daily_active_ms: BTreeMap::new(),
            daily_hours: BTreeMap::new(),
            last_commit_ms: 0,
            retention: StatisticsRetention::Forever,
            last_pruned_day: String::new(),
        }
    }
}

impl TypingStatistics {
    pub fn breakdown(&self, days: Option<&[String]>) -> TypingBreakdown {
        let Some(days) = days else {
            return self.detail.including_unclassified(self.total);
        };
        let mut result = TypingBreakdown::default();
        let mut total = 0_u64;
        for day in days {
            total = total.saturating_add(self.days.get(day).copied().unwrap_or(0));
            if let Some(detail) = self.daily_details.get(day) {
                let _ = result.merge(detail);
            }
        }
        result.including_unclassified(total)
    }

    fn validate(&self) -> Result<(), TypingStatisticsError> {
        // No cap on the number of days: `Forever` keeps every day, as the baseline's stats_daily does, and the document size limit in `read_locked` is what bounds a file.
        if self.total > MAX_COUNT {
            return Err(TypingStatisticsError::InvalidDocument);
        }
        validate_counts(&self.detail, self.total)?;
        for (day, count) in &self.days {
            validate_day(day)?;
            if *count > self.total {
                return Err(TypingStatisticsError::InvalidDocument);
            }
            if let Some(detail) = self.daily_details.get(day) {
                validate_counts(detail, *count)?;
            }
        }
        if self
            .daily_details
            .keys()
            .any(|day| !self.days.contains_key(day))
        {
            return Err(TypingStatisticsError::InvalidDocument);
        }
        for (day, active_ms) in &self.daily_active_ms {
            // A day that has active time but no characters is not a document this code can
            // produce, and letting it through would put a day on the calendar that nobody typed
            // on.
            if !self.days.contains_key(day) || *active_ms > MAX_ACTIVE_MS_PER_DAY {
                return Err(TypingStatisticsError::InvalidDocument);
            }
        }
        for (day, hours) in &self.daily_hours {
            let Some(total) = self.days.get(day) else {
                return Err(TypingStatisticsError::InvalidDocument);
            };
            // Not equality: days recorded before hosts sent an hour have characters and no
            // buckets, so the buckets can only ever be a subset of the day.
            if hours.len() != HOURS
                || hours
                    .iter()
                    .try_fold(0_u64, |sum, count| sum.checked_add(*count))
                    .is_none_or(|sum| sum > *total)
            {
                return Err(TypingStatisticsError::InvalidDocument);
            }
        }
        Ok(())
    }

    /// Drop every recorded day outside the retention window, counting back from `today`.
    ///
    /// The comparison is on the day key, which sorts as a date because it is `YYYY-MM-DD`; no
    /// calendar arithmetic is needed beyond producing the boundary. `Forever` removes nothing,
    /// and a day in the future - a clock that was wrong when it was recorded - is kept rather
    /// than silently deleted, because the alternative is losing real typing to a bad clock.
    pub fn apply_retention(&mut self, today: &str) {
        let Some(days) = self.retention.days() else {
            return;
        };
        let Some(boundary) = day_before(today, days) else {
            return;
        };
        // Like the baseline's ClearThrough, which deletes the stats_daily rows its overview sums, a cleanup takes the pruned days out of the running totals too, so "累计", the category split and the daily average cover the retained window. A legacy day without a breakdown only lowers `total`; `breakdown(None)` reports the rest as unclassified.
        for (_, count) in self.days.range(..boundary.clone()) {
            self.total = self.total.saturating_sub(*count);
        }
        for (_, detail) in self.daily_details.range(..boundary.clone()) {
            for (key, count) in &detail.characters {
                if let Some(value) = self.detail.characters.get_mut(key) {
                    *value = value.saturating_sub(*count);
                }
            }
            for (key, count) in &detail.sources {
                if let Some(value) = self.detail.sources.get_mut(key) {
                    *value = value.saturating_sub(*count);
                }
            }
        }
        self.days.retain(|day, _| *day >= boundary);
        self.daily_details.retain(|day, _| *day >= boundary);
        self.daily_active_ms.retain(|day, _| *day >= boundary);
        self.daily_hours.retain(|day, _| *day >= boundary);
        // Subtraction keeps whatever `total` and `detail` hold beyond the per-day records, which a document written by an older build can have. Where that leaves the counters out of step with each other - `total` below the retained days, or a category sum above `total` - validate() would reject the document this write produces, so fall back to what the retained days themselves say.
        let retained = self
            .days
            .values()
            .fold(0_u64, |sum, count| sum.saturating_add(*count));
        self.total = self.total.max(retained);
        let sum = |values: &BTreeMap<String, u64>| {
            values
                .values()
                .fold(0_u64, |sum, count| sum.saturating_add(*count))
        };
        if sum(&self.detail.characters) > self.total || sum(&self.detail.sources) > self.total {
            let mut rebuilt = TypingBreakdown::default();
            for detail in self.daily_details.values() {
                let _ = rebuilt.merge(detail);
            }
            self.detail = rebuilt;
        }
    }

    /// Active milliseconds recorded for `day`, or `None` when that day predates the measurement.
    pub fn active_ms(&self, day: &str) -> Option<u64> {
        self.daily_active_ms.get(day).copied()
    }

    /// The day's per-hour character counts, or `None` when the host sent no hour for it.
    pub fn hours(&self, day: &str) -> Option<&[u64]> {
        self.daily_hours.get(day).map(Vec::as_slice)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TypingStatisticsError {
    #[error("typing statistics storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid typing statistics document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid typing statistics day")]
    InvalidDay,
    #[error("typing statistics commit is too large")]
    CommitTooLarge,
    #[error("typing statistics document is invalid")]
    InvalidDocument,
    #[error("typing statistics count exhausted")]
    CountExhausted,
    #[error("candidate position is not one-based")]
    InvalidPosition,
}

#[derive(Clone, Debug)]
pub struct TypingStatisticsStore {
    directory: PathBuf,
}

impl TypingStatisticsStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Where the statistics file lives, for hosts that offer to reveal it in a file manager.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn path(&self) -> PathBuf {
        self.directory.join("typing-statistics.json")
    }

    fn lock(&self) -> Result<File, TypingStatisticsError> {
        fs::create_dir_all(&self.directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("typing-statistics.lock"))?;
        crate::file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn read_locked(&self) -> Result<TypingStatistics, TypingStatisticsError> {
        let path = self.path();
        let bytes = match File::open(&path) {
            Ok(file) => read_bounded_document(file)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TypingStatistics::default());
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(TypingStatisticsError::InvalidDocument);
        }
        let value: TypingStatistics = serde_json::from_slice(&bytes)?;
        value.validate()?;
        Ok(value)
    }

    fn write_locked(&self, value: &TypingStatistics) -> Result<(), TypingStatisticsError> {
        value.validate()?;
        let bytes = serde_json::to_vec(value)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path())
            .map(|_| ())
            .map_err(|error| TypingStatisticsError::Io(error.error))
    }

    /// Moves a valid legacy statistics document into this store without
    /// replacing a document already created by the shared host.
    pub fn migrate_from(
        &self,
        legacy_directory: impl AsRef<Path>,
    ) -> Result<bool, TypingStatisticsError> {
        let legacy_directory = legacy_directory.as_ref();
        if legacy_directory == self.directory {
            return Ok(false);
        }
        let _destination_lock = self.lock()?;
        if self.path().try_exists()? {
            return Ok(false);
        }

        let legacy = Self::new(legacy_directory);
        let _legacy_lock = legacy.lock()?;
        if self.path().try_exists()? || !legacy.path().try_exists()? {
            return Ok(false);
        }
        let _ = legacy.read_locked()?;
        fs::rename(legacy.path(), self.path())?;
        Ok(true)
    }

    pub fn load(&self) -> Result<TypingStatistics, TypingStatisticsError> {
        let _lock = self.lock()?;
        self.read_locked()
    }

    pub fn last_written(&self) -> Result<Option<SystemTime>, TypingStatisticsError> {
        match fs::metadata(self.path()) {
            Ok(metadata) => Ok(metadata.modified().ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Count one commit's characters against `day`, and the gap since the previous commit as
    /// active time.
    ///
    /// `hour` is the local hour the commit happened in. `None` records the characters without an
    /// hourly breakdown, which is what a host that cannot resolve a local hour should send rather
    /// than guessing one.
    pub fn record(
        &self,
        text: &str,
        source: TypingSource,
        day: &str,
        hour: Option<u8>,
    ) -> Result<u64, TypingStatisticsError> {
        self.record_at(text, source, day, hour, epoch_millis(SystemTime::now()))
    }

    /// `record` with the instant supplied, so the active-time rules can be tested without
    /// sleeping. Production always passes the current time.
    pub fn record_at(
        &self,
        text: &str,
        source: TypingSource,
        day: &str,
        hour: Option<u8>,
        now_ms: u64,
    ) -> Result<u64, TypingStatisticsError> {
        validate_day(day)?;
        if text.len() > MAX_COMMIT_BYTES || text.chars().count() > MAX_COMMIT_SCALARS {
            return Err(TypingStatisticsError::CommitTooLarge);
        }
        let _lock = self.lock()?;
        let mut value = self.read_locked()?;
        if !value.enabled {
            return Ok(0);
        }
        let mut addition = TypingBreakdown::default();
        let mut count = 0_u64;
        for grapheme in text.graphemes(true) {
            if grapheme.chars().all(char::is_whitespace) {
                continue;
            }
            addition.add(classify(grapheme), source)?;
            count += 1;
        }
        if count == 0 {
            return Ok(0);
        }
        value.total = value
            .total
            .checked_add(count)
            .filter(|total| *total <= MAX_COUNT)
            .ok_or(TypingStatisticsError::CountExhausted)?;
        checked_increment(&mut value.days, day, count)?;
        value.detail.merge(&addition)?;
        value
            .daily_details
            .entry(day.to_owned())
            .or_default()
            .merge(&addition)?;

        // Attribute the gap to the day and hour of *this* commit, the way the Windows baseline
        // does: the pause belongs to the typing it precedes, and a session that crosses midnight
        // therefore leaves its last pause on the new day rather than extending the old one.
        let active_ms = active_gap_ms(value.last_commit_ms, now_ms);
        if active_ms > 0 {
            let day_active = value.daily_active_ms.entry(day.to_owned()).or_default();
            *day_active = day_active
                .saturating_add(active_ms)
                .min(MAX_ACTIVE_MS_PER_DAY);
        }
        // Never moves backwards. A clock set back would otherwise make every later commit look
        // like it followed a huge pause, and the first one after the correction would be counted
        // as a fresh session instead of the continuation it is.
        if now_ms > value.last_commit_ms {
            value.last_commit_ms = now_ms;
        }

        if let Some(hour) = hour.filter(|hour| usize::from(*hour) < HOURS) {
            let buckets = value
                .daily_hours
                .entry(day.to_owned())
                .or_insert_with(|| vec![0; HOURS]);
            // A file edited by hand could carry a short vector; resize rather than panic on the
            // index, because a malformed bucket list must not cost the user the count itself.
            if buckets.len() != HOURS {
                buckets.resize(HOURS, 0);
            }
            let bucket = &mut buckets[usize::from(hour)];
            *bucket = bucket
                .checked_add(count)
                .filter(|count| *count <= MAX_COUNT)
                .ok_or(TypingStatisticsError::CountExhausted)?;
        }

        // The retention setting is the only thing that deletes days, matching the baseline's
        // RetentionCutoff/ClearThrough: `Forever` keeps every one, and `MAX_DOCUMENT_BYTES` is what
        // bounds the file. It runs on the first write of each day; doing it on every write would
        // read the whole history on every commit.
        if value.last_pruned_day != day {
            value.apply_retention(day);
            value.last_pruned_day = day.to_owned();
        }
        self.write_locked(&value)?;
        Ok(count)
    }

    /// Count one commit by the one-based position it was chosen from.
    ///
    /// Separate from `record` because the two count different things: `record` counts characters,
    /// this counts commits, and dividing one by the other would mean nothing. The enable flag and
    /// the lock are shared, so turning statistics off turns this off with them and no second
    /// switch appears in settings for a user to misread.
    pub fn record_selection(&self, position: usize) -> Result<(), TypingStatisticsError> {
        self.record_selections(&[(position, 1)])
    }

    /// Count several commits at once, each `(position, count)` pair adding `count` commits from that one-based position, under one lock, one read and at most one write.
    ///
    /// This is what lets a host keep selections in memory and hand them over in batches instead of paying a full read, fsync and rename per selection. An empty batch touches nothing on disk. The batch is applied whole or not at all: an invalid position or an exhausted count leaves the document as it was. Statistics being off drops the batch without writing, the same answer `record_selection` gives.
    pub fn record_selections(
        &self,
        selections: &[(usize, u64)],
    ) -> Result<(), TypingStatisticsError> {
        if selections.iter().all(|(_, count)| *count == 0) {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut value = self.read_locked()?;
        if !value.enabled {
            return Ok(());
        }
        for &(position, count) in selections {
            value.selections.add(position, count)?;
        }
        self.write_locked(&value)?;
        Ok(())
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<TypingStatistics, TypingStatisticsError> {
        let _lock = self.lock()?;
        let mut value = self.read_locked()?;
        value.enabled = enabled;
        self.write_locked(&value)?;
        Ok(value)
    }

    /// Choose how long recorded days are kept.
    ///
    /// `today` is the caller's local day, for the same reason `record` takes one. A window that
    /// has just been narrowed applies immediately rather than at the next day boundary: the user
    /// asked for those days to be gone, and waiting would leave them visible on the page they
    /// asked from.
    pub fn set_retention(
        &self,
        retention: StatisticsRetention,
        today: &str,
    ) -> Result<TypingStatistics, TypingStatisticsError> {
        validate_day(today)?;
        let _lock = self.lock()?;
        let mut value = self.read_locked()?;
        value.retention = retention;
        value.apply_retention(today);
        value.last_pruned_day = today.to_owned();
        self.write_locked(&value)?;
        Ok(value)
    }

    pub fn reset(&self) -> Result<TypingStatistics, TypingStatisticsError> {
        let _lock = self.lock()?;
        let mut value = self.read_locked()?;
        value.total = 0;
        value.days.clear();
        value.detail = TypingBreakdown::default();
        value.daily_details.clear();
        // Reset means reset. Leaving the selection histogram behind would keep counting after a
        // user asked for it to stop existing, which is the one thing this module must not do.
        value.selections = SelectionCounts::default();
        value.daily_active_ms.clear();
        value.daily_hours.clear();
        // Including when typing last happened: it is the only field that survives a reset by
        // saying anything about the user at all.
        value.last_commit_ms = 0;
        self.write_locked(&value)?;
        Ok(value)
    }
}

fn read_bounded_document(file: File) -> Result<Vec<u8>, TypingStatisticsError> {
    if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
        return Err(TypingStatisticsError::InvalidDocument);
    }
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(TypingStatisticsError::InvalidDocument);
    }
    Ok(bytes)
}

/// Milliseconds since the Unix epoch, saturating at zero for clocks set before 1970.
fn epoch_millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// How much of the gap between two commits counts as active typing.
///
/// Zero for the first commit ever (`previous` is 0), for a gap longer than the limit, and for any
/// non-positive gap — which covers both a clock set backwards and two commits landing in the same
/// millisecond.
fn active_gap_ms(previous_ms: u64, now_ms: u64) -> u64 {
    if previous_ms == 0 || now_ms <= previous_ms {
        return 0;
    }
    let gap = now_ms - previous_ms;
    if gap > ACTIVE_GAP_LIMIT_MS {
        return 0;
    }
    gap
}

fn checked_increment(
    values: &mut BTreeMap<String, u64>,
    key: &str,
    amount: u64,
) -> Result<(), TypingStatisticsError> {
    let value = values.entry(key.to_owned()).or_default();
    *value = value
        .checked_add(amount)
        .filter(|count| *count <= MAX_COUNT)
        .ok_or(TypingStatisticsError::CountExhausted)?;
    Ok(())
}

fn validate_counts(value: &TypingBreakdown, total: u64) -> Result<(), TypingStatisticsError> {
    for values in [&value.characters, &value.sources] {
        if values.len() > 64
            || values.keys().any(|key| {
                key.is_empty()
                    || key.len() > 32
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            || values.values().any(|count| *count > total)
            || values
                .values()
                .try_fold(0_u64, |sum, count| sum.checked_add(*count))
                .is_none_or(|sum| sum > total)
        {
            return Err(TypingStatisticsError::InvalidDocument);
        }
    }
    Ok(())
}

/// The day key `days` days before `day`, or `None` when `day` is not a date.
fn day_before(day: &str, days: u32) -> Option<String> {
    crate::calendar::shift_day(day, -i64::from(days))
}

fn validate_day(day: &str) -> Result<(), TypingStatisticsError> {
    crate::calendar::is_valid_day(day)
        .then_some(())
        .ok_or(TypingStatisticsError::InvalidDay)
}

fn classify(grapheme: &str) -> &'static str {
    let mut scalars = grapheme.chars();
    let Some(first) = scalars.next() else {
        return "symbol";
    };
    let code = first as u32;
    if is_han(code) {
        return "han";
    }
    if is_emoji(grapheme) {
        return "emoji";
    }
    match get_general_category(first) {
        GeneralCategory::DecimalNumber => "number",
        GeneralCategory::UppercaseLetter
        | GeneralCategory::LowercaseLetter
        | GeneralCategory::TitlecaseLetter
        | GeneralCategory::ModifierLetter
        | GeneralCategory::OtherLetter => {
            if is_latin(code) {
                "latin"
            } else {
                "otherLetter"
            }
        }
        GeneralCategory::ConnectorPunctuation
        | GeneralCategory::DashPunctuation
        | GeneralCategory::OpenPunctuation
        | GeneralCategory::ClosePunctuation
        | GeneralCategory::InitialPunctuation
        | GeneralCategory::FinalPunctuation
        | GeneralCategory::OtherPunctuation => "punctuation",
        _ => "symbol",
    }
}

fn is_han(code: u32) -> bool {
    (0x3400..=0x4dbf).contains(&code)
        || (0x4e00..=0x9fff).contains(&code)
        || (0xf900..=0xfaff).contains(&code)
        || (0x20000..=0x323af).contains(&code)
}

fn is_latin(code: u32) -> bool {
    (0x41..=0x5a).contains(&code)
        || (0x61..=0x7a).contains(&code)
        || (0xc0..=0x24f).contains(&code)
        || (0x1e00..=0x1eff).contains(&code)
        || (0xab30..=0xab6f).contains(&code)
        || (0xff21..=0xff3a).contains(&code)
        || (0xff41..=0xff5a).contains(&code)
}

fn is_emoji(grapheme: &str) -> bool {
    grapheme.chars().any(|scalar| {
        let code = scalar as u32;
        (0x1f000..=0x1faff).contains(&code)
            || (0x2600..=0x27bf).contains(&code)
            || code == 0x20e3
            || code == 0xfe0f
    })
}

#[cfg(test)]
mod selection_tests;

#[cfg(test)]
mod tests;
