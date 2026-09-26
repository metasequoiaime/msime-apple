//! Per-user review progress on disk.
//!
//! One JSON document beside `typing-statistics.json` in the host-supplied directory, with the same
//! shape of guarantees: an exclusive lock around every read-modify-write, an atomic replace, a
//! size ceiling, and a corrupt document reported rather than silently replaced with defaults.
//!
//! This is the only part of 背单词模式 that is written, and it is deliberately not a preference.
//! The shared preference document is capped at 16 KiB by the iOS bridge, and a user who has
//! studied a few thousand cards is well past that.

use super::schedule::{self, CardState, ReviewGrade};
use super::wordbook::{self, Wordbook};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The largest progress document that will be read.
///
/// A studied card serialises to roughly 120 bytes. Four mebibytes is tens of thousands of cards —
/// past any real study history, and small enough that a damaged file cannot exhaust memory on a
/// phone.
const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;
/// How many wordbooks may carry progress at once.
const MAX_WORDBOOKS: usize = 32;
/// How many days of review counts are kept, matching the typing statistics' own window.
const MAX_RETAINED_DAYS: usize = 366;
/// The most reviews one day may record. A day has 86 400 seconds; a card cannot take under one.
const MAX_REVIEWS_PER_DAY: u32 = 86_400;

/// How many cards a day's session offers by default.
///
/// Both numbers come from what a session should feel like rather than from a measurement, so they
/// are named here and overridable by the caller rather than buried in the queue builder.
pub const DEFAULT_NEW_CARDS_PER_DAY: usize = 20;
/// The most cards one session will hand out, new and due together.
pub const DEFAULT_SESSION_LIMIT: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum VocabularyProgressError {
    #[error("vocabulary progress storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid vocabulary progress document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid vocabulary review day")]
    InvalidDay,
    #[error("vocabulary progress document is invalid")]
    InvalidDocument,
    #[error("vocabulary wordbook identifier is invalid")]
    InvalidWordbook,
    #[error("word is not in the wordbook")]
    UnknownWord,
    #[error("vocabulary review count exhausted")]
    CountExhausted,
}

/// What one day's session did.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyReviewCounts {
    /// Cards answered, counting a card failed and answered again as two.
    ///
    /// This is 已完成 on the progress row. It counts answers rather than distinct cards because
    /// that is what the user did, and because a failed card genuinely comes round again.
    pub answered: u32,
    /// Cards seen for the very first time.
    pub introduced: u32,
}

/// How the user wants their sessions to run.
///
/// These live in the progress document rather than in [`crate::preferences`], and the reason is a
/// rule the macOS host enforces: its `preference-coverage` ctest requires every public
/// `Preferences` field to be read by the IME host, and force-listing one as not applicable
/// produces a switch that saves, reports success and does nothing. No IME host reads which
/// wordbook is selected — only the review session does — so this is not a preference. Keeping it
/// here also keeps it out of the shared preference document, which the iOS bridge caps at 16 KiB,
/// and out of the six-host `deny_unknown_fields` coordination a new preference key would need.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyReviewSettings {
    /// The wordbook the session draws from. Empty until the user picks one.
    #[serde(default)]
    pub wordbook: String,
    /// New cards to introduce per day.
    #[serde(default = "default_new_per_day")]
    pub new_per_day: u32,
    /// The most cards one session hands out, new and due together.
    #[serde(default = "default_session_limit")]
    pub session_limit: u32,
}

fn default_new_per_day() -> u32 {
    DEFAULT_NEW_CARDS_PER_DAY as u32
}

fn default_session_limit() -> u32 {
    DEFAULT_SESSION_LIMIT as u32
}

/// Hand-written so it returns exactly what the serde defaults return. A missing file produces
/// `Default`, and a file missing only these keys produces the serde defaults; the two have to
/// agree or the same profile behaves differently depending on which path it took.
impl Default for VocabularyReviewSettings {
    fn default() -> Self {
        Self {
            wordbook: String::new(),
            new_per_day: default_new_per_day(),
            session_limit: default_session_limit(),
        }
    }
}

