//! Adapts Azure work items to the tracker publication boundary.
//!
//! Preflight resolves the project-specific metadata before any item is filed.
//! Each creation is a single JSON Patch write, followed by a separate read.

use std::num::NonZeroU32;

use ohno::AppError;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::error::{caused_by, fail};
use crate::json::{array, field, text};
use crate::target::{Platform, decode, escape};
use crate::tracker_body;
use crate::tracker_target::TrackerTarget as Target;
use crate::transport::Transport;

/// Azure's preview resource supports the per-field Markdown format operation.
const VERSION: &str = "7.2-preview.3";
/// The service's standard body field, used when no field is specified.
const DESCRIPTION: &str = "System.Description";
/// Bug forms may display this field instead of the standard description.
const REPRO: &str = "Microsoft.VSTS.TCM.ReproSteps";

/// Validates the destination and resolves the metadata needed to file items.
pub(crate) fn state(
    transport: &mut Transport<'_>,
    target: &Target,
    metadata: &Value,
) -> Result<Value, AppError> {
    let (collection, project) = destination(transport, target)?;
    let requested_type = input_text(metadata, "workItemType")?;
    let requested_area = input_text(metadata, "areaPath")?;
    let description = match metadata.get("descriptionField") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.trim().is_empty() => None,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => return Err(fail("invalidInput", "Invalid description field")),
    };
    validate_description_field(description.unwrap_or(DESCRIPTION))?;
    let parent_id = match metadata.get("parentWorkItemId") {
        None | Some(Value::Null) => None,
        Some(value) => Some(positive_id(value, "invalidInput")?),
    };
    let segments: Vec<_> = requested_area.split('\\').collect();
    if segments
        .iter()
        .any(|segment| segment.trim().is_empty() || matches!(*segment, "." | ".."))
        || segments
            .first()
            .is_none_or(|segment| !same_name(segment, project))
    {
        return Err(fail(
            "invalidInput",
            "Area path must be rooted at the tracker project",
        ));
    }

    let project_segment = escape(project);
    let project_record = get(
        transport,
        &format!("_apis/projects/{project_segment}"),
        &[],
        "7.1",
    )?;
    let service_project = service_text(&project_record, "name")?;
    if !same_name(service_project, project) {
        return Err(fail("service", "Azure returned a different project"));
    }
    let types = transport.list(&format!("{project_segment}/_apis/wit/workitemtypes"))?;
    let mut resolved_type = None;
    for candidate in &types {
        let name = service_text(candidate, "name")?;
        if same_name(name, requested_type) && resolved_type.replace(name.to_owned()).is_some() {
            return Err(fail("service", "Azure returned duplicate work item types"));
        }
    }
    let resolved_type = resolved_type.ok_or_else(|| {
        fail(
            "invalidInput",
            "The project does not define the work item type",
        )
    })?;
    let type_record = get(
        transport,
        &format!(
            "{project_segment}/_apis/wit/workitemtypes/{}",
            escape(&resolved_type)
        ),
        &[],
        "7.1",
    )?;
    let fields = array(field(&type_record, "fields")?)?;
    let requested_field = description.unwrap_or(DESCRIPTION);
    let mut resolved_field = None;
    let mut has_repro = false;
    for entry in fields {
        let name = service_text(entry, "referenceName")?;
        if name.eq_ignore_ascii_case(requested_field)
            && resolved_field.replace(name.to_owned()).is_some()
        {
            return Err(fail("service", "Azure returned duplicate work item fields"));
        }
        has_repro |= name.eq_ignore_ascii_case(REPRO);
    }
    let resolved_field = resolved_field.ok_or_else(|| {
        fail(
            "invalidInput",
            "The work item type does not carry the description field",
        )
    })?;
    if has_repro && description.is_none() {
        return Err(fail(
            "invalidInput",
            "Choose the description field explicitly for this work item type",
        ));
    }
    validate_description_field(&resolved_field)?;
    let field_record = get(
        transport,
        &format!(
            "{project_segment}/_apis/wit/fields/{}",
            escape(&resolved_field)
        ),
        &[],
        "7.1",
    )?;
    // Azure's Markdown documentation describes rich-text fields; a long
    // plain-text field alone does not establish that it renders Markdown.
    // Ref: https://devblogs.microsoft.com/devops/markdown-support-arrives-for-work-items/
    if !service_text(&field_record, "referenceName")?.eq_ignore_ascii_case(&resolved_field)
        || !service_text(&field_record, "type")?.eq_ignore_ascii_case("html")
    {
        return Err(fail(
            "invalidInput",
            "Description field must be a rich-text HTML field",
        ));
    }

    let mut area_route = format!("{project_segment}/_apis/wit/classificationnodes/areas");
    for segment in segments.iter().skip(1) {
        area_route.push('/');
        area_route.push_str(&escape(segment));
    }
    let area = get(transport, &area_route, &[], "7.1")?;
    let area_name = service_text(&area, "name")?;
    if !same_name(area_name, segments.last().expect("area has a root")) {
        return Err(fail("service", "Azure returned a different area"));
    }
    let mut resolved_area = segments.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if let Some(root) = resolved_area.first_mut() {
        project.clone_into(root);
    }
    if resolved_area.len() > 1 {
        area_name.clone_into(
            resolved_area
                .last_mut()
                .expect("area has at least one segment"),
        );
    } else if !same_name(area_name, project) {
        return Err(fail("service", "Azure returned a different area root"));
    }

    let parent = if let Some(id) = parent_id {
        let response = get(
            transport,
            &format!("_apis/wit/workitems/{id}"),
            &[("fields", "System.TeamProject,System.Title".to_owned())],
            "7.1",
        )?;
        if positive_id(field(&response, "id")?, "service")? != id {
            return Err(fail(
                "service",
                "Azure returned a different parent work item",
            ));
        }
        let fields = field(&response, "fields")?;
        if !same_name(service_text(fields, "System.TeamProject")?, project) {
            return Err(fail(
                "invalidInput",
                "Parent work item belongs to another project",
            ));
        }
        let url = service_text(&response, "url")?;
        if !parent_url_matches(url, collection, project, id) {
            return Err(fail(
                "service",
                "Azure returned an unexpected parent API URL",
            ));
        }
        Some(json!({
            "itemId": id.to_string(),
            "title": service_text(fields, "System.Title")?,
            "url": url,
            "webUrl": item_url(collection, project, id),
        }))
    } else {
        None
    };
    Ok(json!({
        "platform": "azureDevOps",
        "trackerName": format!("{}/{project}", target.organization),
        "collectionUrl": collection,
        "project": project,
        "workItemType": resolved_type,
        "areaPath": resolved_area.join("\\"),
        "descriptionField": resolved_field,
        "parent": parent,
    }))
}

