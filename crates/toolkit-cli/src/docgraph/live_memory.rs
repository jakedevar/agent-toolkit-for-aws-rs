//! S4 memory dispatcher. The canonical item-1 planner/expansion code is shared
//! with replay. Admit the whole initial plan before constructing live clients;
//! admit response-dependent requests again, and stop permanently on failure.
use super::memory::{self, MemoryClient};
use std::time::{Duration, Instant};
use toolkit_aws::sdk::database::DatabaseEndpoint;
use toolkit_core::Json;
mod production;
mod wire;

const OPERATION_BUDGET: Duration = Duration::from_secs(60);
const UNKNOWN: &str =
    "the memory operation failed after submission; unknown outcome; no later step was sent";
const PARTIAL: &str = "an earlier memory step completed; the plan may be partially applied; unknown outcome; no later step was sent";
const INVALID: &str = "unsupported or invalid live memory request; nothing was sent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Analytics,
    Create,
    Dynamo,
    Gremlin,
}
pub(super) trait Backend {
    fn call(&mut self, route: Route, request: &Json, deadline: Instant) -> Result<Json, String>;
}
fn text<'a>(request: &'a Json, key: &str) -> Result<&'a str, String> {
    request
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| INVALID.into())
}
// recorded-shape: a required live request/response object; all non-object shapes are errors.
fn object(request: &Json) -> Result<&[(String, Json)], String> {
    match request {
        Json::Object(fields) => Ok(fields),
        _ => Err(INVALID.into()),
    }
}
fn only(request: &Json, keys: &[&str]) -> Result<(), String> {
    if object(request)?
        .iter()
        .all(|(key, _)| keys.contains(&key.as_str()))
    {
        Ok(())
    } else {
        Err(INVALID.into())
    }
}
fn graph_id(id: &str) -> bool {
    // The live API's exact modeled identifier; the replay builder retains its
    // historical >=10 suffix contract for offline compatibility.
    id.strip_prefix("g-").is_some_and(|s| {
        s.len() == 10
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}
fn admit(request: &Json, policy: Option<&DatabaseEndpoint>) -> Result<Route, String> {
    if request.get("graphIdentifier").is_some() {
        only(
            request,
            &["graphIdentifier", "queryString", "language", "parameters"],
        )?;
        let id = text(request, "graphIdentifier")?;
        if !graph_id(id) || text(request, "language")? != "OPEN_CYPHER" {
            return Err(INVALID.into());
        }
        text(request, "queryString")?;
        if let Some(p) = request.get("parameters") {
            wire::document(p)?;
            object(p)?;
        }
        let p = policy.ok_or(INVALID)?;
        if p.port() != 443 || p.host() != format!("{id}.{}.neptune-graph.amazonaws.com", p.region())
        {
            return Err(INVALID.into());
        }
        Ok(Route::Analytics)
    } else if request.get("graphName").is_some() {
        only(
            request,
            &[
                "graphName",
                "provisionedMemory",
                "publicConnectivity",
                "vectorSearchConfiguration",
                "deletionProtection",
                "tags",
                "replicaCount",
            ],
        )?;
        wire::validate_creation(request)?;
        Ok(Route::Create)
    } else if request.get("gremlin").is_some() {
        only(request, &["gremlin", "bindings"])?;
        text(request, "gremlin")?;
        object(request.get("bindings").ok_or(INVALID)?)?;
        let p = policy.ok_or(INVALID)?;
        if p.port() != 8182
            || !p
                .host()
                .ends_with(&format!(".{}.neptune.amazonaws.com", p.region()))
        {
            return Err(INVALID.into());
        }
        Ok(Route::Gremlin)
    } else if request.get("operation").is_some() {
        wire::validate_dynamo(request)?;
        Ok(Route::Dynamo)
    } else {
        Err(INVALID.into())
    }
}
fn preflight(requests: &[Json], policy: Option<&DatabaseEndpoint>) -> Result<(), String> {
    if requests.is_empty() {
        return Err(INVALID.into());
    }
    let first = admit(&requests[0], policy)?;
    if policy.is_some() && matches!(first, Route::Create | Route::Dynamo) {
        return Err(INVALID.into());
    }
    for request in requests {
        if admit(request, policy)? != first {
            return Err(INVALID.into());
        }
    }
    Ok(())
}
struct Dispatcher<'a> {
    backend: &'a mut dyn Backend,
    policy: Option<&'a DatabaseEndpoint>,
    deadline: Instant,
    failed: bool,
    completed: bool,
}
impl MemoryClient for Dispatcher<'_> {
    fn submit(&mut self, request: &Json) -> Result<Json, String> {
        if self.failed {
            return Err(if self.completed { PARTIAL } else { UNKNOWN }.into());
        }
        let result = (|| {
            if Instant::now() >= self.deadline {
                return Err(
                    "the memory invocation deadline elapsed; no later step was sent".into(),
                );
            }
            let route = admit(request, self.policy)?;
            let deadline = (Instant::now() + OPERATION_BUDGET).min(self.deadline);
            self.backend.call(route, request, deadline)
        })();
        match result {
            Ok(value) => {
                self.completed = true;
                Ok(value)
            }
            Err(error) => {
                self.failed = true;
                // A pre-send failure applies only to the current request.
                // Earlier successful steps cannot be rolled back or retried.
                Err(if self.completed {
                    PARTIAL.into()
                } else {
                    error
                })
            }
        }
    }
}
pub(super) fn execute(
    backend: &mut dyn Backend,
    op: &str,
    input: &Json,
    requests: &[Json],
    policy: Option<&DatabaseEndpoint>,
    deadline: Instant,
) -> Result<Json, String> {
    preflight(requests, policy)?;
    let mut client = Dispatcher {
        backend,
        policy,
        deadline,
        failed: false,
        completed: false,
    };
    if op == "retrieve" {
        memory::retrieve(&mut client, text(input, "graph_id")?, &requests[0])
    } else {
        memory::execute(&mut client, op, requests)
    }
}
pub(super) use production::run;
#[cfg(test)]
mod tests;