impl VocabularyReviewSettings {
    /// The daily allowances, clamped to what a session can actually serve.
    ///
    /// Zero new cards a day is a legitimate choice — it means "only review what I already know" —
    /// so it is not corrected upwards. A zero session limit is not, because it would produce a
    /// queue that is always empty and a page that looks broken.
    fn is_valid(&self) -> bool {
        (self.wordbook.is_empty() || wordbook::id_is_well_formed(&self.wordbook))
            && self.new_per_day as usize <= wordbook::MAX_ENTRIES
            && self.session_limit >= 1
            && self.session_limit as usize <= wordbook::MAX_ENTRIES
    }
}

/// The stored document.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyProgress {
    /// Wordbook id → headword → schedule. A word appears only once the user has answered it, so a
    /// fresh profile with five bundled books is still an empty document.
    #[serde(default)]
    pub cards: BTreeMap<String, BTreeMap<String, CardState>>,
    /// Local day → what that day's session did.
    #[serde(default)]
    pub daily: BTreeMap<String, DailyReviewCounts>,
    /// How the user wants their sessions to run.
    #[serde(default)]
    pub settings: VocabularyReviewSettings,
}

impl VocabularyProgress {
    /// The schedule for one word, if it has been answered.
    pub fn card(&self, wordbook: &str, word: &str) -> Option<&CardState> {
        self.cards.get(wordbook)?.get(word)
    }

    /// Answers recorded on `day`.
    pub fn answered_on(&self, day: &str) -> u32 {
        self.daily.get(day).map_or(0, |counts| counts.answered)
    }

    /// Whether this document is one this code could have written.
    fn validate(&self) -> Result<(), VocabularyProgressError> {
        if self.cards.len() > MAX_WORDBOOKS
            || self.daily.len() > MAX_RETAINED_DAYS
            || !self.settings.is_valid()
        {
            return Err(VocabularyProgressError::InvalidDocument);
        }
        for (id, words) in &self.cards {
            if !wordbook::id_is_well_formed(id) || words.len() > wordbook::MAX_ENTRIES {
                return Err(VocabularyProgressError::InvalidDocument);
            }
            for (word, state) in words {
                if word.is_empty()
                    || word.chars().count() > wordbook::MAX_WORD_CHARS
                    || word.chars().any(char::is_control)
                    || !state.is_valid()
                {
                    return Err(VocabularyProgressError::InvalidDocument);
                }
            }
        }
        for (day, counts) in &self.daily {
            if !crate::calendar::is_valid_day(day)
                || counts.answered > MAX_REVIEWS_PER_DAY
                // A day cannot introduce more cards than it answered: the first answer on a card
                // is what introduces it.
                || counts.introduced > counts.answered
            {
                return Err(VocabularyProgressError::InvalidDocument);
            }
        }
        Ok(())
    }

    /// Drop day counts older than `MAX_RETAINED_DAYS` before `today`.
    ///
    /// Card schedules are never pruned. A card the user studied two years ago and has not seen
    /// since is exactly the card the schedule exists to bring back.
    fn prune(&mut self, today: &str) {
        let Some(boundary) = crate::calendar::shift_day(today, -(MAX_RETAINED_DAYS as i64)) else {
            return;
        };
        self.daily.retain(|day, _| day.as_str() > boundary.as_str());
    }
}

/// What a session should show next.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewQueue {
    /// The words to show, due cards before new ones.
    pub words: Vec<String>,
    /// Cards already waiting, before the new-card allowance is added. This is 待复习 on the
    /// progress row.
    pub due: usize,
    /// New cards this session will introduce.
    pub introducing: usize,
    /// Words in the book that are neither due nor being introduced today.
    pub remaining: usize,
}

