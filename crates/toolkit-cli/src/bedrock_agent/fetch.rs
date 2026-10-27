//! The discovery of `fetch_bedrock_agent.py`: every component the migration
//! cares about, gathered into one manifest.
//!
//! Each function below is the Rust counterpart of the Python function of the
//! same name. The error rules are the script's:
//!
//! - a *tolerated* service error (`AccessDeniedException`,
//!   `ResourceNotFoundException`, `ValidationException`) on a detail call is
//!   recorded in place as `{"_error": <code>, "_message": <text>}`;
//! - any other service error on a detail call, and any error while paging a
//!   list of action groups or knowledge bases, ends the run;
//! - a service error while listing aliases, versions, collaborators or the data
//!   sources of one knowledge base is recorded as `{"_error": <text>}` after the
//!   entries already collected;
//! - a transport failure (`Connect`, `NoCredentials`, a pagination or response
//!   shape problem) is never recorded: it ends the run.
//!
//! `<text>` is the `str(ClientError)` of botocore, `An error occurred (<code>)
//! when calling the <Operation> operation: <message>`, with the message already
//! redacted and flattened to one line by `toolkit_aws::ServiceError`.

use toolkit_aws::recorded::{optional_list, optional_text, MemberError};
use toolkit_aws::{collect_pages, next_token_from, AwsError, Page, PageLimits, ServiceError};
use toolkit_core::Json;
use toolkit_msk::pyfmt::py_str;

use super::client::{AgentClient, Service};

/// Service error codes recorded per call instead of ending the run.
const TOLERATED_ERROR_CODES: [&str; 3] = [
    "AccessDeniedException",
    "ResourceNotFoundException",
    "ValidationException",
];

/// The alias id of the built-in test alias, which never signals production.
const TEST_ALIAS_ID: &str = "TSTALIASID";

/// The inputs of one discovery.
pub(crate) struct Params {
    pub(crate) agent_id: String,
    pub(crate) agent_version: String,
    pub(crate) agent_alias_id: Option<String>,
    pub(crate) region: String,
    pub(crate) inline_s3_schemas: bool,
}

/// Why a discovery produced no manifest.
#[derive(Debug)]
pub(crate) enum Failure {
    /// `get_agent` failed with a service error: `ERROR: failed to fetch agent`.
    Agent(String),
    /// Any other error that ends the run (exit status 1).
    Fatal(AwsError),
}

impl From<AwsError> for Failure {
    fn from(error: AwsError) -> Self {
        Self::Fatal(error)
    }
}

fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::object(pairs)
}

/// `value` with `ResponseMetadata` removed at every depth.
pub(crate) fn strip_response_metadata(value: &Json) -> Json {
    match value {
        Json::Object(members) => Json::Object(
            members
                .iter()
                .filter(|(key, _)| key != "ResponseMetadata")
                .map(|(key, member)| (key.clone(), strip_response_metadata(member)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(strip_response_metadata).collect()),
        other => other.clone(),
    }
}

/// `strip_response_metadata(detail).get(key, detail)`.
fn unwrap_member(detail: &Json, key: &str) -> Json {
    strip_response_metadata(detail)
        .get(key)
        .cloned()
        .unwrap_or_else(|| detail.clone())
}

/// `{**object, key: value}`: an existing key keeps its position.
fn with_member(object: &Json, key: &str, value: Json) -> Json {
    let Json::Object(members) = object else {
        return object.clone();
    };
    let mut members = members.clone();
    match members.iter_mut().find(|(name, _)| name == key) {
        Some(slot) => slot.1 = value,
        None => members.push((key.to_string(), value)),
    }
    Json::Object(members)
}

/// Python truthiness of a JSON value.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Int(number) => *number != 0,
        Json::Float(number) => *number != 0.0,
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(members) => !members.is_empty(),
    }
}

/// `str(botocore.exceptions.ClientError)`.
fn client_error_text(operation: &str, error: &ServiceError) -> String {
    format!(
        "An error occurred ({}) when calling the {operation} operation: {}",
        error.code().unwrap_or("Unknown"),
        error.message()
    )
}

/// A wrong-typed optional member of a response (read through the shared
/// `toolkit_aws::recorded` helpers: absent and `[]` are empty, `null` and any
/// other type are refused) as an `aws_invalid_response`.
fn invalid_member(error: &MemberError) -> AwsError {
    AwsError::invalid_response(format!(
        "the member `{}` of a response is not {} (it is {})",
        error.key(),
        error.expected(),
        error.found()
    ))
}

