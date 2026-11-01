//! Exercises the actual PowerShell and native shared-file implementations together.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use ss_ai_runtime::{Clock, FileTransaction, LockMode, ResourceLock};
use tempfile::{NamedTempFile, TempPath};

/// Prevents other test children from inheriting locks during process creation.
static PROCESS_AND_LOCK: Mutex<()> = Mutex::new(());

/// Keeps each test's file and sibling lock within the checkout.
struct TestFile {
    path: TempPath,
}

impl TestFile {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let file = NamedTempFile::new_in(root).unwrap();
        Self {
            path: file.into_temp_path(),
        }
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let mut lock = self.path.as_os_str().to_owned();
        lock.push(".lock");
        if let Err(error) = fs::remove_file(lock) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
    }
}

/// Owns the fixture process and closes it even when an assertion fails.
///
/// The pipes carry line-based synchronization signals, not general logging.
/// Acknowledgements establish lock acquisition and release ordering without
/// timed waits; see the runtime implementation guide's time and waits section.
struct PowerShell {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    response: String,
}

impl PowerShell {
    fn start(path: &Path, scenario: &str) -> Self {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/shared_file_interop.ps1");
        let mut child = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(fixture)
            .arg("-Path")
            .arg(path)
            .args(["-Scenario", scenario])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("PowerShell 7.6 is required for interoperation tests");
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            response: String::new(),
        }
    }

    fn send(&mut self, signal: &str) {
        writeln!(self.input, "{signal}").unwrap();
        self.input.flush().unwrap();
    }

    fn expect(&mut self, expected: &str) {
        self.response.clear();
        assert_ne!(
            self.output.read_line(&mut self.response).unwrap(),
            0,
            "fixture exited"
        );
        assert_eq!(self.response.trim_end(), expected);
    }

    fn finish(&mut self) {
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for PowerShell {
    fn drop(&mut self) {
        // The fixture may already have exited; cleanup must not mask a test failure.
        if let Err(_error) = self.child.kill() {}
        if let Err(_error) = self.child.wait() {}
    }
}

/// Releases the PowerShell owner only after the native operation observes contention.
struct ReleaseClock<'a> {
    owner: RefCell<&'a mut PowerShell>,
    waits: Cell<u32>,
}

impl Clock for ReleaseClock<'_> {
    fn now(&self) -> Timestamp {
        panic!("file contention does not use wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        assert_eq!(self.waits.get(), 0, "unexpected repeated contention");
        self.waits.set(1);
        let mut owner = self.owner.borrow_mut();
        owner.send("release");
        owner.expect("released");
        Ok(())
    }
}

/// Rejects waits when no contention is expected.
struct NoWaitClock;

impl Clock for NoWaitClock {
    fn now(&self) -> Timestamp {
        panic!("file operations do not use wall-clock time")
    }

    fn wait(&self, _duration: Duration) -> Result<(), AppError> {
        panic!("unexpected contention")
    }
}

/// Verifies both PowerShell ownership modes against the native exclusive transaction.
#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses and real file locks")]
fn powershell_owner_blocks_native_transaction() {
    let _guard = PROCESS_AND_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for scenario in ["exclusive-owner", "shared-owner"] {
        let file = TestFile::new();
        let mut owner = PowerShell::start(&file.path, scenario);
        owner.expect("held");
        let clock = ReleaseClock {
            owner: RefCell::new(&mut owner),
            waits: Cell::new(0),
        };
        let transaction = FileTransaction::open(&file.path, &clock).unwrap();
        assert_eq!(
            clock.waits.get(),
            1,
            "{scenario} must exclude the native owner"
        );
        drop(transaction);
        owner.finish();
    }
}

/// Verifies native exclusive ownership against both PowerShell acquisition modes.
#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses and real file locks")]
fn native_owner_blocks_powershell_lock() {
    let _guard = PROCESS_AND_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for scenario in ["exclusive-contender", "shared-contender"] {
        let file = TestFile::new();
        let mut contender = PowerShell::start(&file.path, scenario);
        contender.expect("ready");
        let transaction = FileTransaction::open(&file.path, &NoWaitClock).unwrap();
        contender.send("acquire");
        contender.expect("contended");
        drop(transaction);
        contender.send("retry");
        contender.expect("acquired");
        contender.finish();
    }
}