/// Files exactly one item without assigning it to a user.
pub(crate) fn create(
    transport: &mut Transport<'_>,
    target: &Target,
    state: &Value,
    item: &Value,
) -> Result<Value, AppError> {
    let (collection, project) = destination(transport, target)?;
    validate_state(state, collection, project)?;
    let title = input_text(item, "title")?;
    let body = item
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidInput", "Item body must be a string"))?;
    let description = input_text(state, "descriptionField")?;
    validate_description_field(description)?;
    let mut patch = vec![
        json!({"op":"add", "path":"/fields/System.Title", "value":title}),
        json!({"op":"add", "path":format!("/fields/{description}"), "value":tracker_body::encode_ado_body(body)}),
        json!({"op":"add", "path":"/fields/System.AreaPath", "value":input_text(state, "areaPath")?}),
        json!({"op":"add", "path":format!("/multilineFieldsFormat/{description}"), "value":"Markdown"}),
    ];
    let parent = state
        .get("parent")
        .ok_or_else(|| fail("invalidInput", "Tracker state is missing the parent field"))?;
    if !parent.is_null() {
        let id = positive_id(field(parent, "itemId")?, "invalidInput")?;
        let url = input_text(parent, "url")?;
        if !parent_url_matches(url, collection, project, id) {
            return Err(fail("invalidInput", "Invalid parent work item URL"));
        }
        patch.push(json!({"op":"add", "path":"/relations/-", "value":{
            "rel":"System.LinkTypes.Hierarchy-Reverse", "url":url
        }}));
    }
    let path = format!(
        "{}/_apis/wit/workitems/${}",
        escape(project),
        escape(input_text(state, "workItemType")?)
    );
    let page = transport.ado_json_patch(&path, "POST", &Value::Array(patch), &[], VERSION)?;
    if page.continuation.is_some() {
        return Err(fail(
            "ambiguousWrite",
            "Azure work item creation returned a continuation",
        ));
    }
    record(target, state, &page.payload).map_err(|error| {
        caused_by(
            "ambiguousWrite",
            "Azure work item creation returned incomplete evidence",
            error.into_std_error(),
        )
        .into()
    })
}

