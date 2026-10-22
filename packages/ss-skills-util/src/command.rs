//! Connects named command inputs to complete provider and journal operations.
//!
//! Parsing stays outside the state machine so invalid invocations cannot begin
//! a reservation transaction. Runtime dependencies arrive from the executable.
//! Explicit help overrides keep internal role documentation out of CLI help;
//! the top-level `about` and existing subcommand summaries supply user-facing text.

use std::ffi::OsString;
use std::io::{self, Read};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};
use ohno::AppError;
use serde_json::Value;

use crate::api::Api;
use crate::assessment::{Disposition, Judgment};
use crate::discovery::{self, DiscoveryInput, WorkAction};
use crate::error::{Failure, fail};
use crate::journal::FileJournal;
use crate::publication;
use crate::selection::{self, State, valid_session};
use crate::target::{Platform, Target, escape};
use crate::transport::Transport;
use crate::{Clock, Http, Process};

/// Named-input command hierarchy used by the installed skills.
///
/// Clap owns syntax and help; operation-specific validation follows parsing
/// before authentication or journal access.
#[derive(Debug, Parser)]
#[command(
    name = "ss-skills-util",
    version,
    about = "Shared native operations for dev-env skills",
    long_about = None
)]
struct Cli {
    /// Selects the functional area without exposing provider request details.
    #[command(subcommand)]
    command: Operation,
}

/// Separates reservation workflow commands from participation-only commands.
#[derive(Debug, Subcommand)]
#[command(about = None, long_about = None)]
enum Operation {
    /// Collect metadata and recency evidence without approving pull requests.
    Autoreapprove {
        #[command(subcommand)]
        command: Autoreapprove,
    },
    /// Read exact Git comparisons or normalize their path and hunk evidence.
    Git {
        #[command(subcommand)]
        command: Git,
    },
    /// Select and confirm a cooperative review reservation.
    Review {
        /// Chooses the reservation phase while retaining the same input contract.
        #[command(subcommand)]
        command: Review,
    },
    /// Register the authenticated account without approving a pull request.
    Reviewer {
        /// Chooses a reviewer operation independently of reservation state.
        #[command(subcommand)]
        command: Reviewer,
    },
}