/// The string member `key` of a summary (`summary["actionGroupId"]`).
fn summary_id(summary: &Json, key: &str) -> Result<String, AwsError> {
    summary
        .get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            AwsError::invalid_response(format!("a summary in a response has no string `{key}`"))
        })
}

type Handle<'h> = &'h mut dyn FnMut(&mut Fetcher<'_>, &Json) -> Result<(), AwsError>;

/// The calls of one discovery, over an [`AgentClient`].
pub(crate) struct Fetcher<'a> {
    client: &'a mut dyn AgentClient,
    /// `str(ClientError)` of the latest service error, for the catch sites.
    last_client_error: Option<String>,
}

impl<'a> Fetcher<'a> {
    pub(crate) fn new(client: &'a mut dyn AgentClient) -> Self {
        Self {
            client,
            last_client_error: None,
        }
    }

    fn invoke(
        &mut self,
        service: Service,
        operation: &str,
        request: Vec<(&str, Json)>,
    ) -> Result<Json, AwsError> {
        match self.client.call(service, operation, &object(request)) {
            Err(AwsError::Service(error)) => {
                self.last_client_error = Some(client_error_text(operation, &error));
                Err(AwsError::Service(error))
            }
            other => other,
        }
    }

    /// `except ClientError as e: str(e)`: the text of a service error, or the
    /// error itself when it is not one.
    fn client_failure(&mut self, error: AwsError) -> Result<String, AwsError> {
        match error {
            AwsError::Service(_) => Ok(self.last_client_error.take().unwrap_or_default()),
            other => Err(other),
        }
    }

    /// `_safe(call, ...)`.
    fn safe(
        &mut self,
        service: Service,
        operation: &str,
        request: Vec<(&str, Json)>,
    ) -> Result<Json, AwsError> {
        match self.invoke(service, operation, request) {
            Err(AwsError::Service(error))
                if error
                    .code()
                    .is_some_and(|code| TOLERATED_ERROR_CODES.contains(&code)) =>
            {
                Ok(object(vec![
                    ("_error", Json::string(error.code().unwrap_or_default())),
                    (
                        "_message",
                        Json::string(self.last_client_error.take().unwrap_or_default()),
                    ),
                ]))
            }
            other => other,
        }
    }

    /// `for page in paginator.paginate(...)`: calls `handle` for every summary
    /// of every page as soon as its page arrives, so what `handle` collected
    /// before a failing page stays collected.
    fn walk(
        &mut self,
        service: Service,
        operation: &str,
        base: &[(&str, Json)],
        list_key: &str,
        handle: Handle<'_>,
    ) -> Result<(), AwsError> {
        collect_pages(&PageLimits::default(), |token| {
            let mut request = base.to_vec();
            if let Some(token) = token {
                request.push(("nextToken", Json::string(token)));
            }
            let response = self.invoke(service, operation, request)?;
            let summaries =
                optional_list(&response, list_key).map_err(|error| invalid_member(&error))?;
            for summary in summaries {
                handle(self, summary)?;
            }
            Ok(Page {
                items: summaries.to_vec(),
                next_token: next_token_from(&response, "nextToken")?,
            })
        })
        .map(|_| ())
    }

    /// `_maybe_inline_s3_schema`.
    fn maybe_inline_s3_schema(&mut self, api_schema: &Json) -> Result<Json, AwsError> {
        // recorded-shape: the #4383 port of `api_schema["s3"]`: an `s3` that is
        // not an object (`null` included) leaves the schema as it is.
        let Some(s3_ref @ Json::Object(_)) = api_schema.get("s3") else {
            return Ok(api_schema.clone());
        };
        let bucket = s3_ref
            .get("s3BucketName")
            .and_then(Json::as_str)
            .filter(|name| !name.is_empty());
        let key = s3_ref
            .get("s3ObjectKey")
            .and_then(Json::as_str)
            .filter(|name| !name.is_empty());
        let (Some(bucket), Some(key)) = (bucket, key) else {
            return Ok(api_schema.clone());
        };
        let request = vec![("Bucket", Json::string(bucket)), ("Key", Json::string(key))];
        match self.invoke(Service::S3, "GetObject", request) {
            Ok(object_response) => {
                let body = object_response
                    .get("Body")
                    .and_then(Json::as_str)
                    .ok_or_else(|| {
                        AwsError::invalid_response("an S3 GetObject response has no text `Body`")
                    })?;
                let inlined = with_member(api_schema, "_inlinedPayload", Json::string(body));
                Ok(with_member(
                    &inlined,
                    "_inlinedSource",
                    Json::string(format!("s3://{bucket}/{key}")),
                ))
            }
            Err(error) => {
                let text = self.client_failure(error)?;
                Ok(with_member(api_schema, "_inlineError", Json::string(text)))
            }
        }
    }

