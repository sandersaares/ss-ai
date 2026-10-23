//! Normalizes account evidence for ownership and non-destructive registration.

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::fail;
use crate::json::{field, identifier, required_optional};
use crate::target::Platform;

/// Preserves the identity shape shared by registrations and schema-2 journals.
///
/// Optional identifiers reflect provider evidence, not interchangeable authority.
/// Use `same_account` for ownership and `may_match` to avoid overwriting an
/// existing reviewer. Neither comparison uses the presentation name.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Identity {
    /// Prevents identifiers from different service namespaces from matching.
    pub(crate) platform: Platform,
    /// Provider account identifier; authenticated journal identities require it.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) id: Option<String>,
    /// Human-readable label, never sufficient evidence of account ownership.
    pub(crate) display_name: String,
    /// Login or account name used when stronger shared identifiers are unavailable.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) unique_name: Option<String>,
    /// Provider descriptor or node ID, preferred for ownership comparison.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) descriptor: Option<String>,
    /// Provider account-type evidence, when supplied, for classification.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) account_type: Option<String>,
    /// Person/service/indeterminate classification reported without guessing.
    pub(crate) kind: String,
    /// Provider facts supporting `kind`, retained for consumers inspecting identity.
    pub(crate) evidence: Vec<String>,
}

impl Identity {
    /// Normalizes service account fields without using display names as identity.
    pub(crate) fn from_account(platform: Platform, node: &Value) -> Result<Self, AppError> {
        if !node.is_object() {
            return Err(fail("service", "Missing identity object"));
        }
        let get = |name| -> Result<Option<String>, AppError> {
            match node.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) => Ok((!value.trim().is_empty()).then(|| value.clone())),
                Some(_) => Err(fail("service", format!("Invalid identity field {name}"))),
            }
        };
        let id = node
            .get("id")
            .filter(|value| !value.is_null())
            .map(identifier)
            .transpose()?;
        let github = platform == Platform::GitHub;
        let unique_name = get(if github { "login" } else { "uniqueName" })?;
        let descriptor = get(if github { "node_id" } else { "descriptor" })?;
        let account_type = if github { get("type")? } else { None };
        let display_name = if github {
            unique_name.clone()
        } else {
            get("displayName")?
        }
        .or_else(|| unique_name.clone())
        .unwrap_or_else(|| "unknown".to_owned());
        let mut kind = "indeterminate";
        let mut evidence = Vec::new();
        if github {
            let descriptor = descriptor.as_deref().unwrap_or_default();
            let url = get("html_url")?.unwrap_or_default();
            if account_type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("Bot"))
            {
                kind = "service";
                evidence.push("accountType:Bot".to_owned());
            } else if descriptor.to_ascii_lowercase().starts_with("bot_") {
                kind = "service";
                evidence.push("nodeId:BOT_".to_owned());
            } else if url
                .to_ascii_lowercase()
                .starts_with("https://github.com/apps/")
            {
                kind = "service";
                evidence.push("url:apps".to_owned());
            } else if account_type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("User"))
            {
                kind = "person";
                evidence.push("accountType:User".to_owned());
            }
        } else if let Some(prefix) = descriptor
            .as_deref()
            .and_then(|value| value.split_once('.'))
            .map(|(prefix, _)| prefix)
        {
            if ["aadsp", "s2s", "svc"]
                .iter()
                .any(|value| prefix.eq_ignore_ascii_case(value))
            {
                kind = "service";
            } else if ["aad", "msa"]
                .iter()
                .any(|value| prefix.eq_ignore_ascii_case(value))
            {
                kind = "person";
            }
            if kind != "indeterminate" {
                evidence.push(format!("descriptor:{prefix}"));
            }
        }
        Ok(Self {
            platform,
            id,
            display_name,
            unique_name,
            descriptor,
            account_type,
            kind: kind.to_owned(),
            evidence,
        })
    }

    /// Connection data identifies the authenticated account, not a delegated one.
    pub(crate) fn from_connection(node: &Value) -> Result<Self, AppError> {
        let user = field(node, "authenticatedUser")?;
        let account = user.pointer("/properties/Account");
        let account = account.map_or(Value::Null, |value| {
            if value.get("$type").is_some() {
                value.get("$value").cloned().unwrap_or(Value::Null)
            } else {
                value.clone()
            }
        });
        let normalized = serde_json::json!({
            "id": field(user, "id")?,
            "displayName": user.get("providerDisplayName"),
            "descriptor": user.get("subjectDescriptor"),
            "uniqueName": account
        });
        Self::from_account(Platform::AzureDevOps, &normalized)
    }

    /// Preserves team identities for registration without expanding team membership.
    pub(crate) fn from_team(node: &Value) -> Result<Self, AppError> {
        let normalized = serde_json::json!({
            "id": node.get("id"), "login": node.get("slug"),
            "node_id": node.get("node_id"), "type": "Team", "html_url": node.get("html_url")
        });
        let mut identity = Self::from_account(Platform::GitHub, &normalized)?;
        if let Some(name) = node
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        {
            name.clone_into(&mut identity.display_name);
        }
        Ok(identity)
    }

    /// Conservative matching protects existing votes from duplicate registration.
    ///
    /// Any shared identifier agreement is sufficient to preserve an existing
    /// reviewer, even if other evidence differs. Do not substitute this for the
    /// stricter journal account check.
    pub(crate) fn may_match(&self, other: &Self) -> bool {
        self.matches(other).any(|matches| matches)
    }

    /// Journal ownership uses the most authoritative shared identifier.
    ///
    /// A disagreement on that identifier cannot be repaired by a weaker matching
    /// login. With no shared identifiers, ownership is not established.
    pub(crate) fn same_account(&self, other: &Self) -> bool {
        self.matches(other).next().unwrap_or(false)
    }

    /// Orders shared identifier comparisons from strongest to fallback evidence.
    ///
    /// Keeping disagreements in the iterator lets ownership honor the first
    /// shared identifier while duplicate protection accepts any agreement.
    fn matches<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = bool> + 'a {
        [
            (&self.descriptor, &other.descriptor),
            (&self.id, &other.id),
            (&self.unique_name, &other.unique_name),
        ]
        .into_iter()
        .filter_map(|(left, right)| {
            left.as_deref().zip(right.as_deref()).map(|(left, right)| {
                self.platform == other.platform && left.to_lowercase() == right.to_lowercase()
            })
        })
    }

    /// Ensures persisted identity evidence is structurally and semantically usable.
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.display_name.trim().is_empty()
            || !["person", "service", "indeterminate"].contains(&self.kind.as_str())
            || [
                &self.id,
                &self.descriptor,
                &self.unique_name,
                &self.account_type,
            ]
            .iter()
            .any(|value| {
                value
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
            })
        {
            return Err(fail("invalidState", "Malformed journal identity"));
        }
        Ok(())
    }
}
