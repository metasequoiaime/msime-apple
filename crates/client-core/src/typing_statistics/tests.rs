//! Unit tests for the parent module, in their own file because the module
//! is large enough that mixing them with the implementation obscured both.
//! Same `mod tests` as before, so `use super::*` still names the parent.

use super::*;
use std::sync::Arc;

#[test]
fn oversized_document_is_rejected_before_loading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typing-statistics.json");
    std::fs::File::create(path)
        .unwrap()
        .set_len(MAX_DOCUMENT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        TypingStatisticsStore::new(directory.path()).load(),
        Err(TypingStatisticsError::InvalidDocument)
    ));
}

#[test]
fn records_graphemes_categories_and_sources_without_text() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    assert_eq!(
        store
            .record(
                "汉𠮷Aée\u{301}９1，!👨‍👩‍👧‍👦1️⃣あЖ+ \n",
                TypingSource::NineKey,
                "2026-09-07",
                Some(9),
            )
            .unwrap(),
        14
    );
    let value = store.load().unwrap();
    assert_eq!(value.total, 14);
    assert_eq!(value.detail.characters["han"], 2);
    assert_eq!(value.detail.characters["latin"], 3);
    assert_eq!(value.detail.characters["number"], 2);
    assert_eq!(value.detail.characters["punctuation"], 2);
    assert_eq!(value.detail.characters["emoji"], 2);
    assert_eq!(value.detail.characters["otherLetter"], 2);
    assert_eq!(value.detail.characters["symbol"], 1);
    assert_eq!(value.detail.sources["nineKey"], 14);
    let persisted = fs::read_to_string(directory.path().join("typing-statistics.json")).unwrap();
    assert!(!persisted.contains('汉'));
    assert!(persisted.contains("nineKey\":14"));
}

#[test]
fn migrates_legacy_totals_and_preserves_pause_on_reset() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("typing-statistics.json"),
        r#"{"enabled":true,"total":12,"days":{"2026-09-07":12}}"#,
    )
    .unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    let legacy = store.load().unwrap();
    assert_eq!(legacy.breakdown(None).characters["unknown"], 12);
    store.set_enabled(false).unwrap();
    assert_eq!(
        store
            .record("ignored", TypingSource::English, "2026-09-07", Some(9))
            .unwrap(),
        0
    );
    assert_eq!(store.load().unwrap().total, 12);
    let reset = store.reset().unwrap();
    assert!(!reset.enabled);
    assert_eq!(reset.total, 0);
    assert!(reset.days.is_empty());
}

#[test]
fn moves_a_valid_legacy_store_without_replacing_shared_statistics() {
    let root = tempfile::tempdir().unwrap();
    let legacy = TypingStatisticsStore::new(root.path());
    legacy.set_enabled(true).unwrap();
    legacy
        .record("old", TypingSource::English, "2026-09-07", Some(9))
        .unwrap();
    let shared_directory = root.path().join("MSIME");
    let shared = TypingStatisticsStore::new(&shared_directory);

    assert!(shared.migrate_from(root.path()).unwrap());
    assert!(!root.path().join("typing-statistics.json").exists());
    assert_eq!(shared.load().unwrap().total, 3);

    // Its document was moved away, so as far as the store is concerned this is a fresh
    // profile again - and a fresh profile has statistics off.
    legacy.set_enabled(true).unwrap();
    legacy
        .record("legacy", TypingSource::English, "2026-09-08", Some(9))
        .unwrap();
    assert!(!shared.migrate_from(root.path()).unwrap());
    assert_eq!(shared.load().unwrap().total, 3);
    assert_eq!(legacy.load().unwrap().total, 6);
}

#[test]
fn serializes_writers_and_keeps_every_day_under_forever() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(TypingStatisticsStore::new(directory.path()));
    store.set_enabled(true).unwrap();
    let writers = (0..50)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store
                    .record("字", TypingSource::Quanpin, "2026-01-01", Some(9))
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }
    for offset in 1..=370 {
        let year = 2026 + (offset / 336);
        let day_of_year = offset % 336;
        let month = day_of_year / 28 + 1;
        let day = day_of_year % 28 + 1;
        store
            .record(
                "字",
                TypingSource::Quanpin,
                &format!("{year:04}-{month:02}-{day:02}"),
                Some(9),
            )
            .unwrap();
    }
    let value = store.load().unwrap();
    // Forever is the default and deletes nothing: the 2026-01-01 the writers shared plus the 370 later days.
    assert_eq!(value.retention, StatisticsRetention::Forever);
    assert_eq!(value.days.len(), 371);
    assert_eq!(value.daily_details.len(), 371);
    assert_eq!(value.total, 420);
    assert_eq!(value.detail.characters["han"], 420);
}