/// Read-only stages retain incomplete assessment work rather than reporting success.
#[derive(Debug, Subcommand)]
#[command(about = None, long_about = None)]
enum Autoreapprove {
    /// Revalidate and publish a fully assessed renewal, or reconcile its previous effect.
    Publish {
        #[command(flatten)]
        input: WorkInput,
        #[arg(long)]
        repository_url: String,
        #[arg(long)]
        attribution: String,
    },
    /// Fetch exact source objects and initialize a bounded assessment assignment.
    Prepare(WorkInput),
    /// Page through complete changed-path metadata.
    Assessment {
        #[command(flatten)]
        input: WorkInput,
        #[arg(long, default_value = "0")]
        after: usize,
        #[arg(long, default_value = "10")]
        limit: NonZeroUsize,
    },
    /// Read contiguous bounded policy, discussion, or changed-source text.
    Read {
        #[command(flatten)]
        input: WorkInput,
        #[arg(long)]
        reference: String,
        #[arg(long, default_value = "0")]
        offset: usize,
        #[arg(long, default_value = "4096")]
        limit: NonZeroUsize,
    },
    /// Record one change judgment, or the cumulative judgment.
    Judge {
        #[command(flatten)]
        input: WorkInput,
        #[arg(long)]
        change_id: String,
        #[arg(long)]
        disposition: Disposition,
        #[arg(long)]
        rationale: String,
        #[arg(long, required = true)]
        evidence: Vec<String>,
        #[arg(long)]
        previous_digest: Option<String>,
    },
    /// Require exact coverage and freeze the assessment.
    Complete(WorkInput),
    /// Enumerate open PRs and persist the shared commit-recency result.
    Discover {
        #[arg(long)]
        repository_url: String,
        #[arg(long)]
        state_path: PathBuf,
        #[arg(long)]
        policy_path: PathBuf,
        #[arg(long)]
        tenant: Option<String>,
        #[arg(long)]
        dry_run: bool,
        // A small first page avoids turning a large repository into unbounded context.
        #[arg(long, default_value = "10")]
        limit: NonZeroUsize,
    },
    /// Inspect a recent candidate using complete immutable approval evidence.
    Inspect {
        #[arg(long)]
        repository_url: String,
        #[arg(long)]
        state_path: PathBuf,
        #[arg(long)]
        pull_request_id: NonZeroU32,
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Read a bounded page of discovery results without provider access.
    Results {
        #[arg(long)]
        state_path: PathBuf,
        #[arg(long)]
        after: Option<NonZeroU32>,
        #[arg(long, default_value = "10")]
        limit: NonZeroUsize,
    },
}

/// All assessment commands bind the same immutable captured PR snapshot.
#[derive(Debug, Args)]
#[command(about = None, long_about = None)]
struct WorkInput {
    #[arg(long)]
    state_path: PathBuf,
    #[arg(long)]
    pull_request_id: NonZeroU32,
    #[arg(long)]
    snapshot_digest: String,
}

/// Shared Git operations, with bulk diff content carried on standard input.
#[derive(Debug, Subcommand)]
#[command(about = None, long_about = None)]
enum Git {
    /// Compare exact immutable commits already available in a local repository.
    Diff {
        #[arg(long)]
        repository_path: PathBuf,
        #[arg(long)]
        old_commit: String,
        #[arg(long)]
        new_commit: String,
    },
    /// Parse complete unified diff text from UTF-8 standard input.
    Parse,
    /// Normalize a quoted or service-facing Git path.
    Path {
        #[arg(long)]
        value: String,
        #[arg(long)]
        side_prefix: bool,
    },
}

/// Names the externally driven phases of cooperative review selection.
#[derive(Debug, Subcommand)]
#[command(about = None, long_about = None)]
enum Review {
    /// Claim the newest eligible pull request and return for preparation.
    Select(Selection),
    /// Check the reservation after preparation, replacing an invalid candidate.
    Confirm(Selection),
}

/// Groups participation operations that must not cast review verdicts.
#[derive(Debug, Subcommand)]
#[command(about = None, long_about = None)]
enum Reviewer {
    /// Add only an account not already reviewing the pull request.
    Register(Registration),
}

/// Carries the ownership and announcement inputs shared by select and confirm.
///
/// Confirmation supplies the same journal, session, and repository as selection;
/// the state machine checks them against persisted ownership before acting.
#[derive(Debug, Args)]
#[command(about = None, long_about = None)]
struct Selection {
    /// Fixes enumeration scope and the exact repository string used for ownership.
    #[arg(long, help = None, long_help = None)]
    repository_url: String,
    /// Identifies the primary session whose claim the journal may manage.
    #[arg(long, help = None, long_help = None)]
    session_id: String,
    /// Locates the absolute journal path that protects the entire transaction.
    #[arg(long, help = None, long_help = None)]
    state_path: PathBuf,
    /// Supplies caller-approved prose; the workflow appends its ownership marker.
    #[arg(long, help = None, long_help = None)]
    claim_message: String,
    /// Selects the Azure tenant for OAuth without persisting credentials.
    #[arg(long, help = None, long_help = None)]
    tenant: Option<String>,
}

/// Accepts browser coordinates or explicit registration coordinates.
///
/// Explicit fields override URL components. Azure DevOps may resolve missing
/// project and repository coordinates from the collection and PR identifier.
#[derive(Debug, Args)]
#[command(about = None, long_about = None)]
struct Registration {
    /// Provides browser coordinates when the caller already has a PR link.
    #[arg(long, help = None, long_help = None)]
    url: Option<String>,
    /// Selects routing explicitly, including hosts that cannot be inferred.
    #[arg(long, value_parser = ["gitHub", "azureDevOps"], help = None, long_help = None)]
    platform: Option<String>,
    /// Supplies the GitHub owner or Azure organization when no URL is available.
    #[arg(long, help = None, long_help = None)]
    organization: Option<String>,
    /// Overrides the Azure project, which GitHub does not use.
    #[arg(long, help = None, long_help = None)]
    project: Option<String>,
    /// Overrides the repository instead of inferring it from the current checkout.
    #[arg(long, help = None, long_help = None)]
    repository: Option<String>,
    /// Overrides or supplies the positive identifier within the selected service.
    #[arg(long, help = None, long_help = None)]
    pull_request_id: Option<NonZeroU32>,
    /// Overrides the Azure collection root, including on-premises deployments.
    #[arg(long, help = None, long_help = None)]
    collection_url: Option<String>,
    /// Selects the Azure OAuth tenant for this invocation only.
    #[arg(long, help = None, long_help = None)]
    tenant: Option<String>,
}

impl Registration {
    /// Resolves CLI precedence into the target passed to the provider adapter.
    ///
    /// Retains incomplete Azure coordinates for service-side identifier lookup;
    /// GitHub requires the repository before a request can be routed.
    fn target(&self) -> Result<Target, AppError> {
        let platform = self.platform.as_deref().map(|value| {
            if value == "gitHub" {
                Platform::GitHub
            } else {
                Platform::AzureDevOps
            }
        });
        let mut target = if let Some(url) = &self.url {
            Target::pull_request(url, platform)?
        } else {
            let platform = platform.ok_or_else(|| {
                fail(
                    "invalidInput",
                    "Supply --url or --platform with explicit coordinates",
                )
            })?;
            let organization = self
                .organization
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| fail("invalidInput", "Supply --organization"))?;
            let id = self
                .pull_request_id
                .ok_or_else(|| fail("invalidInput", "Supply --pull-request-id"))?;
            Target {
                platform,
                collection_url: (platform == Platform::AzureDevOps)
                    .then(|| format!("https://dev.azure.com/{}", escape(organization))),
                organization: organization.to_owned(),
                project: None,
                repository: String::new(),
                pull_request_id: id.get(),
                web_url: String::new(),
            }
        };
        if let Some(repository) = &self.repository {
            target.repository.clone_from(repository);
        }
        if let Some(id) = self.pull_request_id {
            target.pull_request_id = id.get();
        }
        if target.platform == Platform::AzureDevOps {
            if let Some(project) = &self.project {
                target.project = Some(project.clone());
            }
            if let Some(collection) = &self.collection_url {
                target.collection_url = Some(collection.trim_end_matches('/').to_owned());
            }
        } else if target.repository.trim().is_empty() {
            return Err(fail(
                "invalidInput",
                "A GitHub target requires --repository",
            ));
        }
        target.refresh_url();
        Ok(target)
    }
}

