//! CLI translation for local review workflow operations.

use std::ffi::OsString;
use std::path::{PathBuf, absolute};
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand, ValueEnum};
use jiff::Timestamp;
use ohno::AppError;
use serde_json::json;
use ss_ai_review_workflow::{anchors, approved, audit, findings, markdown, workflow_plan};
use ss_ai_runtime::{Clock, Process, caused_by, fail, read_bytes};

/// Exposes local workflow operations without provider access.
#[derive(Debug, Parser)]
#[command(name = "ss-ai workflow", about = "Manage local review workflow data")]
struct WorkflowCli {
    #[command(subcommand)]
    command: WorkflowCommand,
}

/// Selects an operation and its file-based inputs.
#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    /// Render shared finding prose from a file-based request.
    Render {
        #[arg(long)]
        request_path: PathBuf,
    },
    /// Render finding files from a file-based request.
    RenderMarkdownFiles {
        #[arg(long)]
        request_path: PathBuf,
    },
    /// Resolve an anchor against captured diff evidence.
    ResolveAnchor {
        #[arg(long)]
        request_path: PathBuf,
    },
    /// Append an audit decision batch.
    AuditAddDecisions {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        plan_path: PathBuf,
        #[arg(long)]
        audit_feedback_path: Option<PathBuf>,
        #[arg(long)]
        policy_classification: bool,
        #[arg(long)]
        recorded_utc: String,
    },
    /// Poll the next finding requiring policy classification.
    AuditPollClassification {
        #[arg(long)]
        artifact_path: PathBuf,
        #[arg(long)]
        decisions_path: PathBuf,
    },
    /// Append verbatim human feedback.
    AuditAddFeedback {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        findings_path: PathBuf,
        #[arg(long)]
        response_id: String,
        #[arg(long)]
        finding_id: String,
        #[arg(long)]
        text_path: PathBuf,
    },
    /// Read human feedback for a finding.
    AuditGetFeedback {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        finding_id: String,
    },
    /// Check a merge decision against its pull request and source.
    AuditTestMergeApproval {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        pull_request_url: String,
        #[arg(long)]
        source_commit: String,
    },
    /// Record or carry forward a merge decision.
    AuditSetMergeApproval {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        context_path: PathBuf,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        carry_forward: bool,
        #[arg(long)]
        previous_context_path: Option<PathBuf>,
    },
    /// Record a merge decision using already loaded capture values.
    AuditSetMergeApprovalValues {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        context_path: PathBuf,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        carry_forward: bool,
        #[arg(long)]
        previous_context_path: Option<PathBuf>,
    },
    /// Build or validate an approved finding set.
    Approved {
        #[command(subcommand)]
        command: ApprovedCommand,
    },
    /// Create an empty workflow plan.
    PlanInitialize {
        #[arg(long, value_enum)]
        kind: PlanKind,
        #[arg(long)]
        path: PathBuf,
    },
    /// Record a validated workflow plan item.
    PlanAppend {
        #[arg(long, value_enum)]
        kind: PlanKind,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        item_path: PathBuf,
        #[arg(long)]
        replace_index: Option<usize>,
    },
    /// Create or resume a reviewer's finding artifact.
    InitializeFinding {
        #[arg(long)]
        output_directory: PathBuf,
        #[arg(long)]
        reviewer_id: String,
        #[arg(long)]
        focus_area: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        queue_path: Option<PathBuf>,
    },
    /// Add authored finding content to an artifact.
    AddFinding {
        #[arg(long)]
        artifact_path: PathBuf,
        #[arg(long)]
        finding_path: PathBuf,
        #[arg(long)]
        context_path: Option<PathBuf>,
        #[arg(long)]
        origin: String,
        #[arg(long)]
        source_id: Vec<String>,
    },
    /// Complete a reviewer artifact against its file-group journal.
    CompleteFinding {
        #[arg(long)]
        artifact_path: PathBuf,
        #[arg(long)]
        queue_path: PathBuf,
    },
    /// Validate candidate clusters against a finding index.
    ValidateCandidates {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        index_path: PathBuf,
    },
    /// Validate artifacts against explicitly named reviewers.
    ValidateFindings {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        reviewer_id: Vec<String>,
    },
    /// Build an index of all reviewer findings.
    FindingIndex {
        #[arg(long)]
        reviewer_directory: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
    },
    /// Consolidate reviewer findings with an optional merge plan.
    ConsolidateFindings {
        #[arg(long)]
        reviewer_directory: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
        #[arg(long)]
        plan_path: Option<PathBuf>,
    },
    /// Apply a single update or a validated update plan.
    UpdateFinding {
        #[arg(long)]
        artifact_path: PathBuf,
        #[arg(long)]
        finding_id: Option<String>,
        #[arg(long)]
        finding_path: Option<PathBuf>,
        #[arg(long)]
        plan_path: Option<PathBuf>,
    },
    /// Remove findings using a validated plan with authored reasons.
    RemoveFinding {
        #[arg(long)]
        artifact_path: PathBuf,
        #[arg(long)]
        plan_path: PathBuf,
        #[arg(long)]
        now_utc: Timestamp,
    },
}