/// The queue for `wordbook` on `today`.
///
/// Due cards come first and in due order — the longest-overdue card is the one most at risk of
/// being forgotten, so it is not left behind a batch of new words. New words follow in the book's
/// own order, capped at `new_per_day`, and the whole queue is capped at `session_limit` so a user
/// returning after a month is not handed nine hundred cards.
///
/// `None` when `today` is not a day or `wordbook` is not one this code could have stored.
pub fn build_queue(
    progress: &VocabularyProgress,
    book: &Wordbook,
    today: &str,
    new_per_day: usize,
    session_limit: usize,
) -> Option<ReviewQueue> {
    if !crate::calendar::is_valid_day(today) || !wordbook::id_is_well_formed(&book.id) {
        return None;
    }
    let states = progress.cards.get(&book.id);

    let mut due: Vec<(&str, &CardState)> = Vec::new();
    let mut fresh: Vec<&str> = Vec::new();
    let mut remaining = 0;
    for word in book.words() {
        match states.and_then(|words| words.get(word)) {
            Some(state) if state.is_due(today) => due.push((word, state)),
            Some(_) => remaining += 1,
            None => fresh.push(word),
        }
    }

    // Longest overdue first, then by word so the order is total and a rebuild is stable.
    due.sort_by(|(left_word, left), (right_word, right)| {
        left.due
            .cmp(&right.due)
            .then_with(|| left_word.cmp(right_word))
    });

    let introduced_today = progress
        .daily
        .get(today)
        .map_or(0, |counts| counts.introduced) as usize;
    let allowance = new_per_day.saturating_sub(introduced_today);

    let due_count = due.len();
    let mut words: Vec<String> = due
        .into_iter()
        .map(|(word, _)| word.to_owned())
        .take(session_limit)
        .collect();
    let room = session_limit.saturating_sub(words.len());
    let introducing = fresh.len().min(allowance).min(room);
    words.extend(
        fresh
            .iter()
            .take(introducing)
            .map(|word| (*word).to_owned()),
    );

    Some(ReviewQueue {
        words,
        due: due_count,
        introducing,
        remaining: remaining + fresh.len() - introducing,
    })
}

/// The review progress file in a host-supplied directory.
#[derive(Clone, Debug)]
pub struct VocabularyProgressStore {
    directory: PathBuf,
}

