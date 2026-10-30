//! Memory live dispatch (S4). All tests inject backends; no test constructs live clients.
use std::time::{Duration, Instant};
use toolkit_aws::sdk::database::DatabaseEndpoint;
use toolkit_core::Json;
use super::memory;
mod wire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route { Analytics, Create, Dynamo, Gremlin }

pub(super) trait Backend {
    fn call(&mut self, route: Route, request: &Json, deadline: Instant) -> Result<Json, String>;
}

pub(super) fn execute(
    _backend: &mut dyn Backend, _op: &str, _input: &Json,
    _requests: &[Json], _policy: Option<&DatabaseEndpoint>, _deadline: Instant,
) -> Result<Json, String> {
    Err("memory live dispatch is not implemented".into())
}

#[cfg(test)]
mod tests;