#[test]
fn rejects_invalid_dates_and_documents_without_overwriting() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    assert!(matches!(
        store.record("x", TypingSource::English, "2026-13-01", Some(9)),
        Err(TypingStatisticsError::InvalidDay)
    ));
    let path = directory.path().join("typing-statistics.json");
    fs::write(&path, r#"{"enabled":true,"total":1,"days":{},"detail":{"characters":{"latin":2},"sources":{}},"dailyDetails":{}}"#).unwrap();
    assert!(matches!(
        store.load(),
        Err(TypingStatisticsError::InvalidDocument)
    ));
    assert!(fs::read_to_string(path).unwrap().contains("\"latin\":2"));
}

#[test]
fn active_time_counts_only_the_gaps_that_are_still_typing() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    let day = "2026-09-21";
    // The first commit has nothing to measure against, so it contributes no active time -
    // otherwise the epoch itself would be counted as one enormous pause.
    store
        .record_at("a", TypingSource::Quanpin, day, Some(9), 1_000)
        .unwrap();
    assert_eq!(store.load().unwrap().active_ms(day), None);

    store
        .record_at("b", TypingSource::Quanpin, day, Some(9), 4_000)
        .unwrap();
    assert_eq!(store.load().unwrap().active_ms(day), Some(3_000));

    // Exactly at the limit still counts; one millisecond past it is a break.
    store
        .record_at(
            "c",
            TypingSource::Quanpin,
            day,
            Some(9),
            4_000 + ACTIVE_GAP_LIMIT_MS,
        )
        .unwrap();
    assert_eq!(
        store.load().unwrap().active_ms(day),
        Some(3_000 + ACTIVE_GAP_LIMIT_MS)
    );
    let after_break = 4_000 + ACTIVE_GAP_LIMIT_MS + ACTIVE_GAP_LIMIT_MS + 1;
    store
        .record_at("d", TypingSource::Quanpin, day, Some(9), after_break)
        .unwrap();
    assert_eq!(
        store.load().unwrap().active_ms(day),
        Some(3_000 + ACTIVE_GAP_LIMIT_MS)
    );

    // A clock set backwards adds nothing and does not move the mark backwards; the next
    // commit at a sane instant must not be measured against the rolled-back one.
    store
        .record_at("e", TypingSource::Quanpin, day, Some(9), 500)
        .unwrap();
    let rolled_back = store.load().unwrap();
    assert_eq!(
        rolled_back.active_ms(day),
        Some(3_000 + ACTIVE_GAP_LIMIT_MS)
    );
    assert_eq!(rolled_back.last_commit_ms, after_break);

    // Two commits in the same millisecond are not a gap.
    store
        .record_at("f", TypingSource::Quanpin, day, Some(9), after_break)
        .unwrap();
    assert_eq!(
        store.load().unwrap().active_ms(day),
        Some(3_000 + ACTIVE_GAP_LIMIT_MS)
    );
}

#[test]
fn hourly_buckets_come_from_the_host_and_are_optional() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    let day = "2026-09-21";
    store
        .record_at("ab", TypingSource::Quanpin, day, Some(0), 1_000)
        .unwrap();
    store
        .record_at("c", TypingSource::Quanpin, day, Some(23), 2_000)
        .unwrap();
    // No hour: the characters still count, the day simply has no breakdown for them. The
    // buckets are therefore a subset of the day's total, never equal to it in general.
    store
        .record_at("de", TypingSource::Quanpin, day, None, 3_000)
        .unwrap();
    // Out of range is dropped rather than folded into a neighbouring hour, which would put
    // typing on the chart at a time it did not happen.
    store
        .record_at("f", TypingSource::Quanpin, day, Some(24), 4_000)
        .unwrap();

    let value = store.load().unwrap();
    let hours = value.hours(day).unwrap();
    assert_eq!(hours.len(), HOURS);
    assert_eq!(hours[0], 2);
    assert_eq!(hours[23], 1);
    assert_eq!(hours.iter().sum::<u64>(), 3);
    assert_eq!(value.days[day], 6);
    assert_eq!(value.hours("2026-09-20"), None);
}