    /// `fetch_action_groups`.
    fn action_groups(
        &mut self,
        agent_id: &str,
        version: &str,
        inline_s3: bool,
    ) -> Result<Vec<Json>, AwsError> {
        let mut groups: Vec<Json> = Vec::new();
        let base = [
            ("agentId", Json::string(agent_id)),
            ("agentVersion", Json::string(version)),
        ];
        self.walk(
            Service::BedrockAgent,
            "ListAgentActionGroups",
            &base,
            "actionGroupSummaries",
            &mut |fetcher, summary| {
                let detail = fetcher.safe(
                    Service::BedrockAgent,
                    "GetAgentActionGroup",
                    vec![
                        ("agentId", Json::string(agent_id)),
                        ("agentVersion", Json::string(version)),
                        (
                            "actionGroupId",
                            Json::string(summary_id(summary, "actionGroupId")?),
                        ),
                    ],
                )?;
                let mut group = unwrap_member(&detail, "agentActionGroup");
                if inline_s3 {
                    if let Some(schema) = group.get("apiSchema").filter(|schema| truthy(schema)) {
                        let inlined = fetcher.maybe_inline_s3_schema(&schema.clone())?;
                        group = with_member(&group, "apiSchema", inlined);
                    }
                }
                groups.push(group);
                Ok(())
            },
        )?;
        Ok(groups)
    }

    /// `fetch_knowledge_bases`.
    fn knowledge_bases(&mut self, agent_id: &str, version: &str) -> Result<Vec<Json>, AwsError> {
        let mut out: Vec<Json> = Vec::new();
        let base = [
            ("agentId", Json::string(agent_id)),
            ("agentVersion", Json::string(version)),
        ];
        self.walk(
            Service::BedrockAgent,
            "ListAgentKnowledgeBases",
            &base,
            "agentKnowledgeBaseSummaries",
            &mut |fetcher, summary| {
                let kb_id = summary_id(summary, "knowledgeBaseId")?;
                let association = fetcher.safe(
                    Service::BedrockAgent,
                    "GetAgentKnowledgeBase",
                    vec![
                        ("agentId", Json::string(agent_id)),
                        ("agentVersion", Json::string(version)),
                        ("knowledgeBaseId", Json::string(&kb_id)),
                    ],
                )?;
                let kb_detail = fetcher.safe(
                    Service::BedrockAgent,
                    "GetKnowledgeBase",
                    vec![("knowledgeBaseId", Json::string(&kb_id))],
                )?;
                let data_sources = fetcher.data_sources(&kb_id)?;
                out.push(object(vec![
                    (
                        "association",
                        unwrap_member(&association, "agentKnowledgeBase"),
                    ),
                    ("knowledgeBase", unwrap_member(&kb_detail, "knowledgeBase")),
                    ("dataSources", Json::Array(data_sources)),
                ]));
                Ok(())
            },
        )?;
        Ok(out)
    }

    /// The data-source loop of `fetch_knowledge_bases`: a service error ends
    /// the listing and is recorded after the sources already collected.
    fn data_sources(&mut self, kb_id: &str) -> Result<Vec<Json>, AwsError> {
        let mut sources: Vec<Json> = Vec::new();
        let base = [("knowledgeBaseId", Json::string(kb_id))];
        let walked = self.walk(
            Service::BedrockAgent,
            "ListDataSources",
            &base,
            "dataSourceSummaries",
            &mut |fetcher, summary| {
                let detail = fetcher.safe(
                    Service::BedrockAgent,
                    "GetDataSource",
                    vec![
                        ("knowledgeBaseId", Json::string(kb_id)),
                        (
                            "dataSourceId",
                            Json::string(summary_id(summary, "dataSourceId")?),
                        ),
                    ],
                )?;
                sources.push(strip_response_metadata(&detail));
                Ok(())
            },
        );
        if let Err(error) = walked {
            let text = self.client_failure(error)?;
            sources.push(object(vec![("_error", Json::string(text))]));
        }
        Ok(sources)
    }

