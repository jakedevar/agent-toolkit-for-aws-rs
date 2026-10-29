//! R-URL: the MCP proxy's one request target (#4413 S3, design 4.4.1).
//!
//! The endpoint is the single positional argument of `toolkit mcp-proxy`. It
//! is checked by [`validate_endpoint`] under [`EndpointPolicy::aws_public`]
//! (https only, no userinfo, query or fragment, an AWS host, no `..` path
//! segment), but the request URL is **not** rebuilt from the returned
//! [`crate::Endpoint`], whose path drops trailing slashes. [`McpTarget`] keeps:
//!
//! - `authority`: the lowercase host, plus `:port` for a port other than 443
//!   (an explicit `:443` is dropped);
//! - `path`: the argument's path **verbatim** (percent-encoding neither decoded
//!   nor re-encoded), or `/` when it is empty.
//!
//! Every `POST`, `GET` and `DELETE` goes to `https://` + authority + path. No
//! URL is ever taken from stdin, a response, an SSE field or a header. A
//! refusal is [`TargetError`], whose text is fixed and never echoes the
//! argument (a secret in a query is never printed).

use crate::endpoint::{public_addresses, validate_endpoint, EndpointPolicy, HostResolver};

/// Why an endpoint argument was refused. The display text is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetError {
    /// The argument is not an allowed https AWS URL (scheme, userinfo, query,
    /// fragment, host, port or path refused).
    NotAllowed,
    /// The host does not resolve, or resolves to a non-public address.
    NotPublic,
}

impl TargetError {
    /// The fixed message (no part of the argument).
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAllowed => {
                "the endpoint must be an https URL of an AWS host (*.api.aws or *.amazonaws.com) \
                 with no userinfo, query or fragment"
            }
            Self::NotPublic => "the endpoint host does not resolve to public addresses only",
        }
    }
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for TargetError {}

/// The validated MCP endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTarget {
    host: String,
    authority: String,
    path: String,
}

impl McpTarget {
    /// Check `endpoint` against the R-URL grammar (no DNS lookup).
    ///
    /// # Errors
    /// [`TargetError::NotAllowed`] for anything [`validate_endpoint`] refuses
    /// under the AWS public policy.
    pub fn parse(endpoint: &str) -> Result<Self, TargetError> {
        let policy = EndpointPolicy::aws_public().without_dns_check();
        let checked = validate_endpoint(endpoint, "endpoint", &policy, &NoLookup)
            .map_err(|_| TargetError::NotAllowed)?;
        // `validate_endpoint` accepted the URL, so it is `https://` + an
        // authority without userinfo + a path without query or fragment.
        let rest = endpoint
            .split_once("://")
            .map(|(_, rest)| rest)
            .ok_or(TargetError::NotAllowed)?;
        let path_start = rest.find('/').unwrap_or(rest.len());
        let path = checked.path();
        let _ = (rest, path_start);
        let host = checked.host().to_string();
        let authority = match checked.port() {
            None | Some(443) => host.clone(),
            Some(port) => format!("{host}:{port}"),
        };
        Ok(Self {
            host,
            authority,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
        })
    }

    /// Require every DNS answer for the host to be public (the start-up
    /// check; the transport applies the same guard again at connect time).
    ///
    /// # Errors
    /// [`TargetError::NotPublic`].
    pub fn check_public(&self, resolver: &dyn HostResolver) -> Result<(), TargetError> {
        public_addresses(&self.host, resolver)
            .map(|_| ())
            .map_err(|_| TargetError::NotPublic)
    }

    /// The lowercase host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The host, with `:port` unless the port is 443.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The path, verbatim (`/` when the argument had none).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The request URL of every MCP request.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://{}{}", self.authority, self.path)
    }
}

/// A resolver that is never asked (the grammar check does no lookup).
struct NoLookup;

