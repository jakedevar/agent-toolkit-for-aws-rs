//! The provider-plane recorder (#4413 S3): an offline [`HttpClient`] for the
//! credential providers, injected through the R-PROV seam
//! ([`super::source::ProviderHttp::injected`]).
//!
//! It answers `"METHOD host/path"` keys from scripted replies, refuses every
//! other request with a dispatch failure, and records method, host and path
//! only (never a header or a body). It opens no socket. It is a public test
//! double, like [`crate::testing`], so the proxy's end-to-end credential
//! tests (T-ABS, T-PROV, T-LEAK-SDK) can run the real aws-config providers in
//! another crate; production code never builds one.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse as SdkHttpResponse;
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;

use super::source::ProviderHttp;

/// One scripted reply.
#[derive(Debug, Clone)]
pub enum RecordedReply {
    /// A response.
    Ok {
        /// Status code.
        status: u16,
        /// Response headers.
        headers: Vec<(String, String)>,
        /// Response body.
        body: String,
    },
    /// A dispatch (I/O) failure.
    Dispatch,
    /// A timeout connector error.
    Timeout,
}

impl RecordedReply {
    /// A `status` response with `body` and no headers.
    #[must_use]
    pub fn ok(status: u16, body: &str) -> Self {
        Self::Ok {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    /// Add a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let Self::Ok { headers, .. } = &mut self {
            headers.push((name.to_string(), value.to_string()));
        }
        self
    }
}

type Routes = Arc<Mutex<Vec<(String, VecDeque<RecordedReply>)>>>;

/// The recording provider-plane client.
#[derive(Clone, Default)]
pub struct ProviderRecorder {
    seen: Arc<Mutex<Vec<String>>>,
    routes: Routes,
}

impl fmt::Debug for ProviderRecorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderRecorder")
    }
}

impl ProviderRecorder {
    /// A recorder that refuses everything.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `"METHOD host/path"` with `replies` in turn (the last repeats).
    #[must_use]
    pub fn route(self, key: &str, replies: Vec<RecordedReply>) -> Self {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((key.to_string(), replies.into()));
        self
    }

    /// Every request seen, as `"METHOD host/path"`, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// This recorder as the provider-plane client.
    #[must_use]
    pub fn provider_http(&self) -> ProviderHttp {
        ProviderHttp::injected(self.clone())
    }

    fn reply(&self, key: &str) -> Option<RecordedReply> {
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        let (_, replies) = routes.iter_mut().find(|(route, _)| route == key)?;
        if replies.len() > 1 {
            replies.pop_front()
        } else {
            replies.front().cloned()
        }
    }
}

/// `"METHOD host/path"` of a request URI (`scheme://host[:port]/path?query`).
fn key_of(method: &str, uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    let path = path.split('?').next().unwrap_or("/");
    format!("{method} {host}{path}")
}

impl HttpConnector for ProviderRecorder {
    fn call(
        &self,
        request: aws_smithy_runtime_api::client::orchestrator::HttpRequest,
    ) -> HttpConnectorFuture {
        let key = key_of(request.method(), request.uri());
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(key.clone());
        let result = match self.reply(&key) {
            Some(RecordedReply::Ok {
                status,
                headers,
                body,
            }) => match StatusCode::try_from(status) {
                Ok(code) => {
                    let mut response = SdkHttpResponse::new(code, SdkBody::from(body));
                    for (name, value) in headers {
                        response.headers_mut().insert(name, value);
                    }
                    Ok(response)
                }
                Err(_) => Err(ConnectorError::other("recorder status".into(), None)),
            },
            Some(RecordedReply::Timeout) => Err(ConnectorError::timeout("recorder timeout".into())),
            Some(RecordedReply::Dispatch) | None => {
                Err(ConnectorError::io("recorder refused the request".into()))
            }
        };
        HttpConnectorFuture::ready(result)
    }
}

impl HttpClient for ProviderRecorder {
    fn http_connector(
        &self,
        _settings: &HttpConnectorSettings,
        _components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_the_port_and_query() {
        assert_eq!(
            key_of("GET", "http://169.254.170.2:80/creds?x=1"),
            "GET 169.254.170.2/creds"
        );
        assert_eq!(
            key_of("PUT", "http://169.254.169.254"),
            "PUT 169.254.169.254/"
        );
    }
}