/// Reads the filed item rather than assuming the service stored the request.
pub(crate) fn read(
    transport: &mut Transport<'_>,
    target: &Target,
    state: &Value,
    receipt: &Value,
) -> Result<Value, AppError> {
    let (collection, project) = destination(transport, target)?;
    validate_state(state, collection, project)?;
    let id = positive_id(field(receipt, "itemId")?, "invalidInput")?;
    let path = format!("{}/_apis/wit/workitems/{id}", escape(project));
    let response = get(transport, &path, &[], VERSION)?;
    let record = record(target, state, &response)?;
    if positive_id(field(&record, "itemId")?, "service")? != id {
        return Err(fail("service", "Azure returned a different work item"));
    }
    Ok(record)
}

/// Prevents a tracker state from redirecting operations to another collection.
fn validate_state(state: &Value, collection: &str, project: &str) -> Result<(), AppError> {
    if state.get("platform").and_then(Value::as_str) != Some("azureDevOps")
        || input_text(state, "collectionUrl")? != collection
        || !same_name(input_text(state, "project")?, project)
    {
        return Err(fail(
            "invalidInput",
            "Tracker state does not match the destination",
        ));
    }
    let area = input_text(state, "areaPath")?;
    if area
        .split('\\')
        .any(|part| part.trim().is_empty() || matches!(part, "." | ".."))
        || !same_name(area.split('\\').next().unwrap_or_default(), project)
    {
        return Err(fail("invalidInput", "Tracker state has an invalid area"));
    }
    Ok(())
}

/// Matches Azure names using the same Unicode casing as tracker identities.
fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Restricts parent links to the collection and globally identified work item.
fn parent_url_matches(url: &str, collection: &str, project: &str, id: NonZeroU32) -> bool {
    let suffix = format!("_apis/wit/workItems/{id}");
    if url.eq_ignore_ascii_case(&format!("{collection}/{suffix}")) {
        return true;
    }

    // Azure can qualify the route with a project ID instead of its name.
    // The parent response separately establishes project membership.
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(segment) = parsed
        .path_segments()
        .and_then(|mut segments| segments.nth_back(4))
    else {
        return false;
    };
    let Ok(decoded) = decode(segment) else {
        return false;
    };
    (same_name(&decoded, project) || Uuid::parse_str(&decoded).is_ok())
        && url.eq_ignore_ascii_case(&format!("{collection}/{segment}/{suffix}"))
}

/// Rejects malformed or reserved body fields before authentication or writes.
pub(crate) fn validate_description_field(field: &str) -> Result<(), AppError> {
    // Azure's field-definition schema bounds the name and requires a
    // namespace followed by nonempty, ASCII identifier segments.
    // Ref: https://learn.microsoft.com/en-us/previous-versions/azure/devops/reference/xml/field-definition-element-reference
    let mut segments = field.split('.');
    let namespace = segments.next().unwrap_or_default();
    let identifier = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    if field.len() > 70
        || !identifier(namespace)
        || !namespace
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        || !field.contains('.')
        || !segments.all(identifier)
        || [
            "System.AssignedTo",
            "System.Title",
            "System.AreaPath",
            "System.TeamProject",
        ]
        .iter()
        .any(|reserved| field.eq_ignore_ascii_case(reserved))
    {
        return Err(fail("invalidInput", "Invalid work item description field"));
    }
    Ok(())
}