impl VocabularyProgressStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Where the progress file lives, for hosts that offer to reveal it in a file manager.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn path(&self) -> PathBuf {
        self.directory.join("vocabulary-progress.json")
    }

    fn lock(&self) -> Result<File, VocabularyProgressError> {
        fs::create_dir_all(&self.directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("vocabulary-progress.lock"))?;
        crate::file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn read_locked(&self) -> Result<VocabularyProgress, VocabularyProgressError> {
        let path = self.path();
        let bytes = match File::open(&path) {
            Ok(file) => read_bounded_document(file)?,
            // A missing file is a fresh profile. A damaged one is not, and is never overwritten
            // below — the two cases are deliberately different.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VocabularyProgress::default());
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(VocabularyProgressError::InvalidDocument);
        }
        let value: VocabularyProgress = serde_json::from_slice(&bytes)?;
        value.validate()?;
        Ok(value)
    }

    fn write_locked(&self, value: &VocabularyProgress) -> Result<(), VocabularyProgressError> {
        value.validate()?;
        let bytes = serde_json::to_vec(value)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path())
            .map(|_| ())
            .map_err(|error| VocabularyProgressError::Io(error.error))
    }

    pub fn load(&self) -> Result<VocabularyProgress, VocabularyProgressError> {
        let _lock = self.lock()?;
        self.read_locked()
    }

    /// Record that the user answered `word` from `book` with `grade` on `today`.
    ///
    /// Returns the card's new schedule. The word must be in the book: a card keyed on a word the
    /// book does not have could never come up again, so storing one would silently lose the
    /// answer.
    pub fn answer(
        &self,
        book: &Wordbook,
        word: &str,
        grade: ReviewGrade,
        today: &str,
    ) -> Result<CardState, VocabularyProgressError> {
        if !crate::calendar::is_valid_day(today) {
            return Err(VocabularyProgressError::InvalidDay);
        }
        if !wordbook::id_is_well_formed(&book.id) {
            return Err(VocabularyProgressError::InvalidWordbook);
        }
        if book.entry(word).is_none() {
            return Err(VocabularyProgressError::UnknownWord);
        }

        let _lock = self.lock()?;
        let mut document = self.read_locked()?;
        document.prune(today);

        let words = document.cards.entry(book.id.clone()).or_default();
        if words.len() >= wordbook::MAX_ENTRIES && !words.contains_key(word) {
            return Err(VocabularyProgressError::InvalidDocument);
        }
        let existing = words
            .get(word)
            .cloned()
            .unwrap_or_else(|| CardState::new(today));
        let first_answer = existing.is_new();
        let next = schedule::schedule(&existing, grade, today)
            .ok_or(VocabularyProgressError::InvalidDay)?;
        words.insert(word.to_owned(), next.clone());

        if document.cards.len() > MAX_WORDBOOKS {
            return Err(VocabularyProgressError::InvalidDocument);
        }

        let counts = document.daily.entry(today.to_owned()).or_default();
        counts.answered = counts
            .answered
            .checked_add(1)
            .ok_or(VocabularyProgressError::CountExhausted)?;
        if first_answer {
            counts.introduced = counts
                .introduced
                .checked_add(1)
                .ok_or(VocabularyProgressError::CountExhausted)?;
        }
        if counts.answered > MAX_REVIEWS_PER_DAY {
            return Err(VocabularyProgressError::CountExhausted);
        }

        self.write_locked(&document)?;
        Ok(next)
    }

    /// Replace the session settings, leaving cards and day counts alone.
    ///
    /// Returns the whole document, not nothing, so the page has the new state without a second
    /// read — the same contract the typing-statistics mutators follow, and what lets a caller keep
    /// one in-flight request rather than hand-rolling a refetch after every change.
    pub fn set_settings(
        &self,
        settings: VocabularyReviewSettings,
    ) -> Result<VocabularyProgress, VocabularyProgressError> {
        if !settings.is_valid() {
            return Err(VocabularyProgressError::InvalidDocument);
        }
        let _lock = self.lock()?;
        let mut document = self.read_locked()?;
        document.settings = settings;
        self.write_locked(&document)?;
        Ok(document)
    }

    /// Forget every card in one wordbook, leaving the day counts alone.
    ///
    /// The counts record what the user did, which resetting a book does not undo.
    pub fn reset_wordbook(
        &self,
        wordbook: &str,
    ) -> Result<VocabularyProgress, VocabularyProgressError> {
        if !wordbook::id_is_well_formed(wordbook) {
            return Err(VocabularyProgressError::InvalidWordbook);
        }
        let _lock = self.lock()?;
        let mut document = self.read_locked()?;
        document.cards.remove(wordbook);
        self.write_locked(&document)?;
        Ok(document)
    }

    /// Forget everything.
    pub fn reset(&self) -> Result<VocabularyProgress, VocabularyProgressError> {
        let _lock = self.lock()?;
        let document = VocabularyProgress::default();
        self.write_locked(&document)?;
        Ok(document)
    }
}

