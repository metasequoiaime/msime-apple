//! Dictionary maintenance with the input hosts' sessions released.
//!
//! Importing, editing or clearing learned data needs the Engine's exclusive dictionary lock, and every open input session holds it shared. The Linux hosts (IBus, Fcitx5) and the macOS input method are other processes, so on those platforms this writes a lease beside the lock. The hosts check it on their preference timers (Fcitx5 every 250 ms, IBus and macOS every second), finish the composition, close their sessions and open no new ones while it is live; the settings window also tells the macOS input method at once over a distributed notification (the `announce` of [`QuiescedHosts`]), so it lets go without waiting for its timer. The lease holds its own expiry, so a writer that dies mid-import cannot leave input off for longer than that. The file name, the expiry on the first line and the 30 second bound are shared with `platforms/common/DictionaryQuiesceLease.h`. Moving the data directory on Linux holds the same lease for the whole copy (the desktop app's `platform::linux::linux_dictionary_quiesce`).
//!
//! More than one process writes the lease: the settings window and the `msime-mcp` server. Each writes a second line naming itself and removes the lease only while that line is still its own, so one writer finishing does not take down a lease another one is still working under. The hosts read only the first line.
//!
//! On Windows every input session lives in the one Server process, which releases them when asked over its auxiliary pipe instead (see [`server`]). The Server does not track who asked, so one writer's resume can hand the sessions back while another is still working; that writer's next request then finds the dictionaries busy, asks again and is retried like the first.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const LEASE_NAME: &str = ".msime-dictionary-quiesce";
const LEASE_DURATION: Duration = Duration::from_secs(30);
/// Long enough for the IBus host's one-second timer to come round twice.
pub const RETRY_BUDGET: Duration = Duration::from_millis(2500);
pub const RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// The host API's reason when an input session holds the dictionaries.
pub const BUSY: &str = "dictionary maintenance busy";
/// The native lease reader consumes at most 31 bytes. Keep a generous bound
/// for the owner line while preventing a corrupt lease from causing an
/// unbounded allocation when a writer is dropped.
const MAX_LEASE_BYTES: u64 = 4096;

fn read_lease(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_LEASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_LEASE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(windows)]
use server::ServerRelease as Release;
/// What the hosts are released under: the lease, or on Windows the Server's own release. Both are taken with `acquire`, renewed with `publish` and let go when dropped.
#[cfg(not(windows))]
use Lease as Release;

/// The lease as one writer holds it: where it is and what this writer last put there.
pub struct Lease {
    path: PathBuf,
    /// The process and a number no other lease in it has, so two leases in one process are told apart too.
    owner: String,
    /// The number in `owner`, which also names this lease's staged file, so two leases in one process never stage over each other.
    serial: u64,
    written: String,
}

impl Lease {
    pub fn acquire(user_data: &Path) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut lease = Self {
            path: user_data.join(LEASE_NAME),
            owner: format!("{} {serial}", std::process::id()),
            serial,
            written: String::new(),
        };
        lease.publish()?;
        Ok(lease)
    }

    /// Write the lease with an expiry `LEASE_DURATION` from now, replacing any earlier one in a single rename so a host never reads a partial file.
    pub fn publish(&mut self) -> std::io::Result<()> {
        let expiry = SystemTime::now()
            .checked_add(LEASE_DURATION)
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .ok_or_else(|| std::io::Error::other("clock before the epoch"))?
            .as_millis();
        let staged = self.path.with_file_name(format!(
            "{LEASE_NAME}.{}-{}",
            std::process::id(),
            self.serial
        ));
        // The owner line tells this lease from one another writer put up; the expiry alone could coincide.
        let contents = format!("{expiry}\n{}\n", self.owner);
        if let Err(error) =
            std::fs::write(&staged, &contents).and_then(|()| std::fs::rename(&staged, &self.path))
        {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        self.written = contents;
        Ok(())
    }
}