/// Ensures every request and browser link uses the transport's collection.
fn destination<'a>(
    transport: &Transport<'_>,
    target: &'a Target,
) -> Result<(&'a str, &'a str), AppError> {
    target.validate()?;
    let collection = target
        .collection_url
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "Missing Azure collection"))?
        .trim_end_matches('/');
    let project = target
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty() && !matches!(*value, "." | ".."))
        .ok_or_else(|| fail("invalidInput", "Missing Azure tracker project"))?;
    let url = Url::parse(collection)
        .map_err(|error| caused_by("invalidInput", "Invalid Azure collection", error))?;
    if target.platform != Platform::AzureDevOps
        || transport.platform != Platform::AzureDevOps
        || transport.ado_collection().map(|s| s.trim_end_matches('/')) != Some(collection)
        || target.organization.trim().is_empty()
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str().trim_end_matches('/') != collection
    {
        return Err(fail("invalidInput", "Invalid Azure tracker destination"));
    }
    Ok((collection, project))
}

/// Rejects truncated singleton responses rather than discarding continuation.
fn get(
    transport: &mut Transport<'_>,
    path: &str,
    query: &[(&str, String)],
    version: &str,
) -> Result<Value, AppError> {
    let page = transport.ado(path, "GET", None, query, version)?;
    if page.continuation.is_some() {
        return Err(fail("service", "Unexpected Azure response continuation"));
    }
    Ok(page.payload)
}

/// Normalizes Azure response evidence to the shared tracker record.
fn record(target: &Target, state: &Value, work_item: &Value) -> Result<Value, AppError> {
    let id = positive_id(field(work_item, "id")?, "service")?;
    let fields = field(work_item, "fields")?;
    let description = input_text(state, "descriptionField")?;
    let formats = work_item.get("multilineFieldsFormat");
    let format = match formats {
        None | Some(Value::Null) => "",
        Some(Value::Object(formats)) => match formats.get(description) {
            None | Some(Value::Null) => "",
            Some(Value::String(value)) => value,
            _ => return Err(fail("service", "Invalid work item description format")),
        },
        _ => return Err(fail("service", "Invalid work item description formats")),
    };
    let collection = target
        .collection_url
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    let project = target.project.as_deref().unwrap_or_default();
    Ok(json!({
        "itemId": id.to_string(), "itemNodeId": id.to_string(),
        "url": item_url(collection, project, id),
        "title": service_text(fields, "System.Title")?,
        "body": text(field(fields, description)?)?,
        "descriptionFormat": format,
    }))
}

/// Constructs the human-facing link from verified coordinates.
fn item_url(collection: &str, project: &str, id: NonZeroU32) -> String {
    format!("{collection}/{}/_workitems/edit/{id}", escape(project))
}

/// Accepts only positive numeric IDs so IDs cannot alter request paths.
fn positive_id(value: &Value, kind: &'static str) -> Result<NonZeroU32, AppError> {
    let number = value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .or_else(|| {
            let text = value.as_str()?;
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            text.parse().ok()
        });
    number
        .and_then(NonZeroU32::new)
        .ok_or_else(|| fail(kind, "Invalid work item identifier"))
}

/// Requires actual nonempty strings at an input boundary.
fn input_text<'a>(value: &'a Value, name: &str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| fail("invalidInput", format!("Missing or invalid {name}")))
}