/// Runs a complete utility command with caller-supplied external dependencies.
///
/// This connects the package binary and its tests, not a supported Rust SDK.
/// Arguments include the executable name. Successful output is JSON except for
/// help and version text. The caller owns writing output and choosing an exit
/// status; failures retain their recovery category for [`crate::classification`].
///
/// Supply dependencies for the entire invocation so authentication, provider
/// effects, journal contention, and preparation waits use the same boundaries.
pub fn run(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<String, AppError> {
    execute(arguments, process, http, clock, None)
}

/// Injects the host storage boundary for embedded callers and isolated integration coverage.
pub fn run_in(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
    directory: &Path,
) -> Result<String, AppError> {
    execute(arguments, process, http, clock, Some(directory))
}

fn execute(
    arguments: &[OsString],
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
    directory: Option<&Path>,
) -> Result<String, AppError> {
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(error.to_string());
        }
        Err(error) => {
            return Err(Failure::caused_by(
                "invalidInput",
                "Invalid command arguments".to_owned(),
                error,
            )
            .into());
        }
    };
    let value = match cli.command {
        Operation::Autoreapprove { command } => match command {
            Autoreapprove::Publish {
                input,
                repository_url,
                attribution,
            } => {
                let repository = Target::repository(&repository_url)?;
                let directory = directory
                    .map_or_else(publication::data_directory, |path| Ok(path.to_owned()))?;
                let mut api = Api {
                    transport: Transport::new(&repository, None, process, http, clock),
                };
                discovery::publish(
                    &mut api,
                    clock,
                    &discovery::PublishInput {
                        path: &input.state_path,
                        number: input.pull_request_id,
                        snapshot: &input.snapshot_digest,
                        repository: &repository,
                        attribution: &attribution,
                        directory: &directory,
                    },
                )?
            }
            action @ (Autoreapprove::Prepare(_)
            | Autoreapprove::Assessment { .. }
            | Autoreapprove::Read { .. }
            | Autoreapprove::Judge { .. }
            | Autoreapprove::Complete(_)) => work(action, process, clock)?,
            Autoreapprove::Discover {
                repository_url,
                state_path,
                policy_path,
                tenant,
                dry_run,
                limit,
            } => {
                let repository = Target::repository(&repository_url)?;
                let mut api = Api {
                    transport: Transport::new(&repository, tenant.as_deref(), process, http, clock),
                };
                discovery::discover(
                    &mut api,
                    clock,
                    &DiscoveryInput {
                        repository: &repository,
                        state_path: &state_path,
                        policy_path: &policy_path,
                        tenant: tenant.as_deref(),
                        dry_run,
                        limit,
                    },
                )?
            }
            Autoreapprove::Inspect {
                repository_url,
                state_path,
                pull_request_id,
                tenant,
            } => {
                let repository = Target::repository(&repository_url)?;
                let mut api = Api {
                    transport: Transport::new(&repository, tenant.as_deref(), process, http, clock),
                };
                discovery::inspect(
                    &mut api,
                    clock,
                    &repository,
                    &state_path,
                    tenant.as_deref(),
                    pull_request_id,
                )?
            }
            Autoreapprove::Results {
                state_path,
                after,
                limit,
            } => discovery::results(&state_path, clock, after.map_or(0, NonZeroU32::get), limit)?,
        },
        Operation::Git { command } => match command {
            Git::Diff {
                repository_path,
                old_commit,
                new_commit,
            } => serde_json::to_value(crate::git::compare(
                process,
                &repository_path,
                &old_commit,
                &new_commit,
            )?)?,
            Git::Parse => {
                let mut text = String::new();
                io::stdin().read_to_string(&mut text)?;
                serde_json::json!({"files": crate::git_diff::parse(&text)?})
            }
            Git::Path { value, side_prefix } => {
                serde_json::json!({"path": crate::git_diff::normalize_path(&value, side_prefix)?})
            }
        },
        Operation::Review { command } => {
            let (arguments, confirm) = match command {
                Review::Select(arguments) => (arguments, false),
                Review::Confirm(arguments) => (arguments, true),
            };
            if !arguments.state_path.is_absolute()
                || !valid_session(&arguments.session_id)
                || arguments.claim_message.trim().is_empty()
            {
                return Err(fail(
                    "invalidInput",
                    "Supply an absolute state path, valid session identifier, and nonempty claim announcement",
                ));
            }
            let repository = Target::repository(&arguments.repository_url)?;
            let mut api = Api {
                transport: Transport::new(
                    &repository,
                    arguments.tenant.as_deref(),
                    process,
                    http,
                    clock,
                ),
            };
            let identity = api.identity()?;
            let state_path = arguments.state_path.to_str().ok_or_else(|| {
                fail(
                    "invalidInput",
                    "Journal path must be representable in UTF-8 JSON",
                )
            })?;
            let initial = State::new(&repository, &identity, &arguments.session_id, state_path);
            let mut journal = FileJournal::open(&arguments.state_path, clock)?;
            serde_json::to_value(selection::run(
                &mut api,
                &mut journal,
                clock,
                initial,
                &arguments.claim_message,
                confirm,
            )?)?
        }
        Operation::Reviewer {
            command: Reviewer::Register(arguments),
        } => {
            let target = arguments.target()?;
            let mut api = Api {
                transport: Transport::new(
                    &target,
                    arguments.tenant.as_deref(),
                    process,
                    http,
                    clock,
                ),
            };
            let identity = api.identity()?;
            api.register(&target, &identity)?
        }
    };
    Ok(serde_json::to_string(&value)?)
}

/// Maps named judgment inputs into the same typed boundary used by record validation.
fn work(
    command: Autoreapprove,
    process: &dyn Process,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let (input, action) = match command {
        Autoreapprove::Prepare(input) => (input, WorkAction::Prepare),
        Autoreapprove::Assessment {
            input,
            after,
            limit,
        } => (input, WorkAction::View { after, limit }),
        Autoreapprove::Read {
            input,
            reference,
            offset,
            limit,
        } => (
            input,
            WorkAction::Read {
                reference,
                offset,
                limit,
            },
        ),
        Autoreapprove::Judge {
            input,
            change_id,
            disposition,
            rationale,
            evidence,
            previous_digest,
        } => (
            input,
            WorkAction::Judge {
                id: change_id,
                previous_digest,
                judgment: Judgment {
                    disposition,
                    rationale,
                    evidence,
                },
            },
        ),
        Autoreapprove::Complete(input) => (input, WorkAction::Complete),
        _ => return Err(fail("invalidInput", "Not an assessment operation")),
    };
    discovery::work(
        &input.state_path,
        input.pull_request_id,
        &input.snapshot_digest,
        action,
        process,
        clock,
    )
}
