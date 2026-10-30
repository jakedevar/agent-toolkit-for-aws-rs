//! Modeled HTTP request encoding and bounded single-attempt response handling.
use super::*;
use toolkit_aws::transport::{HttpRequest, Signer};
use toolkit_aws::stream::StreamingTransport;

pub(super) fn encode(_route: Route, _request: &Json, _region: &str, _policy: Option<&DatabaseEndpoint>, _remaining: Duration) -> Result<HttpRequest,String> { Err("not implemented".into()) }
pub(super) fn exchange(_transport: &dyn StreamingTransport, _signer: &dyn Signer, _route: Route, _wire: HttpRequest, _region: &str, _deadline: Instant) -> Result<Json,String> { Err("not implemented".into()) }
pub(super) fn normalize(_route: Route, _bytes: &[u8]) -> Result<Json,String> { Err("not implemented".into()) }
#[cfg(test)]
mod tests;
