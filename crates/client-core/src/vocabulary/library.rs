//! The wordbooks available to review, on disk.
//!
//! One file per book under `<directory>/vocabulary-wordbooks/`, the way
//! [`crate::translation::store`] keeps one file per learned gloss: a book runs to a few megabytes,
//! and importing one must not rewrite the others.
//!
//! Beside them is a small `index.json` naming each book and its size. Listing the library is what
//! the settings page does on open, and reading five multi-megabyte documents to count their rows
//! would make that the slowest thing on the page. The index is written inside the same lock as the
//! book it describes, and [`WordbookLibrary::list`] drops any entry whose file has gone — the
//! index is a cache of the directory, never the authority on it.

use super::wordbook::{self, Wordbook, WordbookEntry};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The most books one library may hold.
const MAX_BOOKS: usize = 32;
/// The largest single book that will be read. Twenty thousand entries with a long gloss each.
const MAX_BOOK_BYTES: u64 = 8 * 1024 * 1024;
/// The largest index that will be read. Thirty-two short records.
const MAX_INDEX_BYTES: u64 = 64 * 1024;
/// The directory, relative to the host-supplied application data directory.
const DIRECTORY: &str = "vocabulary-wordbooks";

#[derive(Debug, thiserror::Error)]
pub enum WordbookLibraryError {
    #[error("vocabulary wordbook storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid vocabulary wordbook document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("vocabulary wordbook is invalid")]
    InvalidWordbook,
    #[error("vocabulary wordbook is not in the library")]
    UnknownWordbook,
    #[error("the vocabulary library is full")]
    LibraryFull,
}

/// What the picker needs to draw one row, without reading the book itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordbookSummary {
    pub id: String,
    pub name: String,
    pub total: usize,
    /// A bundled book cannot be deleted. Every book is currently imported, so this is currently
    /// always false; it is the field the picker's delete affordance keys off, and bundled books
    /// will set it when they are shipped.
    pub builtin: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryIndex {
    #[serde(default)]
    books: Vec<WordbookSummary>,
}

/// The imported wordbooks in a host-supplied directory.
#[derive(Clone, Debug)]
pub struct WordbookLibrary {
    directory: PathBuf,
}

impl WordbookLibrary {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into().join(DIRECTORY),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn book_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    fn index_path(&self) -> PathBuf {
        self.directory.join("index.json")
    }

    fn lock(&self) -> Result<File, WordbookLibraryError> {
        fs::create_dir_all(&self.directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("wordbooks.lock"))?;
        crate::file_lock::exclusive(&lock)?;
        Ok(lock)
    }

    fn read_index_locked(&self) -> Result<LibraryIndex, WordbookLibraryError> {
        let bytes = match File::open(self.index_path()) {
            Ok(file) => read_bounded_document(file, MAX_INDEX_BYTES)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LibraryIndex::default());
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        let index: LibraryIndex = serde_json::from_slice(&bytes)?;
        if index.books.len() > MAX_BOOKS
            || !index
                .books
                .iter()
                .all(|book| wordbook::id_is_well_formed(&book.id))
        {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        Ok(index)
    }

    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> Result<(), WordbookLibraryError> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map(|_| ())
            .map_err(|error| WordbookLibraryError::Io(error.error))
    }

    /// Every book the library holds, in the order they were imported.
    ///
    /// An index entry whose file has gone is dropped rather than reported: the book is genuinely
    /// not there, and a picker row that fails to open is worse than a row that is absent.
    pub fn list(&self) -> Result<Vec<WordbookSummary>, WordbookLibraryError> {
        let _lock = self.lock()?;
        let index = self.read_index_locked()?;
        Ok(index
            .books
            .into_iter()
            .filter(|book| self.book_path(&book.id).is_file())
            .collect())
    }