    /// `fetch_aliases_and_versions`.
    fn aliases_and_versions(&mut self, agent_id: &str) -> Result<Json, AwsError> {
        let mut aliases: Vec<Json> = Vec::new();
        let mut versions: Vec<Json> = Vec::new();
        let base = [("agentId", Json::string(agent_id))];
        let walked = self.walk(
            Service::BedrockAgent,
            "ListAgentAliases",
            &base,
            "agentAliasSummaries",
            &mut |fetcher, summary| {
                let detail = fetcher.safe(
                    Service::BedrockAgent,
                    "GetAgentAlias",
                    vec![
                        ("agentId", Json::string(agent_id)),
                        (
                            "agentAliasId",
                            Json::string(summary_id(summary, "agentAliasId")?),
                        ),
                    ],
                )?;
                aliases.push(unwrap_member(&detail, "agentAlias"));
                Ok(())
            },
        );
        if let Err(error) = walked {
            let text = self.client_failure(error)?;
            aliases.push(object(vec![("_error", Json::string(text))]));
        }
        let walked = self.walk(
            Service::BedrockAgent,
            "ListAgentVersions",
            &base,
            "agentVersionSummaries",
            &mut |_, summary| {
                versions.push(summary.clone());
                Ok(())
            },
        );
        if let Err(error) = walked {
            let text = self.client_failure(error)?;
            versions.push(object(vec![("_error", Json::string(text))]));
        }
        Ok(object(vec![
            ("aliases", Json::Array(aliases)),
            ("versions", Json::Array(versions)),
        ]))
    }

    /// `fetch_collaborators`.
    fn collaborators(&mut self, agent_id: &str, version: &str) -> Result<Vec<Json>, AwsError> {
        let mut out: Vec<Json> = Vec::new();
        if !self
            .client
            .supports(Service::BedrockAgent, "ListAgentCollaborators")
        {
            return Ok(out);
        }
        let base = [
            ("agentId", Json::string(agent_id)),
            ("agentVersion", Json::string(version)),
        ];
        let walked = self.walk(
            Service::BedrockAgent,
            "ListAgentCollaborators",
            &base,
            "agentCollaboratorSummaries",
            &mut |fetcher, summary| {
                let detail = fetcher.safe(
                    Service::BedrockAgent,
                    "GetAgentCollaborator",
                    vec![
                        ("agentId", Json::string(agent_id)),
                        ("agentVersion", Json::string(version)),
                        (
                            "collaboratorId",
                            Json::string(summary_id(summary, "collaboratorId")?),
                        ),
                    ],
                )?;
                out.push(unwrap_member(&detail, "agentCollaborator"));
                Ok(())
            },
        );
        if let Err(error) = walked {
            let text = self.client_failure(error)?;
            out.push(object(vec![("_error", Json::string(text))]));
        }
        Ok(out)
    }

    /// `fetch_iam_role`.
    fn iam_role(&mut self, role_arn: Option<&str>) -> Result<Json, AwsError> {
        let Some(role_arn) = role_arn.filter(|arn| !arn.is_empty()) else {
            return Ok(Json::Object(Vec::new()));
        };
        let role_name = role_arn.rsplit('/').next().unwrap_or(role_arn);
        let role_request = || vec![("RoleName", Json::string(role_name))];
        let role = self.safe(Service::Iam, "GetRole", role_request())?;
        let attached = self.safe(Service::Iam, "ListAttachedRolePolicies", role_request())?;
        let inline_names = self.safe(Service::Iam, "ListRolePolicies", role_request())?;
        let mut inline_policies: Vec<Json> = Vec::new();
        if inline_names.get("_error").is_none() {
            for name in optional_list(&inline_names, "PolicyNames")
                .map_err(|error| invalid_member(&error))?
            {
                let Some(name) = name.as_str() else {
                    return Err(AwsError::invalid_response(
                        "a policy name in a response is not a string",
                    ));
                };
                let document = self.safe(
                    Service::Iam,
                    "GetRolePolicy",
                    vec![
                        ("RoleName", Json::string(role_name)),
                        ("PolicyName", Json::string(name)),
                    ],
                )?;
                inline_policies.push(strip_response_metadata(&document));
            }
        }
        Ok(object(vec![
            ("role", unwrap_member(&role, "Role")),
            (
                "attachedPolicies",
                unwrap_member(&attached, "AttachedPolicies"),
            ),
            ("inlinePolicies", Json::Array(inline_policies)),
        ]))
    }
}