/// Selects a complete approved-set operation.
#[derive(Debug, Subcommand)]
enum ApprovedCommand {
    /// Build the current approved projection.
    Build {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        decisions_path: PathBuf,
        #[arg(long)]
        output_path: PathBuf,
        #[arg(long)]
        context_path: Option<PathBuf>,
        #[arg(long)]
        repository_path: Option<PathBuf>,
    },
    /// Validate a projection against its capture.
    Check {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        context_path: Option<PathBuf>,
    },
    /// Validate already loaded projection and capture values.
    CheckRequest {
        #[arg(long)]
        request_path: PathBuf,
    },
}

/// Matches the plan kind names used by existing callers.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum PlanKind {
    #[value(name = "mergeCandidates")]
    MergeCandidates,
    #[value(name = "mergePlan")]
    MergePlan,
    #[value(name = "updates")]
    Updates,
    #[value(name = "removals")]
    Removals,
    #[value(name = "decisions")]
    Decisions,
}

impl PlanKind {
    /// Translates command names to the workflow schema selection.
    fn native(self) -> workflow_plan::PlanKind {
        match self {
            Self::MergeCandidates => workflow_plan::PlanKind::MergeCandidates,
            Self::MergePlan => workflow_plan::PlanKind::MergePlan,
            Self::Updates => workflow_plan::PlanKind::Updates,
            Self::Removals => workflow_plan::PlanKind::Removals,
            Self::Decisions => workflow_plan::PlanKind::Decisions,
        }
    }

    /// Keeps the caller's kind spelling in the result.
    fn name(self) -> &'static str {
        match self {
            Self::MergeCandidates => "mergeCandidates",
            Self::MergePlan => "mergePlan",
            Self::Updates => "updates",
            Self::Removals => "removals",
            Self::Decisions => "decisions",
        }
    }
}

