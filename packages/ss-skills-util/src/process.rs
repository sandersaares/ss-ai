//! Runs authentication CLIs without console encoding or shell payload coupling.

use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// Provides an injectable byte-oriented boundary for authenticated provider CLIs.
///
/// Transport uses this for GitHub API calls and Azure OAuth discovery. Implement
/// it with a native launcher in the binary or recorded replies in tests; provider
/// interpretation belongs to transport, not to the launcher.
pub trait Process {
    /// Captures one completed invocation without interpreting its provider reply.
    ///
    /// Pass arguments separately and payload bytes on stdin. A child exit failure
    /// belongs in `ProcessOutput`; an I/O error represents a launch or pipe failure.
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput>;

    /// Reads Git objects in an invocation-owned repository with isolated configuration.
    fn isolated_git(&self, directory: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        let mut explicit = vec!["-C".to_owned(), directory.to_string_lossy().into_owned()];
        explicit.extend_from_slice(arguments);
        self.execute("git", &explicit, &[])
    }

    /// Fetches Azure objects with a URL-scoped bearer header outside process arguments.
    fn isolated_git_oauth(
        &self,
        _directory: &Path,
        _arguments: &[String],
        _url: &str,
        _token: &str,
    ) -> io::Result<ProcessOutput> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OAuth Git execution is unavailable",
        ))
    }
}

/// Captures provider output without depending on the console's encoding.
///
/// Transport decodes these bytes only after inspecting exit success. Keeping
/// stderr separate preserves refusal evidence without mixing it into JSON.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProcessOutput {
    /// Distinguishes a provider refusal from successful stdout evidence.
    pub success: bool,
    /// Provider data or token output, retained as bytes until transport decodes it.
    pub stdout: Vec<u8>,
    /// Failure evidence used for classification, not a successful result stream.
    pub stderr: Vec<u8>,
}

impl ProcessOutput {
    /// Supplies the captured result from a launcher or an injected test process.
    #[must_use]
    pub fn new(success: bool, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            success,
            stdout,
            stderr,
        }
    }
}

/// Launches native programs with an explicit working directory.
///
/// The executable constructs one instance per invocation and passes it to
/// `run`. Provider CLIs retain responsibility for their own authentication stores.
#[derive(Debug)]
pub struct NativeProcess {
    /// Execution context for provider tools, never a source of target coordinates.
    directory: PathBuf,
}

impl NativeProcess {
    /// Binds provider execution to the caller-selected working directory.
    #[must_use]
    pub fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_owned(),
        }
    }

    /// Drains output concurrently with input so large JSON payloads can complete.
    fn launch(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
        isolated_git: bool,
        oauth: Option<(&str, &str)>,
    ) -> io::Result<ProcessOutput> {
        #[cfg(windows)]
        let program = resolve_program(program)?;
        // Native Unix lookup skips non-executable PATH entries and preserves exec errors.
        let mut command = Command::new(program);
        if isolated_git {
            // Inherited Git paths/configuration must not redirect writes into the user's checkout.
            for (key, _) in env::vars_os() {
                if key
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("GIT_")
                {
                    command.env_remove(key);
                }
            }
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", self.directory.join("empty-config"))
                .env("GIT_CONFIG_SYSTEM", self.directory.join("empty-config"));
        }
        if let Some((url, token)) = oauth {
            // The secret stays off argv and disk; the header matches only the trusted repository.
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", format!("http.{url}.extraHeader"))
                .env(
                    "GIT_CONFIG_VALUE_0",
                    format!("Authorization: Bearer {token}"),
                );
        }
        command
            .args(arguments)
            .current_dir(&self.directory)
            .env("GH_PAGER", "")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        execute_child(command, input)
    }
}

impl Process for NativeProcess {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        self.launch(program, arguments, input, false, None)
    }

    fn isolated_git(&self, directory: &Path, arguments: &[String]) -> io::Result<ProcessOutput> {
        Self::new(directory).launch("git", arguments, &[], true, None)
    }

    fn isolated_git_oauth(
        &self,
        directory: &Path,
        arguments: &[String],
        url: &str,
        token: &str,
    ) -> io::Result<ProcessOutput> {
        Self::new(directory).launch("git", arguments, &[], true, Some((url, token)))
    }
}

/// Drains both output pipes while feeding input, preserving an early provider refusal.
fn execute_child(mut command: Command, input: &[u8]) -> io::Result<ProcessOutput> {
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Child stdin was not piped"))?;
    // wait_with_output drains both output pipes while the writer feeds stdin.
    // Sequential pipe access deadlocks for large provider documents.
    thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output();
        let written = writer
            .join()
            .map_err(|_panic| io::Error::other("Child input writer panicked"))?;
        let output = output?;
        // A failing provider can close stdin before consuming the complete request.
        // Preserve its refusal response instead of replacing it with BrokenPipe.
        if output.status.success() {
            written?;
        }
        Ok(ProcessOutput::new(
            output.status.success(),
            output.stdout,
            output.stderr,
        ))
    })
}

/// Windows PATH discovery includes Azure CLI's command shim.
#[cfg(windows)]
fn resolve_program(program: &str) -> io::Result<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return Ok(path.to_owned());
    }
    let search = env::var_os("PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PATH is not configured"))?;
    for directory in env::split_paths(&search) {
        let extensions = [".exe", ".cmd", ".bat", ""];
        for extension in extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                // Rust's Windows process implementation invokes .cmd/.bat through cmd.exe
                // and escapes each argument; payload JSON remains on stdin.
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{program} was not found on PATH"),
    ))
}
