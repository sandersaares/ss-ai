use std::path::Path;

use ohno::{AppError, EnrichableExt};
use serde::Serialize;

use crate::Process;
use crate::error::fail;
use crate::git_diff::{FileDiff, parse};

/// Exact endpoint comparison shared by review anchoring and change assessment.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Comparison {
    pub(crate) old_commit: String,
    pub(crate) new_commit: String,
    pub(crate) files: Vec<FileDiff>,
}

/// Reads committed content without external diff or text conversion programs.
pub(crate) fn compare(
    process: &dyn Process,
    repository: &Path,
    old: &str,
    new: &str,
) -> Result<Comparison, AppError> {
    let repository = repository
        .to_str()
        .ok_or_else(|| fail("invalidInput", "Git repository path must be UTF-8"))?;
    for commit in [old, new] {
        validate_commit(commit)?;
    }
    git(process, repository, &["rev-parse", "--git-dir"]).map_err(|error| {
        error.enrich(format!(
            "There is no readable Git repository at '{repository}'"
        ))
    })?;
    for commit in [old, new] {
        let kind = git(process, repository, &["cat-file", "-t", commit]).map_err(|error| {
            error.enrich(format!(
                "Commit {commit} must be fetched into the repository before comparison"
            ))
        })?;
        if kind != b"commit\n" {
            return Err(fail(
                "invalidInput",
                format!("Object {commit} is not a Git commit"),
            ));
        }
    }
    // Context matches hosting-service comment anchors. Explicit endpoints, not
    // three-dot syntax, retain removals after source history is rewritten.
    let output = git(
        process,
        repository,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-relative",
            "--no-textconv",
            "--submodule=short",
            "--ignore-submodules=none",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            old,
            new,
            "--",
        ],
    )?;
    let text = String::from_utf8(output)?;
    Ok(Comparison {
        old_commit: old.to_owned(),
        new_commit: new.to_owned(),
        files: parse(&text)?,
    })
}

/// Rejects revisions and command options where immutable object IDs are required.
fn validate_commit(commit: &str) -> Result<(), AppError> {
    // Git supports SHA-1 and SHA-256 object formats; abbreviated IDs are ambiguous.
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fail("invalidInput", "Supply a full Git commit object ID"));
    }
    Ok(())
}

/// Keeps repository selection explicit and preserves failing Git diagnostics.
fn git(process: &dyn Process, repository: &str, arguments: &[&str]) -> Result<Vec<u8>, AppError> {
    let arguments = ["--no-pager", "--no-replace-objects", "-C", repository]
        .into_iter()
        .chain(arguments.iter().copied())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let output = process.execute("git", &arguments, &[])?;
    if !output.success {
        return Err(fail(
            "io",
            format!(
                "Git comparison failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;

    use crate::ProcessOutput;

    use super::*;

    /// Records the comparison boundary and injects a single failing operation.
    struct GitProcess {
        calls: RefCell<Vec<Vec<String>>>,
        failure: Option<usize>,
    }

    impl Process for GitProcess {
        fn execute(
            &self,
            program: &str,
            arguments: &[String],
            input: &[u8],
        ) -> io::Result<ProcessOutput> {
            assert_eq!(program, "git");
            assert!(input.is_empty());
            let mut calls = self.calls.borrow_mut();
            calls.push(arguments.to_vec());
            let success = self.failure != Some(calls.len());
            let output = if arguments.iter().any(|argument| argument == "cat-file") {
                b"commit\n".to_vec()
            } else {
                Vec::new()
            };
            Ok(ProcessOutput::new(
                success,
                output,
                b"fixture refusal".to_vec(),
            ))
        }
    }

    #[test]
    fn comparison_pins_content_and_exact_endpoints() {
        let process = GitProcess {
            calls: RefCell::default(),
            failure: None,
        };
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        compare(&process, Path::new("fixture"), &old, &new).unwrap();
        let calls = process.calls.borrow();
        let diff = calls.last().unwrap();
        for argument in [
            "--no-replace-objects",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            "--ignore-submodules=none",
            "--submodule=short",
        ] {
            assert!(diff.iter().any(|value| value == argument));
        }
        assert_eq!(
            diff.iter()
                .rev()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["--", &new, &old]
        );
        assert_eq!(calls.len(), 4);
    }

    #[test]
    fn failed_object_reads_and_diff_reads_stop_at_the_boundary() {
        for failure in 1..=4 {
            let process = GitProcess {
                calls: RefCell::default(),
                failure: Some(failure),
            };
            let error = compare(
                &process,
                Path::new("fixture"),
                &"a".repeat(40),
                &"b".repeat(40),
            )
            .unwrap_err();
            assert!(error.to_string().contains("fixture refusal"));
            assert_eq!(process.calls.borrow().len(), failure);
        }
    }
}