#[test]
fn retention_and_reset_take_the_activity_axes_with_them() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    // 28-day months and 12-month years, so the synthetic calendar stays valid past a year of recorded days without pulling in a date library.
    let recorded = 367;
    let mut last_day = String::new();
    for offset in 0..recorded {
        last_day = synthetic_day(offset);
        store
            .record_at(
                "字",
                TypingSource::Quanpin,
                &last_day,
                Some(9),
                1_000 + offset as u64 * 500,
            )
            .unwrap();
    }
    let value = store.load().unwrap();
    // Forever keeps every day on every axis.
    assert_eq!(value.days.len(), recorded);
    assert_eq!(value.daily_details.len(), recorded);
    assert_eq!(value.daily_hours.len(), recorded);
    // The first commit has no gap to measure, so it is the one day without active time.
    assert_eq!(value.daily_active_ms.len(), recorded - 1);

    let boundary = day_before(&last_day, 365).unwrap();
    let narrowed = store
        .set_retention(StatisticsRetention::Days365, &last_day)
        .unwrap();
    assert!(narrowed.days.len() < recorded);
    assert!(!narrowed.days.is_empty());
    // Pruning a day has to drop every axis keyed by it, or validate() rejects the document it just wrote and the user loses the whole history to a stale entry.
    assert!(narrowed.days.keys().all(|day| *day >= boundary));
    assert!(narrowed.daily_details.keys().all(|day| *day >= boundary));
    assert!(narrowed.daily_active_ms.keys().all(|day| *day >= boundary));
    assert!(narrowed.daily_hours.keys().all(|day| *day >= boundary));
    assert!(narrowed
        .daily_details
        .keys()
        .all(|day| narrowed.days.contains_key(day)));
    assert!(narrowed
        .daily_active_ms
        .keys()
        .all(|day| narrowed.days.contains_key(day)));
    assert!(narrowed
        .daily_hours
        .keys()
        .all(|day| narrowed.days.contains_key(day)));
    // One character a day, so the running total is the number of retained days.
    assert_eq!(narrowed.total, narrowed.days.len() as u64);
    assert_eq!(narrowed.detail.characters["han"], narrowed.total);
    assert!(store.load().is_ok());

    let reset = store.reset().unwrap();
    assert!(reset.daily_active_ms.is_empty());
    assert!(reset.daily_hours.is_empty());
    // Reset means reset: when typing last happened is the one field that would otherwise
    // survive and still say something about the user.
    assert_eq!(reset.last_commit_ms, 0);
}

/// A valid `YYYY-MM-DD` for `offset` on a calendar of 28-day months and 12-month years.
fn synthetic_day(offset: usize) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        2026 + offset / 336,
        (offset % 336) / 28 + 1,
        offset % 28 + 1
    )
}

#[test]
fn forever_never_prunes_even_across_many_first_writes_of_a_day() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    // Every day is a new day, so every write runs the first-write-of-a-day retention pass.
    let recorded = 800;
    for offset in 0..recorded {
        store
            .record_at(
                "字",
                TypingSource::Quanpin,
                &synthetic_day(offset),
                Some(9),
                1_000 + offset as u64 * 500,
            )
            .unwrap();
    }
    let value = store.load().unwrap();
    assert_eq!(value.retention, StatisticsRetention::Forever);
    assert_eq!(value.days.len(), recorded);
    assert_eq!(value.daily_details.len(), recorded);
    assert_eq!(value.daily_hours.len(), recorded);
    assert_eq!(value.last_pruned_day, synthetic_day(recorded - 1));
}

#[test]
fn a_document_with_more_than_a_year_of_days_loads() {
    let directory = tempfile::tempdir().unwrap();
    let days = (0..500)
        .map(|offset| format!("\"{}\":1", synthetic_day(offset)))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        directory.path().join("typing-statistics.json"),
        format!(r#"{{"enabled":true,"total":500,"days":{{{days}}}}}"#),
    )
    .unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    let value = store.load().unwrap();
    assert_eq!(value.days.len(), 500);
    assert_eq!(value.total, 500);
}