/// `tag` of the DRAFT warning for the aliases that are not the test alias.
fn production_alias_tag(aliases: &[Json]) -> Option<String> {
    let production: Vec<&Json> = aliases
        .iter()
        .filter(|alias| {
            matches!(alias, Json::Object(_))
                && alias.get("agentAliasId").is_some_and(|id| {
                    !matches!(id, Json::Null) && id.as_str() != Some(TEST_ALIAS_ID)
                })
        })
        .collect();
    if production.is_empty() {
        return None;
    }
    let tag = production
        .iter()
        .map(|alias| {
            let name = alias
                .get("agentAliasName")
                .map_or_else(|| "?".to_string(), py_str);
            let version = match alias.get("routingConfiguration") {
                Some(routing) if truthy(routing) => match routing {
                    Json::Array(entries) => entries
                        .first()
                        .and_then(|entry| entry.get("agentVersion"))
                        .map_or_else(|| "?".to_string(), py_str),
                    _ => "?".to_string(),
                },
                _ => "?".to_string(),
            };
            format!("{name}->v{version}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(tag)
}

/// `main()` up to the written file: the manifest of the agent.
///
/// # Errors
///
/// [`Failure::Agent`] when `get_agent` returns a service error;
/// [`Failure::Fatal`] for any other error that ends the run.
pub(crate) fn discover(client: &mut dyn AgentClient, params: &Params) -> Result<Json, Failure> {
    let mut fetcher = Fetcher::new(client);
    let identity = fetcher.invoke(Service::Sts, "GetCallerIdentity", Vec::new())?;
    let agent = match fetcher.invoke(
        Service::BedrockAgent,
        "GetAgent",
        vec![("agentId", Json::string(&params.agent_id))],
    ) {
        Ok(agent) => agent,
        Err(error @ AwsError::Service(_)) => {
            return Err(Failure::Agent(fetcher.client_failure(error)?));
        }
        Err(error) => return Err(Failure::Fatal(error)),
    };
    let agent_doc = unwrap_member(&agent, "agent");
    if !matches!(agent_doc, Json::Object(_)) {
        return Err(AwsError::invalid_response("the GetAgent response is not an object").into());
    }
    let role_arn = optional_text(&agent_doc, "agentResourceRoleArn")
        .map_err(|error| invalid_member(&error))?
        .map(str::to_string);
    let role_info = fetcher.iam_role(role_arn.as_deref())?;
    let aliases_and_versions = fetcher.aliases_and_versions(&params.agent_id)?;

    let defaulted = |key: &str, default: &str| -> Json {
        agent_doc
            .get(key)
            .filter(|value| truthy(value))
            .cloned()
            .unwrap_or_else(|| Json::string(default))
    };
    let alias_id = params
        .agent_alias_id
        .as_deref()
        .map_or(Json::Null, Json::string);
    let identity_member = |key: &str| identity.get(key).cloned().unwrap_or(Json::Null);
    let mut warnings: Vec<Json> = Vec::new();
    if params.agent_version == "DRAFT" {
        // recorded-shape: our own `aliasesAndVersions` object, not a response.
        if let Some(Json::Array(aliases)) = aliases_and_versions.get("aliases") {
            if let Some(tag) = production_alias_tag(aliases) {
                warnings.push(Json::string(format!(
                    "Fetched DRAFT but non-DRAFT aliases exist ({tag}). DRAFT may diverge from production."
                )));
            }
        }
    }
    let manifest_agent_collaboration = defaulted("agentCollaboration", "DISABLED");
    let manifest_orchestration = defaulted("orchestrationType", "DEFAULT");
    let action_groups = fetcher.action_groups(
        &params.agent_id,
        &params.agent_version,
        params.inline_s3_schemas,
    )?;
    let knowledge_bases = fetcher.knowledge_bases(&params.agent_id, &params.agent_version)?;
    let collaborators = fetcher.collaborators(&params.agent_id, &params.agent_version)?;
    Ok(object(vec![
        (
            "discovery",
            object(vec![
                ("account", identity_member("Account")),
                ("region", Json::string(&params.region)),
                ("callerArn", identity_member("Arn")),
                ("fetchedAgentVersion", Json::string(&params.agent_version)),
                ("fetchedAgentAliasId", alias_id),
                ("warnings", Json::Array(warnings)),
            ]),
        ),
        ("agent", agent_doc.clone()),
        ("agentCollaborationMode", manifest_agent_collaboration),
        ("orchestrationType", manifest_orchestration),
        ("executionRole", role_info),
        ("actionGroups", Json::Array(action_groups)),
        ("knowledgeBases", Json::Array(knowledge_bases)),
        ("collaborators", Json::Array(collaborators)),
        ("aliasesAndVersions", aliases_and_versions),
    ]))
}