fn read_bounded_document(file: File) -> Result<Vec<u8>, VocabularyProgressError> {
    if file.metadata()?.len() > MAX_DOCUMENT_BYTES {
        return Err(VocabularyProgressError::InvalidDocument);
    }
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(VocabularyProgressError::InvalidDocument);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocabulary::wordbook::WordbookEntry;

    const TODAY: &str = "2026-09-23";

    fn book_of(words: &[&str]) -> Wordbook {
        Wordbook {
            id: "cet-4".to_owned(),
            name: "CET-4".to_owned(),
            entries: words
                .iter()
                .map(|word| WordbookEntry {
                    word: (*word).to_owned(),
                    phonetic: String::new(),
                    meaning: "adj. 合成释义".to_owned(),
                })
                .collect(),
        }
    }

    fn store() -> (tempfile::TempDir, VocabularyProgressStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = VocabularyProgressStore::new(directory.path());
        (directory, store)
    }

    #[test]
    fn a_missing_file_is_a_fresh_profile() {
        let (_directory, store) = store();
        assert_eq!(store.load().unwrap(), VocabularyProgress::default());
    }

    #[test]
    fn an_answer_round_trips_and_counts_the_day() {
        let (_directory, store) = store();
        let book = book_of(&["ubiquitous", "ephemeral"]);

        let card = store
            .answer(&book, "ubiquitous", ReviewGrade::Known, TODAY)
            .unwrap();
        assert_eq!(card.interval_days, 1);
        assert_eq!(card.due, "2026-09-24");

        let document = store.load().unwrap();
        assert_eq!(document.card("cet-4", "ubiquitous"), Some(&card));
        assert_eq!(document.answered_on(TODAY), 1);
        assert_eq!(document.daily[TODAY].introduced, 1);

        // A second answer on the same card is a second answer, not a second introduction.
        store
            .answer(&book, "ubiquitous", ReviewGrade::Unknown, TODAY)
            .unwrap();
        let document = store.load().unwrap();
        assert_eq!(document.answered_on(TODAY), 2);
        assert_eq!(document.daily[TODAY].introduced, 1);
    }

    #[test]
    fn a_word_outside_the_book_is_refused_rather_than_stored() {
        let (_directory, store) = store();
        let book = book_of(&["ubiquitous"]);
        assert!(matches!(
            store.answer(&book, "absent", ReviewGrade::Known, TODAY),
            Err(VocabularyProgressError::UnknownWord)
        ));
        assert_eq!(store.load().unwrap(), VocabularyProgress::default());
    }

    #[test]
    fn an_unparseable_day_is_refused_before_anything_is_written() {
        let (_directory, store) = store();
        let book = book_of(&["ubiquitous"]);
        assert!(matches!(
            store.answer(&book, "ubiquitous", ReviewGrade::Known, "2026-13-01"),
            Err(VocabularyProgressError::InvalidDay)
        ));
        assert_eq!(store.load().unwrap(), VocabularyProgress::default());
    }

    #[test]
    fn a_damaged_document_is_reported_and_left_on_disk() {
        let (directory, store) = store();
        let path = directory.path().join("vocabulary-progress.json");
        fs::write(&path, br#"{"cards":{"cet-4":{"x":{"dueDate":1}}}}"#).unwrap();

        assert!(store.load().is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            br#"{"cards":{"cet-4":{"x":{"dueDate":1}}}}"#,
            "a damaged document is never replaced with defaults"
        );
    }

    #[test]
    fn an_oversized_document_is_rejected_before_loading() {
        let (directory, store) = store();
        File::create(directory.path().join("vocabulary-progress.json"))
            .unwrap()
            .set_len(MAX_DOCUMENT_BYTES + 1)
            .unwrap();
        assert!(matches!(
            store.load(),
            Err(VocabularyProgressError::InvalidDocument)
        ));
    }

    #[test]
    fn a_document_with_an_out_of_range_card_is_rejected() {
        let (directory, store) = store();
        let path = directory.path().join("vocabulary-progress.json");
        let document = VocabularyProgress {
            cards: BTreeMap::from([(
                "cet-4".to_owned(),
                BTreeMap::from([(
                    "ubiquitous".to_owned(),
                    CardState {
                        ease_permille: 100,
                        ..CardState::new(TODAY)
                    },
                )]),
            )]),
            daily: BTreeMap::new(),
            settings: VocabularyReviewSettings::default(),
        };
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(matches!(
            store.load(),
            Err(VocabularyProgressError::InvalidDocument)
        ));
    }

    #[test]
    fn day_counts_are_pruned_but_card_schedules_are_kept() {
        let (_directory, store) = store();
        let book = book_of(&["ubiquitous"]);
        store
            .answer(&book, "ubiquitous", ReviewGrade::Known, "2024-01-01")
            .unwrap();
        assert_eq!(store.load().unwrap().answered_on("2024-01-01"), 1);

        store
            .answer(&book, "ubiquitous", ReviewGrade::Known, TODAY)
            .unwrap();
        let document = store.load().unwrap();
        assert_eq!(
            document.answered_on("2024-01-01"),
            0,
            "a day past the retention window is dropped"
        );
        assert!(
            document.card("cet-4", "ubiquitous").is_some(),
            "a card studied long ago is exactly the card the schedule exists to bring back"
        );
    }

    #[test]
    fn resetting_one_book_leaves_the_other_and_the_day_counts_alone() {
        let (_directory, store) = store();
        let cet = book_of(&["ubiquitous"]);
        let mut kaoyan = book_of(&["ephemeral"]);
        kaoyan.id = "kaoyan".to_owned();

        store
            .answer(&cet, "ubiquitous", ReviewGrade::Known, TODAY)
            .unwrap();
        store
            .answer(&kaoyan, "ephemeral", ReviewGrade::Known, TODAY)
            .unwrap();

        store.reset_wordbook("cet-4").unwrap();
        let document = store.load().unwrap();
        assert!(document.card("cet-4", "ubiquitous").is_none());
        assert!(document.card("kaoyan", "ephemeral").is_some());
        assert_eq!(
            document.answered_on(TODAY),
            2,
            "the counts record what the user did, which a reset does not undo"
        );

        store.reset().unwrap();
        assert_eq!(store.load().unwrap(), VocabularyProgress::default());
    }

    #[test]
    fn a_fresh_book_offers_only_the_new_card_allowance() {
        let words: Vec<String> = (0..50).map(|index| format!("word{index:02}")).collect();
        let book = book_of(&words.iter().map(String::as_str).collect::<Vec<_>>());
        let progress = VocabularyProgress::default();

        let queue = build_queue(&progress, &book, TODAY, 20, DEFAULT_SESSION_LIMIT).unwrap();
        assert_eq!(queue.due, 0);
        assert_eq!(queue.introducing, 20);
        assert_eq!(queue.words.len(), 20);
        assert_eq!(queue.remaining, 30);
        assert_eq!(
            queue.words[0], "word00",
            "new words follow the book's own order"
        );
    }

    #[test]
    fn due_cards_come_first_and_longest_overdue_leads() {
        let book = book_of(&["alpha", "beta", "gamma"]);
        let progress = VocabularyProgress {
            cards: BTreeMap::from([(
                "cet-4".to_owned(),
                BTreeMap::from([
                    (
                        "beta".to_owned(),
                        CardState {
                            due: "2026-09-01".to_owned(),
                            reviews: 1,
                            ..CardState::new(TODAY)
                        },
                    ),
                    (
                        "gamma".to_owned(),
                        CardState {
                            due: "2026-08-01".to_owned(),
                            reviews: 1,
                            ..CardState::new(TODAY)
                        },
                    ),
                ]),
            )]),
            daily: BTreeMap::new(),
            settings: VocabularyReviewSettings::default(),
        };

        let queue = build_queue(&progress, &book, TODAY, 20, DEFAULT_SESSION_LIMIT).unwrap();
        assert_eq!(queue.due, 2);
        assert_eq!(queue.introducing, 1);
        assert_eq!(
            queue.words,
            vec!["gamma", "beta", "alpha"],
            "the longest-overdue card is the one most at risk, so it is not queued behind new words"
        );
    }

    #[test]
    fn cards_not_yet_due_stay_out_of_the_queue() {
        let book = book_of(&["alpha", "beta"]);
        let progress = VocabularyProgress {
            cards: BTreeMap::from([(
                "cet-4".to_owned(),
                BTreeMap::from([(
                    "alpha".to_owned(),
                    CardState {
                        due: "2026-12-31".to_owned(),
                        reviews: 3,
                        ..CardState::new(TODAY)
                    },
                )]),
            )]),
            daily: BTreeMap::new(),
            settings: VocabularyReviewSettings::default(),
        };
        let queue = build_queue(&progress, &book, TODAY, 20, DEFAULT_SESSION_LIMIT).unwrap();
        assert_eq!(queue.due, 0);
        assert_eq!(queue.words, vec!["beta"]);
        assert_eq!(queue.remaining, 1);
    }

    #[test]
    fn the_new_card_allowance_is_spent_by_what_the_day_already_introduced() {
        let words: Vec<String> = (0..50).map(|index| format!("word{index:02}")).collect();
        let book = book_of(&words.iter().map(String::as_str).collect::<Vec<_>>());
        let progress = VocabularyProgress {
            cards: BTreeMap::new(),
            daily: BTreeMap::from([(
                TODAY.to_owned(),
                DailyReviewCounts {
                    answered: 18,
                    introduced: 18,
                },
            )]),
            settings: VocabularyReviewSettings::default(),
        };
        let queue = build_queue(&progress, &book, TODAY, 20, DEFAULT_SESSION_LIMIT).unwrap();
        assert_eq!(queue.introducing, 2);
    }

    #[test]
    fn the_session_limit_caps_a_backlog_instead_of_handing_over_everything() {
        let words: Vec<String> = (0..500).map(|index| format!("word{index:03}")).collect();
        let book = book_of(&words.iter().map(String::as_str).collect::<Vec<_>>());
        let cards = words
            .iter()
            .map(|word| {
                (
                    word.clone(),
                    CardState {
                        due: "2026-01-01".to_owned(),
                        reviews: 1,
                        ..CardState::new(TODAY)
                    },
                )
            })
            .collect();
        let progress = VocabularyProgress {
            cards: BTreeMap::from([("cet-4".to_owned(), cards)]),
            daily: BTreeMap::new(),
            settings: VocabularyReviewSettings::default(),
        };

        let queue = build_queue(&progress, &book, TODAY, 20, DEFAULT_SESSION_LIMIT).unwrap();
        assert_eq!(queue.due, 500, "the backlog is reported in full");
        assert_eq!(
            queue.words.len(),
            DEFAULT_SESSION_LIMIT,
            "but a month away must not hand over nine hundred cards"
        );
        assert_eq!(
            queue.introducing, 0,
            "a backlog leaves no room for new words"
        );
    }

    #[test]
    fn a_fresh_profile_gets_the_same_settings_whether_the_keys_were_missing_or_the_file_was() {
        let (directory, store) = store();
        assert_eq!(
            store.load().unwrap().settings,
            VocabularyReviewSettings::default(),
            "a missing file is the Default impl"
        );

        // A document written before these keys existed must read as the same thing, or the same
        // profile behaves differently depending on which path it took.
        fs::write(directory.path().join("vocabulary-progress.json"), b"{}").unwrap();
        assert_eq!(
            store.load().unwrap().settings,
            VocabularyReviewSettings::default(),
            "the serde defaults have to agree with the Default impl"
        );
    }

    #[test]
    fn settings_round_trip_and_leave_the_cards_and_counts_alone() {
        let (_directory, store) = store();
        let book = book_of(&["ubiquitous"]);
        store
            .answer(&book, "ubiquitous", ReviewGrade::Known, TODAY)
            .unwrap();

        let document = store
            .set_settings(VocabularyReviewSettings {
                wordbook: "kaoyan".to_owned(),
                new_per_day: 40,
                session_limit: 100,
            })
            .unwrap();
        assert_eq!(document.settings.wordbook, "kaoyan");
        assert_eq!(document.settings.new_per_day, 40);

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded.settings, document.settings);
        assert!(reloaded.card("cet-4", "ubiquitous").is_some());
        assert_eq!(reloaded.answered_on(TODAY), 1);
    }

    #[test]
    fn settings_outside_the_stored_range_are_refused() {
        let (_directory, store) = store();
        let valid = VocabularyReviewSettings::default();

        // Zero new cards a day means "only review what I already know", which is a real choice.
        assert!(store
            .set_settings(VocabularyReviewSettings {
                new_per_day: 0,
                ..valid.clone()
            })
            .is_ok());

        // A zero session limit would leave the queue permanently empty and the page looking broken.
        assert!(store
            .set_settings(VocabularyReviewSettings {
                session_limit: 0,
                ..valid.clone()
            })
            .is_err());
        assert!(store
            .set_settings(VocabularyReviewSettings {
                wordbook: "CET 4".to_owned(),
                ..valid
            })
            .is_err());
    }

    #[test]
    fn a_queue_is_refused_for_a_day_or_a_book_it_cannot_key() {
        let book = book_of(&["alpha"]);
        let progress = VocabularyProgress::default();
        assert!(build_queue(&progress, &book, "not-a-day", 20, 200).is_none());

        let mut bad = book;
        bad.id = "CET 4".to_owned();
        assert!(build_queue(&progress, &bad, TODAY, 20, 200).is_none());
    }
}