#[test]
fn the_retention_boundary_is_calendar_arithmetic() {
    // Across a month, a year and a leap day, which is what a subtraction on the day number
    // alone would get wrong.
    assert_eq!(day_before("2026-09-21", 0).as_deref(), Some("2026-09-21"));
    assert_eq!(day_before("2026-09-21", 30).as_deref(), Some("2026-08-22"));
    assert_eq!(day_before("2026-01-05", 30).as_deref(), Some("2025-12-06"));
    // 2028 is a leap year: 2028-03-01 minus one day is the 29th.
    assert_eq!(day_before("2028-03-01", 1).as_deref(), Some("2028-02-29"));
    assert_eq!(day_before("2026-03-01", 1).as_deref(), Some("2026-02-28"));
    assert_eq!(day_before("2027-01-01", 365).as_deref(), Some("2026-01-01"));
    // Not a date at all.
    assert_eq!(day_before("not-a-day", 30), None);
}

#[test]
fn an_unknown_retention_keeps_everything() {
    // A preference this build does not understand must never be read as permission to delete.
    assert_eq!(
        StatisticsRetention::parse("30d"),
        StatisticsRetention::Days30
    );
    assert_eq!(
        StatisticsRetention::parse("365d"),
        StatisticsRetention::Days365
    );
    assert_eq!(
        StatisticsRetention::parse("7d"),
        StatisticsRetention::Forever
    );
    assert_eq!(StatisticsRetention::parse(""), StatisticsRetention::Forever);
    assert_eq!(StatisticsRetention::Forever.days(), None);
    assert_eq!(StatisticsRetention::Days90.days(), Some(90));
    // And the same through the document, where a damaged value must not make the whole file
    // unreadable either.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typing-statistics.json");
    fs::write(
        &path,
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1},"retention":"7d"}"#,
    )
    .unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    assert_eq!(
        store.load().unwrap().retention,
        StatisticsRetention::Forever
    );
}

#[test]
fn retention_drops_days_outside_the_window_on_the_first_write_of_a_day() {
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    store.set_enabled(true).unwrap();
    for day in ["2026-06-01", "2026-08-25", "2026-09-20"] {
        store
            .record_at("字", TypingSource::Quanpin, day, Some(9), 1_000)
            .unwrap();
    }
    assert_eq!(store.load().unwrap().days.len(), 3);

    // Choosing a window applies it at once: the user asked for those days to be gone, and
    // waiting for the next day boundary would leave them on the page they asked from.
    let narrowed = store
        .set_retention(StatisticsRetention::Days30, "2026-09-21")
        .unwrap();
    assert_eq!(
        narrowed.days.keys().collect::<Vec<_>>(),
        ["2026-08-25", "2026-09-20"]
    );
    assert!(!narrowed.daily_details.contains_key("2026-06-01"));
    assert!(!narrowed.daily_hours.contains_key("2026-06-01"));
    // As with the baseline's ClearThrough, the pruned day leaves the running totals as well, so "累计" and its categories cover the retained window.
    assert_eq!(narrowed.total, 2);
    let mut retained = TypingBreakdown::default();
    for detail in narrowed.daily_details.values() {
        retained.merge(detail).unwrap();
    }
    assert_eq!(narrowed.detail, retained);
    assert_eq!(narrowed.detail.characters["han"], 2);
    assert_eq!(narrowed.detail.sources["quanpin"], 2);

    // A later day carries the window with it: 2026-08-25 falls out once "today" moves past
    // thirty days from it.
    store
        .record_at("字", TypingSource::Quanpin, "2026-09-25", Some(9), 2_000)
        .unwrap();
    let moved = store.load().unwrap();
    assert!(!moved.days.contains_key("2026-08-25"));
    assert!(moved.days.contains_key("2026-09-20"));
    assert_eq!(moved.total, 2);
    assert_eq!(moved.detail.characters["han"], 2);

    // The mark that says the window has been applied for this day.
    //
    // That pruning happens on the *first* write of a day rather than on every write is a
    // cost property, not an observable one: the boundary only depends on the day, so running
    // it on every commit would reach the same result by doing more work. This asserts the
    // mark is kept; nothing here can tell the two apart, and an assertion claiming to would
    // be pinning nothing.
    assert_eq!(moved.last_pruned_day, "2026-09-25");

    // Forever removes nothing.
    let kept = store
        .set_retention(StatisticsRetention::Forever, "2027-12-31")
        .unwrap();
    assert_eq!(kept.days.len(), 2);
}