impl Drop for Lease {
    /// Remove the lease only while it is still the one this writer last wrote. When another writer has replaced it since, that writer's work is still running under it, and removing it would let the hosts reopen their sessions in the middle of it. The read and the removal are not one step, so a replacement landing between them is still removed; the other writer puts it back on its next request.
    fn drop(&mut self) {
        if read_lease(&self.path).is_some_and(|current| current == self.written) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The lease itself, or one still being staged under `<lease>.<pid>-<n>` (by these writers and by the macOS input method). Copying either along with the user directory would keep input off in the copy until it expired. Only the Linux data-directory move copies the user directory.
pub fn is_lease_file(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.strip_prefix(LEASE_NAME)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

/// The hosts asked to let go of `user_data` for one settings-page action, which may be several requests: an import larger than one host request is sent in batches. The lease goes up the first time a request finds the dictionaries busy and stays up for every request after it, so the hosts close their sessions once rather than once per batch, and it is renewed before each later request so a long import does not outlive its expiry. `announce` runs once, right after the lease first goes up, so a host that can be told directly (the macOS input method) lets go at once instead of on its timer. Dropping this removes the lease, which is the resume.
pub struct QuiescedHosts<'a, Announce: FnMut()> {
    user_data: Option<&'a Path>,
    announce: Announce,
    lease: Option<Release>,
}

impl<'a, Announce: FnMut()> QuiescedHosts<'a, Announce> {
    pub fn new(user_data: Option<&'a str>, announce: Announce) -> Self {
        Self {
            user_data: user_data.map(Path::new).filter(|path| path.is_absolute()),
            announce,
            lease: None,
        }
    }

    /// Run `attempt`; when it fails only because an input session holds the dictionaries, ask the hosts to let go and retry until it gets through or the budget runs out. Any other failure is about the request itself and is returned as it is. A completed write is never replayed, because only the lock failure is retried.
    pub fn run<T>(&mut self, attempt: impl FnMut() -> Result<T, String>) -> Result<T, String> {
        self.run_within(RETRY_BUDGET, attempt)
    }

    fn run_within<T>(
        &mut self,
        budget: Duration,
        mut attempt: impl FnMut() -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(lease) = &mut self.lease {
            // A lease that cannot be renewed still holds until its expiry, and a host that reopens a session after that makes the attempt below busy, which is retried like any other.
            let _ = lease.publish();
        }
        let mut result = attempt();
        if !matches!(&result, Err(reason) if reason == BUSY) {
            return result;
        }
        let Some(user_data) = self.user_data else {
            return result;
        };
        if self.lease.is_none() {
            let Ok(lease) = Release::acquire(user_data) else {
                return result;
            };
            self.lease = Some(lease);
            (self.announce)();
        }
        let deadline = Instant::now() + budget;
        while matches!(&result, Err(reason) if reason == BUSY) && Instant::now() < deadline {
            std::thread::sleep(RETRY_INTERVAL);
            // A session that came back after the Server released them (a window focused meanwhile, or another writer resuming) is only let go when the Server is asked again. The Linux and macOS hosts keep watching the lease, so there is nothing to repeat there.
            #[cfg(windows)]
            if let Some(release) = &mut self.lease {
                let _ = release.publish();
            }
            result = attempt();
        }
        result
    }
}

/// The Windows Server's release, asked for over its auxiliary pipe with the UTF-16LE message `DictionaryQuiesce` and given back with `DictionaryResume` (`platforms/windows/common/AuxMessage.h`). It answers "OK" only once its sessions really are gone, so that reply, not a write getting through, is what makes the exclusive lock safe to take. It gives the sessions back by itself 30 seconds after the last `DictionaryQuiesce`, so a writer that dies mid-import cannot leave input off for longer, and a long one renews the release before each request.
#[cfg(any(windows, test))]
pub mod server {
    pub const PIPE_NAME: &str = r"\\.\pipe\FanyImeAuxNamedPipe";

    pub fn message(verb: &str) -> Vec<u8> {
        verb.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// The Server writes UTF-16LE "OK" and nothing else.
    pub fn answered(reply: &[u8]) -> bool {
        reply == b"O\x00K\x00"
    }

    /// Send `verb` and wait for the Server's answer. No pipe means no Server and so no sessions to release. Any other failure to reach it, a refused connection included, is reported rather than taken for release: going ahead would only meet the lock.
    #[cfg(windows)]
    pub fn ask(verb: &str) -> std::io::Result<()> {
        use std::io::{Read, Write};
        /// Every instance of the pipe is serving another client.
        const ERROR_PIPE_BUSY: i32 = 231;
        let mut attempt = 0;
        let mut pipe = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_NAME)
            {
                Ok(pipe) => break pipe,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempt < 4 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        pipe.write_all(&message(verb))?;
        let mut reply = [0_u8; 8];
        let read = pipe.read(&mut reply)?;
        if answered(&reply[..read]) {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "the input method server did not release its sessions",
            ))
        }
    }

    #[cfg(windows)]
    pub struct ServerRelease(());

    #[cfg(windows)]
    impl ServerRelease {
        /// The user directory is where the Linux and macOS lease goes; the Server needs no path.
        pub fn acquire(_user_data: &std::path::Path) -> std::io::Result<Self> {
            ask("DictionaryQuiesce")?;
            Ok(Self(()))
        }

        pub fn publish(&mut self) -> std::io::Result<()> {
            ask("DictionaryQuiesce")
        }
    }

    #[cfg(windows)]
    impl Drop for ServerRelease {
        /// Resume whatever happened: input without sessions because an import failed would be worse than the failure itself.
        fn drop(&mut self) {
            let _ = ask("DictionaryResume");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// One action of one request, with the hosts released again as soon as it is done.
    fn quiesce_with<T>(
        user_data: Option<&str>,
        budget: Duration,
        announce: impl FnMut(),
        attempt: impl FnMut() -> Result<T, String>,
    ) -> Result<T, String> {
        QuiescedHosts::new(user_data, announce).run_within(budget, attempt)
    }

    fn lease_expiry(directory: &Path) -> Option<u128> {
        std::fs::read_to_string(directory.join(LEASE_NAME))
            .ok()
            .and_then(|text| text.lines().next()?.parse().ok())
    }

    #[test]
    fn a_lease_another_writer_replaced_is_left_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let first = Lease::acquire(directory.path()).unwrap();
        // Another writer takes the lease over while the first is still held.
        let second = Lease::acquire(directory.path()).unwrap();
        drop(first);
        assert!(lease_expiry(directory.path()).is_some());
        drop(second);
        assert!(!directory.path().join(LEASE_NAME).exists());
    }

    #[test]
    fn an_oversized_lease_is_ignored_without_reading_it_unboundedly() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(LEASE_NAME);
        std::fs::write(&path, vec![b'x'; MAX_LEASE_BYTES as usize + 1]).unwrap();
        assert_eq!(read_lease(&path), None);
    }

    #[test]
    fn leases_in_one_process_publish_side_by_side() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut lease = Lease::acquire(&path).unwrap();
                    for _ in 0..50 {
                        lease.publish().unwrap();
                    }
                    lease
                })
            })
            .collect();
        let leases: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        // Whichever wrote last owns the lease, and no staged file is left behind.
        let current = std::fs::read_to_string(path.join(LEASE_NAME)).unwrap();
        assert!(leases.iter().any(|lease| lease.written == current));
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        drop(leases);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    }

    #[test]
    fn a_lease_a_host_raised_is_left_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let lease = Lease::acquire(directory.path()).unwrap();
        // The macOS input method raises the lease for its own dictionary window with its own owner line (`raise_dictionary_quiesce_lease`).
        let raised = format!("{}\n4242 0\n", lease_expiry(directory.path()).unwrap());
        std::fs::write(directory.path().join(LEASE_NAME), &raised).unwrap();
        drop(lease);
        assert_eq!(
            std::fs::read_to_string(directory.path().join(LEASE_NAME)).unwrap(),
            raised
        );
    }

    #[test]
    fn busy_is_retried_under_a_lease_that_is_removed_afterwards() {
        let directory = tempfile::tempdir().unwrap();
        let user_data = directory.path().to_str().unwrap().to_owned();
        let calls = Cell::new(0);
        let result = quiesce_with(
            Some(&user_data),
            Duration::from_secs(2),
            || {},
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    // The hosts see the lease while the request is still being retried, with an expiry inside the bound they accept.
                    if calls.get() == 2 {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis();
                        let expiry = lease_expiry(directory.path()).unwrap();
                        assert!(expiry > now && expiry - now <= 30_000);
                    }
                    Err(BUSY.to_owned())
                } else {
                    Ok("imported")
                }
            },
        );
        assert_eq!(result, Ok("imported"));
        assert_eq!(calls.get(), 3);
        assert!(!directory.path().join(LEASE_NAME).exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn one_lease_covers_every_request_of_an_action_and_is_renewed_between_them() {
        let directory = tempfile::tempdir().unwrap();
        let user_data = directory.path().to_str().unwrap().to_owned();
        let announced = Cell::new(0);
        let mut hosts = QuiescedHosts::new(Some(&user_data), || announced.set(announced.get() + 1));
        let calls = Cell::new(0);
        let first = hosts.run_within(Duration::from_secs(2), || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(BUSY.to_owned())
            } else {
                Ok(1)
            }
        });
        assert_eq!(first, Ok(1));
        let expiry = lease_expiry(directory.path()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // The next batch of the same import finds the hosts still released, under a lease pushed forward rather than one about to lapse.
        let second = hosts.run_within(Duration::from_secs(2), || {
            assert!(lease_expiry(directory.path()).unwrap() > expiry);
            Ok(2)
        });
        assert_eq!(second, Ok(2));
        // A later batch that finds the dictionaries busy again is retried under the same lease, and the hosts are not told a second time.
        let calls = Cell::new(0);
        let third = hosts.run_within(Duration::from_secs(2), || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(BUSY.to_owned())
            } else {
                Ok(3)
            }
        });
        assert_eq!(third, Ok(3));
        drop(hosts);
        assert_eq!(announced.get(), 1);
        assert!(!directory.path().join(LEASE_NAME).exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn other_failures_and_success_take_no_lease() {
        let directory = tempfile::tempdir().unwrap();
        let user_data = directory.path().to_str().unwrap().to_owned();
        let calls = Cell::new(0);
        let rejected: Result<(), String> = quiesce_with(
            Some(&user_data),
            Duration::from_secs(2),
            || {},
            || {
                calls.set(calls.get() + 1);
                assert!(!directory.path().join(LEASE_NAME).exists());
                Err("dictionary import rejected".to_owned())
            },
        );
        assert_eq!(rejected, Err("dictionary import rejected".to_owned()));
        assert_eq!(calls.get(), 1);
        let done = quiesce_with(Some(&user_data), Duration::from_secs(2), || {}, || Ok(1));
        assert_eq!(done, Ok(1));
    }

    #[test]
    fn busy_stays_busy_once_the_budget_runs_out_and_the_lease_goes() {
        let directory = tempfile::tempdir().unwrap();
        let user_data = directory.path().to_str().unwrap().to_owned();
        let result: Result<(), String> = quiesce_with(
            Some(&user_data),
            Duration::from_millis(120),
            || {},
            || Err(BUSY.to_owned()),
        );
        assert_eq!(result, Err(BUSY.to_owned()));
        assert!(!directory.path().join(LEASE_NAME).exists());
    }

    #[test]
    fn hosts_are_told_once_and_only_while_the_lease_is_live() {
        let directory = tempfile::tempdir().unwrap();
        let user_data = directory.path().to_str().unwrap().to_owned();
        let calls = Cell::new(0);
        let announced = Cell::new(0);
        let result = quiesce_with(
            Some(&user_data),
            Duration::from_secs(2),
            || {
                assert!(lease_expiry(directory.path()).is_some());
                announced.set(announced.get() + 1);
            },
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 4 {
                    Err(BUSY.to_owned())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(announced.get(), 1);

        // Nothing to release: the request went through, failed for its own reasons, or had nowhere to put a lease.
        let silent = Cell::new(0);
        let _ = quiesce_with(
            Some(&user_data),
            Duration::from_secs(2),
            || silent.set(silent.get() + 1),
            || Ok(()),
        );
        let _: Result<(), String> = quiesce_with(
            Some(&user_data),
            Duration::from_secs(2),
            || silent.set(silent.get() + 1),
            || Err("dictionary import rejected".to_owned()),
        );
        let _: Result<(), String> = quiesce_with(
            Some("relative/user"),
            Duration::from_millis(100),
            || silent.set(silent.get() + 1),
            || Err(BUSY.to_owned()),
        );
        assert_eq!(silent.get(), 0);
    }

    #[test]
    fn the_windows_server_is_spoken_to_in_utf16() {
        assert_eq!(server::message("DictionaryResume")[..4], *b"D\x00i\x00");
        assert_eq!(
            server::message("DictionaryQuiesce").len(),
            2 * "DictionaryQuiesce".len()
        );
        assert!(server::answered(b"O\x00K\x00"));
        for reply in [&b""[..], b"O\x00", b"OK", b"O\x00K\x00\n\x00"] {
            assert!(!server::answered(reply));
        }
    }

    #[test]
    fn lease_files_include_a_lease_being_staged() {
        assert!(is_lease_file(OsStr::new(LEASE_NAME)));
        assert!(is_lease_file(OsStr::new(".msime-dictionary-quiesce.4242")));
        assert!(is_lease_file(OsStr::new(
            ".msime-dictionary-quiesce.4242-0"
        )));
        assert!(!is_lease_file(OsStr::new(".msime-dictionary-quiesced")));
        assert!(!is_lease_file(OsStr::new(".msime-dictionary-access.lock")));
        assert!(!is_lease_file(OsStr::new("msime_user.db")));
    }

    #[test]
    fn without_an_absolute_user_directory_busy_is_returned_once() {
        for user_data in [None, Some("relative/user")] {
            let calls = Cell::new(0);
            let result: Result<(), String> = quiesce_with(
                user_data,
                Duration::from_secs(2),
                || {},
                || {
                    calls.set(calls.get() + 1);
                    Err(BUSY.to_owned())
                },
            );
            assert_eq!(result, Err(BUSY.to_owned()));
            assert_eq!(calls.get(), 1);
        }
    }
}