    /// One book in full. `Ok(None)` when the library does not have it.
    pub fn load(&self, id: &str) -> Result<Option<Wordbook>, WordbookLibraryError> {
        if !wordbook::id_is_well_formed(id) {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        let _lock = self.lock()?;
        let bytes = match File::open(self.book_path(id)) {
            Ok(file) => read_bounded_document(file, MAX_BOOK_BYTES)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_BOOK_BYTES {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        let book: Wordbook = serde_json::from_slice(&bytes)?;
        // A book that does not validate is reported, never silently skipped: the user imported it
        // and would otherwise see it vanish from the picker with no explanation.
        if !book.is_valid() || book.id != id {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        Ok(Some(book))
    }

    /// Store `entries` as the book `id`, called `name`. Importing an existing id replaces it.
    ///
    /// `id` is the caller's and must never be taken from the imported file. A book keys the review
    /// progress, so a file that named its own id could silently inherit — or destroy — the
    /// schedule of a book the user imported earlier. It is a parameter rather than generated here
    /// so that the schedule of a replaced book is deliberately preserved, and so these tests do
    /// not have to assert against a fresh random value.
    pub fn import(
        &self,
        name: &str,
        entries: Vec<WordbookEntry>,
        id: &str,
    ) -> Result<Wordbook, WordbookLibraryError> {
        let book = Wordbook {
            id: id.to_owned(),
            name: name.to_owned(),
            entries,
        };
        if !book.is_valid() {
            return Err(WordbookLibraryError::InvalidWordbook);
        }

        let _lock = self.lock()?;
        let mut index = self.read_index_locked()?;
        if !index.books.iter().any(|entry| entry.id == book.id) && index.books.len() >= MAX_BOOKS {
            return Err(WordbookLibraryError::LibraryFull);
        }

        let bytes = serde_json::to_vec(&book)?;
        if bytes.len() as u64 > MAX_BOOK_BYTES {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        // The book first, then the index. A crash between the two leaves a file no index names,
        // which is invisible and harmless; the other order would leave a picker row that opens
        // nothing.
        self.write_atomically(&self.book_path(&book.id), &bytes)?;

        let summary = WordbookSummary {
            id: book.id.clone(),
            name: book.name.clone(),
            total: book.entries.len(),
            builtin: false,
        };
        match index.books.iter_mut().find(|entry| entry.id == summary.id) {
            Some(existing) => *existing = summary,
            None => index.books.push(summary),
        }
        self.write_atomically(&self.index_path(), &serde_json::to_vec(&index)?)?;
        Ok(book)
    }

    /// Forget one book. The caller is responsible for its review progress.
    pub fn remove(&self, id: &str) -> Result<(), WordbookLibraryError> {
        if !wordbook::id_is_well_formed(id) {
            return Err(WordbookLibraryError::InvalidWordbook);
        }
        let _lock = self.lock()?;
        let mut index = self.read_index_locked()?;
        let before = index.books.len();
        index.books.retain(|entry| entry.id != id);
        if index.books.len() == before && !self.book_path(id).is_file() {
            return Err(WordbookLibraryError::UnknownWordbook);
        }
        // The index first this time, so a crash between the two leaves an orphan file rather than
        // a row pointing at a deleted book.
        self.write_atomically(&self.index_path(), &serde_json::to_vec(&index)?)?;
        match fs::remove_file(self.book_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn read_bounded_document(file: File, maximum: u64) -> Result<Vec<u8>, WordbookLibraryError> {
    if file.metadata()?.len() > maximum {
        return Err(WordbookLibraryError::InvalidWordbook);
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(WordbookLibraryError::InvalidWordbook);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(words: &[&str]) -> Vec<WordbookEntry> {
        words
            .iter()
            .map(|word| WordbookEntry {
                word: (*word).to_owned(),
                phonetic: String::new(),
                meaning: "adj. 合成释义".to_owned(),
            })
            .collect()
    }

    fn library() -> (tempfile::TempDir, WordbookLibrary) {
        let directory = tempfile::tempdir().unwrap();
        let library = WordbookLibrary::new(directory.path());
        (directory, library)
    }

    #[test]
    fn an_empty_library_lists_nothing() {
        let (_directory, library) = library();
        assert!(library.list().unwrap().is_empty());
        assert_eq!(library.load("user-1").unwrap(), None);
    }

    #[test]
    fn an_imported_book_is_listed_and_can_be_read_back() {
        let (_directory, library) = library();
        let book = library
            .import("我的词表", entries(&["alpha", "beta"]), "user-1")
            .unwrap();
        assert_eq!(book.entries.len(), 2);

        assert_eq!(
            library.list().unwrap(),
            vec![WordbookSummary {
                id: "user-1".to_owned(),
                name: "我的词表".to_owned(),
                total: 2,
                builtin: false,
            }]
        );
        assert_eq!(library.load("user-1").unwrap().as_ref(), Some(&book));
    }

    #[test]
    fn importing_the_same_id_replaces_the_book_without_a_second_row() {
        let (_directory, library) = library();
        library
            .import("第一版", entries(&["alpha"]), "user-1")
            .unwrap();
        library
            .import("第二版", entries(&["alpha", "beta", "gamma"]), "user-1")
            .unwrap();

        let listed = library.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "第二版");
        assert_eq!(listed[0].total, 3);
    }

    #[test]
    fn several_books_keep_their_import_order() {
        let (_directory, library) = library();
        library.import("甲", entries(&["a"]), "user-1").unwrap();
        library.import("乙", entries(&["b"]), "user-2").unwrap();
        library.import("丙", entries(&["c"]), "user-3").unwrap();
        assert_eq!(
            library
                .list()
                .unwrap()
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["user-1", "user-2", "user-3"]
        );
    }

    #[test]
    fn removing_a_book_drops_the_row_and_the_file() {
        let (_directory, library) = library();
        library.import("甲", entries(&["a"]), "user-1").unwrap();
        library.import("乙", entries(&["b"]), "user-2").unwrap();

        library.remove("user-1").unwrap();
        assert_eq!(
            library
                .list()
                .unwrap()
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["user-2"]
        );
        assert_eq!(library.load("user-1").unwrap(), None);
        assert!(!library.directory().join("user-1.json").exists());

        assert!(matches!(
            library.remove("user-1"),
            Err(WordbookLibraryError::UnknownWordbook)
        ));
    }

    #[test]
    fn an_index_row_whose_file_is_gone_is_dropped_from_the_listing() {
        let (_directory, library) = library();
        library.import("甲", entries(&["a"]), "user-1").unwrap();
        library.import("乙", entries(&["b"]), "user-2").unwrap();
        fs::remove_file(library.directory().join("user-1.json")).unwrap();

        // A picker row that fails to open is worse than a row that is not there.
        assert_eq!(
            library
                .list()
                .unwrap()
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["user-2"]
        );
    }

    #[test]
    fn a_damaged_book_is_reported_rather_than_silently_skipped() {
        let (_directory, library) = library();
        library.import("甲", entries(&["a"]), "user-1").unwrap();
        fs::write(
            library.directory().join("user-1.json"),
            b"{\"id\":\"user-1\"}",
        )
        .unwrap();

        // The user imported it; it must not vanish from the picker with no explanation.
        assert!(library.load("user-1").is_err());
    }

    #[test]
    fn an_oversized_book_is_rejected_before_loading() {
        let (_directory, library) = library();
        fs::create_dir_all(library.directory()).unwrap();
        File::create(library.directory().join("user-1.json"))
            .unwrap()
            .set_len(MAX_BOOK_BYTES + 1)
            .unwrap();
        assert!(matches!(
            library.load("user-1"),
            Err(WordbookLibraryError::InvalidWordbook)
        ));
    }

    #[test]
    fn a_book_whose_stored_id_disagrees_with_its_file_is_refused() {
        let (_directory, library) = library();
        let book = Wordbook {
            id: "user-2".to_owned(),
            name: "错位".to_owned(),
            entries: entries(&["a"]),
        };
        fs::create_dir_all(library.directory()).unwrap();
        fs::write(
            library.directory().join("user-1.json"),
            serde_json::to_vec(&book).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            library.load("user-1"),
            Err(WordbookLibraryError::InvalidWordbook)
        ));
    }

    #[test]
    fn an_invalid_book_is_refused_before_anything_is_written() {
        let (_directory, library) = library();
        assert!(matches!(
            library.import("空的", Vec::new(), "user-1"),
            Err(WordbookLibraryError::InvalidWordbook)
        ));
        assert!(matches!(
            library.import("坏 id", entries(&["a"]), "User 1"),
            Err(WordbookLibraryError::InvalidWordbook)
        ));
        assert!(library.list().unwrap().is_empty());
    }

    #[test]
    fn the_library_refuses_a_thirty_third_book() {
        let (_directory, library) = library();
        for index in 0..MAX_BOOKS {
            library
                .import("书", entries(&["a"]), &format!("user-{index}"))
                .unwrap();
        }
        assert!(matches!(
            library.import("再来一本", entries(&["a"]), "user-extra"),
            Err(WordbookLibraryError::LibraryFull)
        ));
        // An existing book may still be replaced when the library is full.
        assert!(library.import("书", entries(&["a", "b"]), "user-0").is_ok());
    }
}