/// Requires actual nonempty strings in service metadata.
fn service_text<'a>(value: &'a Value, name: &str) -> Result<&'a str, AppError> {
    let result = text(field(value, name)?)?;
    if result.trim().is_empty() {
        return Err(fail("service", format!("Empty service field {name}")));
    }
    Ok(result)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    reason = "Test fixtures have fixed shapes, and a missing element should fail the test"
)]
#[expect(
    clippy::assertions_on_result_states,
    reason = "These tests check refusal and acceptance without requiring a particular error"
)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;

    use super::*;
    use crate::target::Target as TransportTarget;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput};

    /// Captures the transport call and its decoded request body.
    type Request = (String, String, Option<Value>, bool);

    /// Preserves the ordered requests so the test observes every effect.
    struct Mock {
        replies: RefCell<VecDeque<Value>>,
        requests: RefCell<Vec<Request>>,
    }

    impl Mock {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn reply(&self, method: &str, url: &str, body: Option<&[u8]>, patch: bool) -> HttpResponse {
            self.requests.borrow_mut().push((
                method.to_owned(),
                url.to_owned(),
                body.map(|bytes| serde_json::from_slice(bytes).unwrap()),
                patch,
            ));
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            HttpResponse::new(200, BTreeMap::new(), serde_json::to_vec(&reply).unwrap())
        }
    }

    impl Process for Mock {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "az");
            Ok(ProcessOutput::new(true, b"token".to_vec(), Vec::new()))
        }
    }

    impl Http for Mock {
        fn send(
            &self,
            method: &str,
            url: &str,
            _: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Ok(self.reply(method, url, body, false))
        }

        fn send_json_patch(
            &self,
            method: &str,
            url: &str,
            _: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Ok(self.reply(method, url, body, true))
        }
    }

    /// Unexpected retry waits indicate a violation of the single-write contract.
    struct NoWait;

    impl Clock for NoWait {
        fn now(&self) -> Timestamp {
            "2026-01-01T00:00:00Z".parse().unwrap()
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("unexpected retry")
        }
    }

    fn target() -> Target {
        Target {
            platform: Platform::AzureDevOps,
            collection_url: Some("https://dev.azure.com/org".into()),
            organization: "org".into(),
            project: Some("Prøject".into()),
            repository: None,
            web_url: "https://dev.azure.com/org/Pr%C3%B8ject/_workitems".into(),
        }
    }

    fn transport_target() -> TransportTarget {
        TransportTarget {
            platform: Platform::AzureDevOps,
            collection_url: Some("https://dev.azure.com/org".into()),
            organization: "org".into(),
            project: Some("Prøject".into()),
            repository: String::new(),
            pull_request_id: 0,
            web_url: String::new(),
        }
    }

    fn preflight() -> Vec<Value> {
        vec![
            json!({"name":"Prøject"}),
            json!({"count":1,"value":[{"name":"Bug"}]}),
            json!({"fields":[{"referenceName":"System.Description"},{"referenceName":REPRO}]}),
            json!({"referenceName":REPRO,"type":"html"}),
            json!({"name":"Sub Area"}),
            json!({"id":9,"url":"https://dev.azure.com/org/_apis/wit/workItems/9",
                "fields":{"System.TeamProject":"Prøject","System.Title":"Parent"}}),
        ]
    }

    fn metadata() -> Value {
        json!({"workItemType":"bug","areaPath":"Prøject\\Sub Area",
            "descriptionField":REPRO,"parentWorkItemId":9})
    }

    fn work_item() -> Value {
        json!({"id":12,"fields":{"System.Title":"Tést", (REPRO):"&lt;é&gt; &amp;"},
            "multilineFieldsFormat":{(REPRO):"Markdown"}})
    }

    #[test]
    fn preflight_create_and_read_use_exact_provider_contract() {
        let mut replies = preflight();
        replies.extend([work_item(), work_item()]);
        let mock = Mock::new(replies);
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let target = target();
        let state = state(&mut transport, &target, &metadata()).unwrap();
        let receipt = create(
            &mut transport,
            &target,
            &state,
            &json!({"title":"Tést","body":"<é> &amp;"}),
        )
        .unwrap();
        assert_eq!(
            read(&mut transport, &target, &state, &receipt).unwrap(),
            receipt
        );
        assert_eq!(receipt["body"], "&lt;é&gt; &amp;");
        assert_eq!(receipt["descriptionFormat"], "Markdown");
        assert_eq!(
            receipt["url"],
            "https://dev.azure.com/org/Pr%C3%B8ject/_workitems/edit/12"
        );
        let requests = mock.requests.borrow();
        assert_eq!(requests.len(), 8);
        assert!(
            requests[4]
                .1
                .contains("Pr%C3%B8ject/_apis/wit/classificationnodes/areas/Sub%20Area")
        );
        assert!(
            requests[5]
                .1
                .contains("fields=System.TeamProject%2CSystem.Title")
        );
        let (method, url, body, patch) = &requests[6];
        assert_eq!(method, "POST");
        assert!(*patch);
        assert!(url.contains("/Pr%C3%B8ject/_apis/wit/workitems/$Bug?api-version=7.2-preview.3"));
        assert_eq!(
            body.as_ref().unwrap(),
            &json!([
                {"op":"add","path":"/fields/System.Title","value":"Tést"},
                {"op":"add","path":format!("/fields/{REPRO}"),"value":"&lt;é&gt; &amp;"},
                {"op":"add","path":"/fields/System.AreaPath","value":"Prøject\\Sub Area"},
                {"op":"add","path":format!("/multilineFieldsFormat/{REPRO}"),"value":"Markdown"},
                {"op":"add","path":"/relations/-","value":{"rel":"System.LinkTypes.Hierarchy-Reverse","url":"https://dev.azure.com/org/_apis/wit/workItems/9"}}
            ])
        );
        assert!(requests[7].1.contains("api-version=7.2-preview.3"));
    }

    #[test]
    fn unicode_names_and_project_qualified_parent_survive_preflight_and_creation() {
        let parent_url =
            "https://dev.azure.com/org/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/9";
        let mut replies = preflight();
        replies[0]["name"] = json!("PrØject");
        replies[1]["value"][0]["name"] = json!("BÜG");
        replies[4]["name"] = json!("SÜB Area");
        replies[5]["fields"]["System.TeamProject"] = json!("PRØJECT");
        replies[5]["url"] = json!(parent_url);
        replies.push(work_item());
        let mock = Mock::new(replies);
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let input = json!({"workItemType":"büg","areaPath":"prØject\\süb area",
            "descriptionField":REPRO,"parentWorkItemId":9});
        let mut saved = state(&mut transport, &target(), &input).unwrap();
        assert_eq!(saved["workItemType"], "BÜG");
        assert_eq!(saved["areaPath"], "Prøject\\SÜB Area");
        assert_eq!(saved["parent"]["url"], parent_url);
        saved["project"] = json!("PRØJECT");
        saved["areaPath"] = json!("PRØJECT\\SÜB Area");
        create(
            &mut transport,
            &target(),
            &saved,
            &json!({"title":"Tést","body":"body"}),
        )
        .unwrap();
        assert_eq!(
            mock.requests.borrow()[6].2.as_ref().unwrap()[4]["value"]["url"],
            parent_url
        );
    }

    #[test]
    fn parent_links_preserve_collection_and_work_item_identity() {
        let id = NonZeroU32::new(9).unwrap();
        let collection = "https://dev.azure.com/org";
        let project = "Prøject";
        for url in [
            "https://dev.azure.com/org/_apis/wit/workItems/9",
            "https://dev.azure.com/org/Pr%C3%B8ject/_apis/wit/workItems/9",
            "https://dev.azure.com/org/Pr%C3%98ject/_apis/wit/workItems/9",
            "https://dev.azure.com/org/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/9",
        ] {
            assert!(parent_url_matches(url, collection, project, id), "{url}");
        }
        for url in [
            "https://dev.azure.com/other/_apis/wit/workItems/9",
            "https://dev.azure.com/org/Other/_apis/wit/workItems/9",
            "https://dev.azure.com/org/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/10",
            "https://dev.azure.com/org/_apis/wit/workItems/9?redirect=other",
            "https://dev.azure.com/org/extra/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/9",
        ] {
            assert!(!parent_url_matches(url, collection, project, id), "{url}");
        }
    }

    #[test]
    fn foreign_parent_url_is_rejected_before_creation() {
        let mut replies = preflight();
        replies[5]["url"] = json!("https://dev.azure.com/other/_apis/wit/workItems/9");
        let mock = Mock::new(replies);
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        assert!(state(&mut transport, &target(), &metadata()).is_err());
        assert!(
            mock.requests
                .borrow()
                .iter()
                .all(|request| request.0 == "GET")
        );

        let mock = Mock::new(Vec::new());
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let saved = json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org",
            "project":"Prøject","areaPath":"Prøject","workItemType":"Bug",
            "descriptionField":DESCRIPTION,"parent":{"itemId":"9",
                "url":"https://dev.azure.com/other/_apis/wit/workItems/9"}});
        assert!(
            create(
                &mut transport,
                &target(),
                &saved,
                &json!({"title":"T","body":"body"})
            )
            .is_err()
        );
        assert!(mock.requests.borrow().is_empty());
    }

    #[test]
    fn rejects_missing_explicit_field_and_foreign_parent_before_writing() {
        let mock = Mock::new(preflight());
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let mut input = metadata();
        input["descriptionField"] = Value::Null;
        assert!(state(&mut transport, &target(), &input).is_err());
        assert_eq!(mock.requests.borrow().len(), 3);

        let mut replies = preflight();
        replies[5]["fields"]["System.TeamProject"] = json!("Other");
        let mock = Mock::new(replies);
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        assert!(state(&mut transport, &target(), &metadata()).is_err());
        assert_eq!(mock.requests.borrow().len(), 6);
        assert!(
            mock.requests
                .borrow()
                .iter()
                .all(|request| request.0 == "GET")
        );
    }

    #[test]
    fn refuses_plain_text_and_other_non_html_fields_before_creation() {
        for field_type in ["plainText", "string", "integer"] {
            let mut replies = preflight();
            replies[3]["type"] = json!(field_type);
            let mock = Mock::new(replies);
            let transport_target = transport_target();
            let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
            assert!(state(&mut transport, &target(), &metadata()).is_err());
            let requests = mock.requests.borrow();
            assert_eq!(requests.len(), 4);
            assert!(requests.iter().all(|request| request.0 == "GET"));
        }
    }

    #[test]
    fn refuses_injected_paths_and_forged_assignment_without_requests() {
        let mock = Mock::new(Vec::new());
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let target = target();
        let mut input = metadata();
        input["areaPath"] = json!("Other\\area");
        assert!(state(&mut transport, &target, &input).is_err());
        input["areaPath"] = json!("Prøject\\..\\area");
        assert!(state(&mut transport, &target, &input).is_err());
        let mut saved = json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org",
            "project":"Prøject","areaPath":"Prøject","workItemType":"Bug",
            "descriptionField":"System.AssignedTo","parent":null});
        assert!(
            create(
                &mut transport,
                &target,
                &saved,
                &json!({"title":"T","body":"x"})
            )
            .is_err()
        );
        saved["descriptionField"] = json!(DESCRIPTION);
        assert!(
            read(
                &mut transport,
                &target,
                &saved,
                &json!({"itemId":"1/../../2"})
            )
            .is_err()
        );
        assert!(mock.requests.borrow().is_empty());
    }

    #[test]
    fn rejects_malformed_description_reference_before_authentication() {
        let transport_target = transport_target();
        for field in [
            "System/Description",
            "System..Description",
            "System.Description/",
            "System Description",
            "NoNamespace",
            "System.AssignedTo",
        ] {
            let mock = Mock::new(Vec::new());
            let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
            let mut input = metadata();
            input["descriptionField"] = json!(field);
            assert!(state(&mut transport, &target(), &input).is_err());
            assert!(mock.requests.borrow().is_empty());
        }
        assert!(validate_description_field("Custom.Field_Name2").is_ok());
        assert!(validate_description_field(&format!("Custom.{}", "a".repeat(64))).is_err());
    }

    #[test]
    fn incomplete_creation_evidence_is_ambiguous_and_missing_format_is_not_assumed() {
        let mock = Mock::new(vec![
            json!({"id":12}),
            json!({
                "id":12,"fields":{"System.Title":"T", (DESCRIPTION):"body"}
            }),
        ]);
        let transport_target = transport_target();
        let mut transport = Transport::new(&transport_target, None, &mock, &mock, &NoWait);
        let target = target();
        let state = json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org",
            "project":"Prøject","areaPath":"Prøject","workItemType":"Bug",
            "descriptionField":DESCRIPTION,"parent":null});
        let error = create(
            &mut transport,
            &target,
            &state,
            &json!({"title":"T","body":"body"}),
        )
        .unwrap_err();
        assert_eq!(crate::classification(&error), "ambiguousWrite");
        let item = read(&mut transport, &target, &state, &json!({"itemId":"12"})).unwrap();
        assert_eq!(item["descriptionFormat"], "");
        assert_eq!(mock.requests.borrow().len(), 2);
    }
}