#[test]
fn pruning_a_day_without_a_breakdown_only_lowers_the_total() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("typing-statistics.json"),
        r#"{"enabled":true,"total":5,"days":{"2026-06-01":3,"2026-09-20":2},"detail":{"characters":{"han":2},"sources":{"quanpin":2}},"dailyDetails":{"2026-09-20":{"characters":{"han":2},"sources":{"quanpin":2}}}}"#,
    )
    .unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    let narrowed = store
        .set_retention(StatisticsRetention::Days30, "2026-09-21")
        .unwrap();
    assert_eq!(narrowed.days.keys().collect::<Vec<_>>(), ["2026-09-20"]);
    assert_eq!(narrowed.total, 2);
    assert_eq!(narrowed.detail.characters["han"], 2);
    assert_eq!(narrowed.detail.sources["quanpin"], 2);
    assert!(store.load().is_ok());
}

#[test]
fn pruning_rebuilds_categories_that_would_exceed_the_new_total() {
    // An older build could keep categories for days it had already dropped. Subtracting only the pruned day's own records would then leave a category sum above the lowered total, which validate() rejects.
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("typing-statistics.json"),
        r#"{"enabled":true,"total":10,"days":{"2026-06-01":3,"2026-09-20":2},"detail":{"characters":{"han":9},"sources":{"quanpin":9}},"dailyDetails":{"2026-09-20":{"characters":{"han":2},"sources":{"quanpin":2}}}}"#,
    )
    .unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    let narrowed = store
        .set_retention(StatisticsRetention::Days30, "2026-09-21")
        .unwrap();
    assert_eq!(narrowed.total, 7);
    assert_eq!(narrowed.detail.characters["han"], 2);
    assert_eq!(narrowed.detail.sources["quanpin"], 2);
    assert!(store.load().is_ok());
}

#[test]
fn statistics_are_off_until_they_are_asked_for() {
    // The baseline ships them disabled and says so in its feature list. A fresh profile must
    // not start counting what someone types before they have said yes.
    let directory = tempfile::tempdir().unwrap();
    let store = TypingStatisticsStore::new(directory.path());
    assert!(!store.load().unwrap().enabled);
    assert_eq!(
        store
            .record("字", TypingSource::Quanpin, "2026-09-21", Some(9))
            .unwrap(),
        0
    );
    assert_eq!(store.load().unwrap().total, 0);
    // A document written before this field existed keeps what it says.
    fs::write(
        directory.path().join("typing-statistics.json"),
        r#"{"enabled":true,"total":5,"days":{"2026-09-21":5}}"#,
    )
    .unwrap();
    assert!(store.load().unwrap().enabled);
}

#[test]
fn rejects_activity_axes_that_do_not_match_the_days() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typing-statistics.json");
    let store = TypingStatisticsStore::new(directory.path());
    let cases = [
        // Active time on a day with no characters.
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1},"dailyActiveMs":{"2026-09-20":5}}"#,
        // More active time than a day contains.
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1},"dailyActiveMs":{"2026-09-21":86400001}}"#,
        // Buckets that do not describe a day of 24 hours.
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1},"dailyHours":{"2026-09-21":[1,0,0]}}"#,
        // Buckets claiming more characters than the day has.
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1},"dailyHours":{"2026-09-21":[2,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}"#,
    ];
    for document in cases {
        fs::write(&path, document).unwrap();
        assert!(
            matches!(store.load(), Err(TypingStatisticsError::InvalidDocument)),
            "accepted {document}"
        );
    }
    // A day with characters and no activity axes is not malformed: that is every day
    // recorded before these axes existed.
    fs::write(
        &path,
        r#"{"enabled":true,"total":1,"days":{"2026-09-21":1}}"#,
    )
    .unwrap();
    assert_eq!(store.load().unwrap().total, 1);
}