/// Runs a local workflow command using the supplied external boundaries.
pub fn run<I, T>(arguments: I, process: &dyn Process, clock: &dyn Clock) -> Result<String, AppError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match WorkflowCli::try_parse_from(crate::command::namespace_arguments(
        arguments, "workflow",
    )) {
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
            return Err(caused_by("invalidInput", "Invalid command arguments", error).into());
        }
    };
    match cli.command {
        WorkflowCommand::Render { request_path } => {
            let bytes = read_bytes(&absolute(request_path)?, clock)?
                .ok_or_else(|| fail("invalidInput", "Rendering request is missing"))?;
            Ok(serde_json::to_string(&markdown::render_json(&bytes)?)?)
        }
        WorkflowCommand::RenderMarkdownFiles { request_path } => {
            let bytes = read_bytes(&absolute(request_path)?, clock)?
                .ok_or_else(|| fail("invalidInput", "Rendering request is missing"))?;
            Ok(serde_json::to_string(&markdown::render_files_json(
                &bytes, clock,
            )?)?)
        }
        WorkflowCommand::ResolveAnchor { request_path } => Ok(serde_json::to_string(
            &anchors::resolve_request_path(&absolute(request_path)?, clock)?,
        )?),
        WorkflowCommand::AuditAddDecisions {
            path,
            plan_path,
            audit_feedback_path,
            policy_classification,
            recorded_utc,
        } => {
            let audit_feedback_path = audit_feedback_path.map(absolute).transpose()?;
            Ok(serde_json::to_string(&audit::add_decisions(
                &absolute(path)?,
                &absolute(plan_path)?,
                audit_feedback_path.as_deref(),
                policy_classification,
                &recorded_utc,
                clock,
            )?)?)
        }
        WorkflowCommand::AuditPollClassification {
            artifact_path,
            decisions_path,
        } => Ok(serde_json::to_string(&audit::poll_classification(
            &absolute(artifact_path)?,
            &absolute(decisions_path)?,
            clock,
        )?)?),
        WorkflowCommand::AuditAddFeedback {
            path,
            findings_path,
            response_id,
            finding_id,
            text_path,
        } => {
            audit::add_feedback(
                &absolute(path)?,
                &absolute(findings_path)?,
                &response_id,
                &finding_id,
                &absolute(text_path)?,
                clock,
            )?;
            Ok(serde_json::to_string(&json!({
                "status": "Recorded", "findingId": finding_id, "responseId": response_id
            }))?)
        }
        WorkflowCommand::AuditGetFeedback { path, finding_id } => Ok(serde_json::to_string(
            &audit::get_feedback(&absolute(path)?, &finding_id, clock)?,
        )?),
        WorkflowCommand::AuditTestMergeApproval {
            path,
            pull_request_url,
            source_commit,
        } => Ok(serde_json::to_string(&audit::test_merge_approval(
            &absolute(path)?,
            &pull_request_url,
            &source_commit,
            clock,
        )?)?),
        WorkflowCommand::AuditSetMergeApproval {
            path,
            context_path,
            approve,
            carry_forward,
            previous_context_path,
        } => {
            let context_path = absolute(context_path)?;
            let previous_context_path = previous_context_path.map(absolute).transpose()?;
            let result = audit::set_merge_approval(
                &absolute(path)?,
                &context_path,
                approve,
                previous_context_path.as_deref(),
                carry_forward,
                clock,
            )?;
            Ok(serde_json::to_string(&result)?)
        }
        WorkflowCommand::AuditSetMergeApprovalValues {
            path,
            context_path,
            approve,
            carry_forward,
            previous_context_path,
        } => {
            let previous_context_path = previous_context_path.map(absolute).transpose()?;
            let result = audit::set_merge_approval_from_files(
                &absolute(path)?,
                &absolute(context_path)?,
                approve,
                previous_context_path.as_deref(),
                carry_forward,
                clock,
            )?;
            Ok(serde_json::to_string(&result)?)
        }
        WorkflowCommand::Approved { command } => match command {
            ApprovedCommand::Build {
                path,
                decisions_path,
                output_path,
                context_path,
                repository_path,
            } => {
                let context_path = context_path.map(absolute).transpose()?;
                let repository_path = repository_path.map(absolute).transpose()?;
                Ok(serde_json::to_string(&approved::build_approved(
                    &absolute(path)?,
                    &absolute(decisions_path)?,
                    &absolute(output_path)?,
                    context_path.as_deref(),
                    repository_path.as_deref(),
                    clock,
                    process,
                )?)?)
            }
            ApprovedCommand::Check { path, context_path } => {
                let context_path = context_path.map(absolute).transpose()?;
                approved::check_approved(&absolute(path)?, context_path.as_deref(), clock)?;
                Ok(serde_json::to_string(&json!({"status": "Valid"}))?)
            }
            ApprovedCommand::CheckRequest { request_path } => {
                approved::check_approved_request_path(&absolute(request_path)?, clock)?;
                Ok(serde_json::to_string(&json!({"status": "Valid"}))?)
            }
        },
        WorkflowCommand::PlanInitialize { kind, path } => {
            let path = absolute(path)?;
            workflow_plan::initialize(kind.native(), &path, clock)?;
            Ok(serde_json::to_string(&json!({
                "status": "Created", "path": path, "kind": kind.name()
            }))?)
        }
        WorkflowCommand::PlanAppend {
            kind,
            path,
            item_path,
            replace_index,
        } => {
            let path = absolute(path)?;
            let item_path = absolute(item_path)?;
            workflow_plan::append(kind.native(), &path, &item_path, replace_index, clock)?;
            Ok(serde_json::to_string(&json!({
                "status": "Recorded", "path": path, "kind": kind.name()
            }))?)
        }
        WorkflowCommand::InitializeFinding {
            output_directory,
            reviewer_id,
            focus_area,
            force,
            resume,
            queue_path,
        } => {
            let artifact_path = absolute(output_directory)?.join(format!("{reviewer_id}.json"));
            let (document, resumed_existing) = match queue_path {
                Some(queue_path) => {
                    let result = findings::initialize_with_queue_result(
                        &artifact_path,
                        &reviewer_id,
                        &focus_area,
                        force,
                        resume,
                        &absolute(queue_path)?,
                        clock,
                    )?;
                    (result.artifact, result.resumed_existing)
                }
                None if !resume => (
                    findings::initialize(&artifact_path, &reviewer_id, &focus_area, force, clock)?,
                    false,
                ),
                None => return Err(fail("invalidInput", "Resume requires a file-group journal")),
            };
            let count = document
                .get("findings")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| fail("invalidInput", "Finding artifact lacks findings"))?
                .len();
            Ok(serde_json::to_string(&json!({
                "artifactPath": artifact_path, "reviewerId": reviewer_id,
                "findingCount": count, "resumedExisting": resumed_existing
            }))?)
        }
        WorkflowCommand::AddFinding {
            artifact_path,
            finding_path,
            context_path,
            origin,
            source_id,
        } => {
            let artifact_path = absolute(artifact_path)?;
            let finding_path = absolute(finding_path)?;
            let context_path = context_path.map(absolute).transpose()?;
            let finding_id = findings::add_with_context(
                &artifact_path,
                &finding_path,
                context_path.as_deref(),
                &origin,
                &source_id,
                clock,
            )?;
            Ok(serde_json::to_string(&json!({
                "artifactPath": artifact_path, "findingId": finding_id
            }))?)
        }
        WorkflowCommand::CompleteFinding {
            artifact_path,
            queue_path,
        } => {
            let artifact_path = absolute(artifact_path)?;
            let document = findings::complete(&artifact_path, &absolute(queue_path)?, clock)?;
            let count = document
                .get("findings")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| fail("invalidInput", "Finding artifact lacks findings"))?
                .len();
            let status = document
                .get("status")
                .ok_or_else(|| fail("invalidInput", "Finding artifact lacks status"))?;
            Ok(serde_json::to_string(&json!({
                "artifactPath": artifact_path, "findingCount": count, "status": status
            }))?)
        }
        WorkflowCommand::ValidateCandidates { path, index_path } => {
            let count =
                findings::validate_candidates(&absolute(path)?, &absolute(index_path)?, clock)?;
            Ok(serde_json::to_string(
                &json!({"status": "Valid", "clusterCount": count}),
            )?)
        }
        WorkflowCommand::ValidateFindings { path, reviewer_id } => Ok(serde_json::to_string(
            &findings::validate_artifacts(&absolute(path)?, &reviewer_id, clock)?,
        )?),
        WorkflowCommand::FindingIndex {
            reviewer_directory,
            output_path,
        } => {
            let result = findings::build_index_result(
                &absolute(reviewer_directory)?,
                &absolute(output_path)?,
                clock,
            )?;
            Ok(serde_json::to_string(&json!({
                "outputPath": result.output_path, "artifactCount": result.artifact_count,
                "findingCount": result.finding_count
            }))?)
        }
        WorkflowCommand::ConsolidateFindings {
            reviewer_directory,
            output_path,
            plan_path,
        } => {
            let plan_path = plan_path.map(absolute).transpose()?;
            let result = findings::consolidate_result(
                &absolute(reviewer_directory)?,
                plan_path.as_deref(),
                &absolute(output_path)?,
                clock,
            )?;
            Ok(serde_json::to_string(&json!({
                "outputPath": result.output_path, "findingCount": result.finding_count,
                "mergedCount": result.merged_count, "carriedOverCount": result.carried_over_count
            }))?)
        }
        WorkflowCommand::UpdateFinding {
            artifact_path,
            finding_id,
            finding_path,
            plan_path,
        } => {
            let artifact_path = absolute(artifact_path)?;
            let updates = match (finding_id, finding_path, plan_path) {
                (Some(id), Some(path), None) => vec![(id, absolute(path)?)],
                (None, None, Some(path)) => {
                    let path = absolute(path)?;
                    let plan = workflow_plan::read(workflow_plan::PlanKind::Updates, &path, clock)?;
                    let parent = path.parent().ok_or_else(|| {
                        fail("invalidInput", "Update plan requires a parent directory")
                    })?;
                    plan.items()
                        .iter()
                        .map(|item| match item {
                            workflow_plan::PlanItem::Update(update) => Ok((
                                update.finding_id.clone(),
                                absolute(parent.join(&update.finding_path))?,
                            )),
                            _ => Err(fail("invalidInput", "Invalid update plan item")),
                        })
                        .collect::<Result<Vec<_>, AppError>>()?
                }
                _ => {
                    return Err(fail(
                        "invalidInput",
                        "Supply a plan or one finding and its content",
                    ));
                }
            };
            if updates.is_empty() {
                return Err(fail("invalidInput", "Update plan must not be empty"));
            }
            findings::update(&artifact_path, &updates, clock)?;
            let ids: Vec<_> = updates.into_iter().map(|(id, _)| id).collect();
            Ok(serde_json::to_string(
                &json!({"artifactPath": artifact_path, "updated": ids}),
            )?)
        }
        WorkflowCommand::RemoveFinding {
            artifact_path,
            plan_path,
            now_utc,
        } => {
            let artifact_path = absolute(artifact_path)?;
            let plan = workflow_plan::read(
                workflow_plan::PlanKind::Removals,
                &absolute(plan_path)?,
                clock,
            )?;
            let removals = plan
                .items()
                .iter()
                .map(|item| match item {
                    workflow_plan::PlanItem::Removal(removal) => {
                        Ok((removal.finding_id.clone(), removal.reason.clone()))
                    }
                    _ => Err(fail("invalidInput", "Invalid removal plan item")),
                })
                .collect::<Result<Vec<_>, AppError>>()?;
            if removals.is_empty() {
                return Err(fail("invalidInput", "Removal plan must not be empty"));
            }
            let clock = FixedTimestampClock {
                timestamp: now_utc,
                clock,
            };
            findings::remove(&artifact_path, &removals, &clock)?;
            let ids: Vec<_> = removals.into_iter().map(|(id, _)| id).collect();
            Ok(serde_json::to_string(
                &json!({"artifactPath": artifact_path, "removed": ids}),
            )?)
        }
    }
}

/// Uses the caller's timestamp while retaining the ordinary retry wait policy.
struct FixedTimestampClock<'a> {
    timestamp: Timestamp,
    clock: &'a dyn Clock,
}

impl Clock for FixedTimestampClock<'_> {
    fn now(&self) -> Timestamp {
        self.timestamp
    }

    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        self.clock.wait(duration)
    }
}