/// Confirms shared ownership interoperates and a nonblocking writer sees contention.
#[test]
#[cfg_attr(miri, ignore = "requires PowerShell subprocesses and real file locks")]
fn shared_owners_coexist_and_exclude_writers() {
    let _guard = PROCESS_AND_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let file = TestFile::new();
    let mut owner = PowerShell::start(&file.path, "shared-owner");
    owner.expect("held");
    let reader = ResourceLock::open(&file.path, LockMode::Shared, &NoWaitClock).unwrap();
    assert!(
        ResourceLock::try_open(&file.path, LockMode::Exclusive)
            .unwrap()
            .is_none()
    );
    owner.send("release");
    owner.expect("released");
    owner.finish();

    // Start the child before taking another lock so no lock descriptor is inherited.
    drop(reader);
    let mut contender = PowerShell::start(&file.path, "shared-after-signal");
    contender.expect("ready");
    let reader = ResourceLock::open(&file.path, LockMode::Shared, &NoWaitClock).unwrap();
    contender.send("acquire");
    contender.expect("acquired");
    contender.finish();
    assert!(
        ResourceLock::try_open(&file.path, LockMode::Exclusive)
            .unwrap()
            .is_none()
    );
    drop(reader);
    assert!(
        ResourceLock::try_open(&file.path, LockMode::Exclusive)
            .unwrap()
            .is_some()
    );
}

/// Checks opened snapshots and whole-file publications across both implementations.
#[test]
#[cfg_attr(
    miri,
    ignore = "requires PowerShell subprocesses and real file replacement"
)]
fn complete_publications_are_readable_in_both_directions() {
    let _guard = PROCESS_AND_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let file = TestFile::new();
    fs::write(&file.path, br#"{"version":"original"}"#).unwrap();
    let mut reader = PowerShell::start(&file.path, "publication");
    reader.expect("reader-open");
    let transaction = FileTransaction::open(&file.path, &NoWaitClock).unwrap();
    assert_eq!(
        transaction.read_bytes().unwrap().unwrap(),
        br#"{"version":"original"}"#
    );
    transaction.write_bytes(br#"{"version":"native"}"#).unwrap();
    drop(transaction);
    reader.send("published");
    reader.expect("published");
    reader.finish();
    let transaction = FileTransaction::open(&file.path, &NoWaitClock).unwrap();
    assert_eq!(
        transaction.read_bytes().unwrap().unwrap(),
        br#"{"version":"powershell"}"#
    );
}

/// Observes the actual staging file while Windows blocks atomic replacement.
#[cfg(windows)]
#[test]
#[cfg_attr(miri, ignore = "requires Windows file sharing and replacement")]
fn native_staging_names_identify_a_blocked_transaction() {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct InspectClock {
        directory: PathBuf,
        blocker: RefCell<Option<fs::File>>,
        waits: Cell<u32>,
    }
    impl Clock for InspectClock {
        fn now(&self) -> Timestamp {
            panic!("file operations do not use wall-clock time")
        }

        fn wait(&self, _duration: Duration) -> Result<(), AppError> {
            assert_eq!(self.waits.get(), 0, "unexpected repeated contention");
            self.waits.set(1);
            let names = fs::read_dir(&self.directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| name.starts_with(".ss-ai-transaction-"))
                .collect::<Vec<_>>();
            let [name] = names.as_slice() else {
                panic!("expected exactly one staging file, found {names:?}");
            };
            let random = name
                .strip_prefix(".ss-ai-transaction-")
                .unwrap()
                .strip_suffix(".tmp")
                .unwrap();
            assert_eq!(random.len(), 24);
            assert!(random.bytes().all(|byte| byte.is_ascii_alphanumeric()));
            assert_eq!(fs::read(self.directory.join(name)).unwrap(), b"new");
            drop(self.blocker.borrow_mut().take());
            Ok(())
        }
    }

    let directory = Directory(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".staging-interop-{}", std::process::id())),
    );
    #[expect(clippy::create_dir, reason = "The test directory must be exclusive")]
    fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("reviewer.json");
    fs::write(&path, b"old").unwrap();
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&path)
        .unwrap();
    let clock = InspectClock {
        directory: directory.0.clone(),
        blocker: RefCell::new(Some(blocker)),
        waits: Cell::new(0),
    };
    let transaction = FileTransaction::open(&path, &clock).unwrap();
    transaction.write_bytes(b"new").unwrap();
    assert_eq!(clock.waits.get(), 1);
    assert_eq!(fs::read(&path).unwrap(), b"new");
}