impl HostResolver for NoLookup {
    fn resolve(&self, _host: &str) -> Result<Vec<std::net::IpAddr>, String> {
        Err("no lookup".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FixedResolver;

    fn target(url: &str) -> McpTarget {
        McpTarget::parse(url).unwrap_or_else(|_| panic!("accepted: {url}"))
    }

    #[test]
    fn the_path_is_kept_verbatim_and_the_authority_drops_443() {
        let cases = [
            (
                "https://aws-mcp.us-east-1.api.aws/mcp",
                "aws-mcp.us-east-1.api.aws",
                "/mcp",
            ),
            (
                "https://aws-mcp.us-east-1.api.aws/mcp/",
                "aws-mcp.us-east-1.api.aws",
                "/mcp/",
            ),
            (
                "https://aws-mcp.us-east-1.api.aws",
                "aws-mcp.us-east-1.api.aws",
                "/",
            ),
            (
                "https://aws-mcp.us-east-1.api.aws/",
                "aws-mcp.us-east-1.api.aws",
                "/",
            ),
            (
                "https://aws-mcp.us-east-1.api.aws/a%2Fb/mcp",
                "aws-mcp.us-east-1.api.aws",
                "/a%2Fb/mcp",
            ),
            (
                "https://AWS-MCP.us-east-1.api.aws:443/mcp",
                "aws-mcp.us-east-1.api.aws",
                "/mcp",
            ),
            (
                "https://aws-mcp.us-east-1.api.aws:8443/mcp",
                "aws-mcp.us-east-1.api.aws:8443",
                "/mcp",
            ),
        ];
        for (url, authority, path) in cases {
            let t = target(url);
            assert_eq!((t.authority(), t.path()), (authority, path), "{url}");
            assert_eq!(t.url(), format!("https://{authority}{path}"));
        }
    }

    #[test]
    fn the_documented_host_shapes_are_accepted() {
        for url in [
            "https://aws-mcp.us-east-1.api.aws/mcp",
            "https://connect.aidevops.us-west-2.api.aws/mcp",
            "https://agentaccess-mcp.us-east-1.api.aws/mcp",
            "https://bedrock-agentcore.us-east-1.amazonaws.com/runtimes/x/invocations",
        ] {
            target(url);
        }
    }

    #[test]
    fn refused_endpoints_never_echo_the_argument() {
        for url in [
            "http://aws-mcp.us-east-1.api.aws/mcp",
            "https://localhost/mcp",
            "https://127.0.0.1/mcp",
            "https://[::1]/mcp",
            "https://example.com/mcp",
            "https://aws-mcp.cn-north-1.amazonaws.com.cn/mcp",
            "https://user:pw@aws-mcp.us-east-1.api.aws/mcp",
            "https://aws-mcp.us-east-1.api.aws/mcp?x=1",
            "https://aws-mcp.us-east-1.api.aws/mcp?",
            "https://aws-mcp.us-east-1.api.aws/mcp#f",
            "https://aws-mcp.us-east-1.api.aws/mcp?token=CANARY",
            "https://aws-mcp.us-east-1.api.aws/../mcp",
            "aws-mcp.us-east-1.api.aws/mcp",
        ] {
            let error = McpTarget::parse(url).expect_err(url);
            assert_eq!(error, TargetError::NotAllowed, "{url}");
            assert!(!error.to_string().contains("CANARY"));
            assert!(!error.to_string().contains("example"));
        }
    }

    #[test]
    fn the_dns_check_refuses_private_answers() {
        let t = target("https://aws-mcp.us-east-1.api.aws/mcp");
        let public = FixedResolver::new().answer("aws-mcp.us-east-1.api.aws", &["52.94.0.1"]);
        assert_eq!(t.check_public(&public), Ok(()));
        let private = FixedResolver::new().answer("aws-mcp.us-east-1.api.aws", &["10.0.0.1"]);
        assert_eq!(t.check_public(&private), Err(TargetError::NotPublic));
        assert_eq!(
            t.check_public(&FixedResolver::new()),
            Err(TargetError::NotPublic)
        );
    }
}
