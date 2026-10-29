//! The live review client (S2, #5489; R-DG-04, T-DG-04a..e) over injected
//! transports. Nothing here opens a socket, resolves a name or reads a
//! credential: the toolkit transport is a scripted in-memory double, the
//! signer is the real toolkit SigV4 signer over fixture keys, and the SDK
//! service crates only encode requests and decode the scripted responses.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use toolkit_aws::sdk::SigV4Signer;
use toolkit_aws::testing::{FakeProvider, RecordingSleeper};
use toolkit_aws::{
    AwsError, Headers, HttpRequest, NoJitter, ResponseStream, Signer, SigningScope, StreamError,
    StreamLimits, StreamingTransport, ThreadSleeper, FRAMING_HEADERS,
};
use toolkit_core::Json;

use super::{json, summary, NOW, NOW_SECONDS, SECRET};
use crate::docgraph::client::{AwsClient, Service};
use crate::docgraph::infra::run_infra_checks;
use crate::docgraph::live::{
    self, refuse_credential_identity, DeadlineSigner, LiveAwsClient, ReviewBounds, OPERATIONS,
};

const REGION: &str = "us-east-1";
const START: &str = "2026-01-24T12:00:00Z";
const START_SECONDS: i64 = 1_769_256_000;
const EVENTS_START: &str = "2026-01-18T12:00:00Z";
const ARN: &str = "arn:aws:rds:us-east-1:111122223333:cluster:prod-docdb";

// ---- a minimal CBOR codec for the CloudWatch (RPC-v2 CBOR) fixtures ----

mod cbor {
    /// The CBOR values the CloudWatch fixtures use.
    #[derive(Debug, Clone, PartialEq)]
    pub(super) enum V {
        Map(Vec<(String, V)>),
        Array(Vec<V>),
        Text(String),
        Uint(u64),
        Float(f64),
        Tag(u64, Box<V>),
    }

    pub(super) fn text(value: &str) -> V {
        V::Text(value.to_string())
    }

    pub(super) fn map(pairs: Vec<(&str, V)>) -> V {
        V::Map(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    pub(super) fn timestamp(seconds: i64) -> V {
        V::Tag(1, Box::new(V::Float(seconds as f64)))
    }

    fn head(major: u8, n: u64, out: &mut Vec<u8>) {
        let major = major << 5;
        if n < 24 {
            out.push(major | n as u8);
        } else if n < 256 {
            out.push(major | 24);
            out.push(n as u8);
        } else if n < 65_536 {
            out.push(major | 25);
            out.extend((n as u16).to_be_bytes());
        } else if n < (1 << 32) {
            out.push(major | 26);
            out.extend((n as u32).to_be_bytes());
        } else {
            out.push(major | 27);
            out.extend(n.to_be_bytes());
        }
    }

    /// Maps are indefinite-length (`0xbf` .. `0xff`), as the SDK encoder
    /// writes them; everything else is definite and minimal.
    pub(super) fn encode(value: &V) -> Vec<u8> {
        let mut out = Vec::new();
        write(value, &mut out);
        out
    }

    fn write(value: &V, out: &mut Vec<u8>) {
        match value {
            V::Map(pairs) => {
                out.push(0xbf);
                for (key, value) in pairs {
                    write(&V::Text(key.clone()), out);
                    write(value, out);
                }
                out.push(0xff);
            }
            V::Array(items) => {
                head(4, items.len() as u64, out);
                for item in items {
                    write(item, out);
                }
            }
            V::Text(text) => {
                head(3, text.len() as u64, out);
                out.extend(text.as_bytes());
            }
            V::Uint(n) => head(0, *n, out),
            V::Float(x) => {
                out.push(0xfb);
                out.extend(x.to_be_bytes());
            }
            V::Tag(tag, inner) => {
                head(6, *tag, out);
                write(inner, out);
            }
        }
    }

    /// Decode one value; panics on anything the fixtures do not use.
    pub(super) fn decode(bytes: &[u8]) -> V {
        let mut at = 0;
        let value = read(bytes, &mut at);
        assert_eq!(at, bytes.len(), "trailing CBOR bytes");
        value
    }

    fn argument(bytes: &[u8], at: &mut usize, info: u8) -> u64 {
        let take = |at: &mut usize, n: usize| {
            let mut value = 0_u64;
            for byte in &bytes[*at..*at + n] {
                value = (value << 8) | u64::from(*byte);
            }
            *at += n;
            value
        };
        match info {
            0..=23 => u64::from(info),
            24 => take(at, 1),
            25 => take(at, 2),
            26 => take(at, 4),
            27 => take(at, 8),
            _ => panic!("unsupported CBOR length"),
        }
    }

    fn read(bytes: &[u8], at: &mut usize) -> V {
        let initial = bytes[*at];
        *at += 1;
        let (major, info) = (initial >> 5, initial & 0x1f);
        match (major, info) {
            (5, 31) => {
                let mut pairs = Vec::new();
                while bytes[*at] != 0xff {
                    let V::Text(key) = read(bytes, at) else {
                        panic!("map key")
                    };
                    pairs.push((key, read(bytes, at)));
                }
                *at += 1;
                V::Map(pairs)
            }
            (5, _) => {
                let n = argument(bytes, at, info);
                let mut pairs = Vec::new();
                for _ in 0..n {
                    let V::Text(key) = read(bytes, at) else {
                        panic!("map key")
                    };
                    pairs.push((key, read(bytes, at)));
                }
                V::Map(pairs)
            }
            (4, _) => {
                let n = argument(bytes, at, info);
                V::Array((0..n).map(|_| read(bytes, at)).collect())
            }
            (3, _) => {
                let n = argument(bytes, at, info) as usize;
                let text = std::str::from_utf8(&bytes[*at..*at + n]).expect("UTF-8");
                *at += n;
                V::Text(text.to_string())
            }
            (0, _) => V::Uint(argument(bytes, at, info)),
            (6, _) => {
                let tag = argument(bytes, at, info);
                V::Tag(tag, Box::new(read(bytes, at)))
            }
            (7, 27) => {
                let mut raw = [0_u8; 8];
                raw.copy_from_slice(&bytes[*at..*at + 8]);
                *at += 8;
                V::Float(f64::from_be_bytes(raw))
            }
            _ => panic!("unsupported CBOR item {initial:#x}"),
        }
    }

    /// A field of a decoded map.
    pub(super) fn field<'v>(value: &'v V, key: &str) -> Option<&'v V> {
        match value {
            V::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

use cbor::V;

// ---- doubles: the toolkit signer (recording), the scripted transport ----

static TICK: AtomicU64 = AtomicU64::new(0);

/// A clock that moves one second per signature, so every attempt carries a
/// fresh `x-amz-date` and signature.
fn ticking() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_769_860_800 + TICK.fetch_add(1, Ordering::SeqCst))
}

/// One toolkit signing: what went in, what came out, and the deadline.
struct Signing {
    scope: SigningScope,
    before: HttpRequest,
    after: HttpRequest,
    deadline: Instant,
}

/// The real toolkit SigV4 signer over fixture keys, recording each signing.
struct ToolkitSigner {
    signer: SigV4Signer<FakeProvider>,
    log: RefCell<Vec<Signing>>,
}

impl ToolkitSigner {
    fn new() -> Self {
        Self {
            signer: SigV4Signer::new(FakeProvider::with_keys(
                "fixture",
                "AKIDFIXTUREEXAMPLE",
                "fixture-secret-key",
            ))
            .with_clock(ticking),
            log: RefCell::new(Vec::new()),
        }
    }

    fn count(&self) -> usize {
        self.log.borrow().len()
    }
}

impl DeadlineSigner for ToolkitSigner {
    fn sign_until(
        &self,
        request: &mut HttpRequest,
        scope: &SigningScope,
        deadline: Instant,
    ) -> Result<(), AwsError> {
        let before = request.clone();
        self.signer.sign(request, scope)?;
        self.log.borrow_mut().push(Signing {
            scope: scope.clone(),
            before,
            after: request.clone(),
            deadline,
        });
        Ok(())
    }
}

/// A signer whose credentials always fail (counting attempts).
struct FailingSigner {
    error: AwsError,
    calls: Cell<usize>,
}

impl DeadlineSigner for FailingSigner {
    fn sign_until(&self, _: &mut HttpRequest, _: &SigningScope, _: Instant) -> Result<(), AwsError> {
        self.calls.set(self.calls.get() + 1);
        Err(self.error.clone())
    }
}

type Reply = Result<(u16, Vec<(&'static str, String)>, Vec<u8>), AwsError>;

/// The scripted toolkit streaming transport: every opened request (with the
/// limits it was opened under) is recorded, and `route` answers it.
struct Wire<'r> {
    route: Box<dyn Fn(&HttpRequest, usize) -> Reply + 'r>,
    sent: RefCell<Vec<(HttpRequest, StreamLimits)>>,
}

impl<'r> Wire<'r> {
    fn new(route: impl Fn(&HttpRequest, usize) -> Reply + 'r) -> Self {
        Self {
            route: Box::new(route),
            sent: RefCell::new(Vec::new()),
        }
    }

    fn sends(&self) -> usize {
        self.sent.borrow().len()
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.sent.borrow().iter().map(|(r, _)| r.clone()).collect()
    }
}

struct Body {
    status: u16,
    headers: Headers,
    chunks: VecDeque<Vec<u8>>,
}

impl ResponseStream for Body {
    fn status(&self) -> u16 {
        self.status
    }

    fn headers(&self) -> &Headers {
        &self.headers
    }

    fn narrow_body_limit(&mut self, _: u64) {}

    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        Ok(self.chunks.pop_front())
    }
}

impl StreamingTransport for Wire<'_> {
    fn open<'a>(
        &'a self,
        request: &HttpRequest,
        limits: &StreamLimits,
    ) -> Result<Box<dyn ResponseStream + 'a>, AwsError> {
        let index = self.sends();
        self.sent.borrow_mut().push((request.clone(), *limits));
        let (status, pairs, body) = (self.route)(request, index)?;
        let mut headers = Headers::new();
        for (name, value) in pairs {
            headers.insert(name, &value).expect("fixture header");
        }
        Ok(Box::new(Body {
            status,
            headers,
            chunks: body.chunks(7).map(<[u8]>::to_vec).collect(),
        }))
    }
}

/// The operation a request names: the Query `Action`, the JSON target or the
/// RPC-v2 CBOR path.
fn action(request: &HttpRequest) -> String {
    if let Some(target) = request.headers.get("x-amz-target") {
        return target.rsplit('.').next().unwrap_or_default().to_string();
    }
    if let Some((_, op)) = request.url.split_once("/operation/") {
        return op.to_string();
    }
    let body = String::from_utf8_lossy(&request.body);
    body.split('&')
        .find_map(|pair| pair.strip_prefix("Action="))
        .unwrap_or_default()
        .to_string()
}

fn xml(body: &str) -> Reply {
    Ok((200, vec![("content-type", "text/xml".into())], body.as_bytes().to_vec()))
}

fn query_ok(op: &str, result: &str) -> Reply {
    xml(&format!(
        r#"<{op}Response xmlns="http://rds.amazonaws.com/doc/2014-10-31/"><{op}Result>{result}</{op}Result><ResponseMetadata><RequestId>req-1</RequestId></ResponseMetadata></{op}Response>"#
    ))
}

fn json_ok(body: &str) -> Reply {
    Ok((
        200,
        vec![("content-type", "application/x-amz-json-1.1".into())],
        body.as_bytes().to_vec(),
    ))
}

fn cbor_reply(status: u16, value: &V, extra: Vec<(&'static str, String)>) -> Reply {
    let mut headers = vec![
        ("content-type", "application/cbor".into()),
        ("smithy-protocol", "rpc-v2-cbor".into()),
    ];
    headers.extend(extra);
    Ok((status, headers, cbor::encode(value)))
}

fn query_error(status: u16, code: &str) -> Reply {
    Ok((
        status,
        vec![("content-type", "text/xml".into())],
        format!(
            "<ErrorResponse><Error><Type>Sender</Type><Code>{code}</Code><Message>{SECRET}</Message></Error><RequestId>req-err</RequestId></ErrorResponse>"
        )
        .into_bytes(),
    ))
}

fn client<'a>(
    transport: &'a Wire<'_>,
    signer: &'a dyn DeadlineSigner,
    sleeper: &'a RecordingSleeper,
) -> LiveAwsClient<'a> {
    LiveAwsClient::new(REGION, transport, signer, sleeper, &NoJitter).expect("client")
}

/// Object keys sorted at every level, so seam objects compare by content.
fn canonical(value: &Json) -> Json {
    match value {
        Json::Object(pairs) => {
            let mut pairs: Vec<(String, Json)> = pairs
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(pairs)
        }
        Json::Array(items) => Json::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn assert_seam(got: &Json, expected: &str) {
    assert_eq!(canonical(got), canonical(&json(expected)));
}

// ---- the healthy-cluster fixture: one source, two encodings ----

const CLUSTER_XML: &str = "<DBClusters><DBCluster><BackupRetentionPeriod>7</BackupRetentionPeriod><DBClusterIdentifier>prod-docdb</DBClusterIdentifier><DBClusterParameterGroup>prod-params</DBClusterParameterGroup><DBSubnetGroup>sng</DBSubnetGroup><Status>available</Status><Engine>docdb</Engine><EngineVersion>5.0.0</EngineVersion><MasterUsername>FAKE-MASTER-USER</MasterUsername><PreferredMaintenanceWindow>sun:05:00-sun:06:00</PreferredMaintenanceWindow><VpcSecurityGroups><VpcSecurityGroupMembership><VpcSecurityGroupId>sg-0abc</VpcSecurityGroupId><Status>active</Status></VpcSecurityGroupMembership></VpcSecurityGroups><StorageEncrypted>true</StorageEncrypted><KmsKeyId>FAKE-KMS-KEY</KmsKeyId><DBClusterArn>arn:aws:rds:us-east-1:111122223333:cluster:prod-docdb</DBClusterArn><EnabledCloudwatchLogsExports><member>audit</member><member>profiler</member></EnabledCloudwatchLogsExports><DeletionProtection>true</DeletionProtection><StorageType>iopt1</StorageType></DBCluster></DBClusters>";

const CLUSTER_SEAM: &str = r#"{"DBClusters": [{"BackupRetentionPeriod": 7, "DeletionProtection": true,
  "EngineVersion": "5.0.0", "DBClusterParameterGroup": "prod-params",
  "VpcSecurityGroups": [{"VpcSecurityGroupId": "sg-0abc"}], "StorageEncrypted": true,
  "EnabledCloudwatchLogsExports": ["audit", "profiler"], "DBSubnetGroup": "sng",
  "PreferredMaintenanceWindow": "sun:05:00-sun:06:00",
  "DBClusterArn": "arn:aws:rds:us-east-1:111122223333:cluster:prod-docdb", "StorageType": "iopt1"}]}"#;

const INSTANCES_XML: &str = "<DBInstances><DBInstance><DBInstanceIdentifier>i-writer</DBInstanceIdentifier><DBInstanceClass>db.r6g.large</DBInstanceClass><Engine>docdb</Engine><AvailabilityZone>us-east-1a</AvailabilityZone></DBInstance><DBInstance><DBInstanceIdentifier>i-reader</DBInstanceIdentifier><DBInstanceClass>db.r6g.large</DBInstanceClass><AvailabilityZone>us-east-1b</AvailabilityZone></DBInstance></DBInstances>";

const INSTANCES_SEAM: &str = r#"{"DBInstances": [
  {"DBInstanceIdentifier": "i-writer", "DBInstanceClass": "db.r6g.large", "AvailabilityZone": "us-east-1a"},
  {"DBInstanceIdentifier": "i-reader", "DBInstanceClass": "db.r6g.large", "AvailabilityZone": "us-east-1b"}]}"#;

const PARAMETERS_XML: &str = "<Parameters><Parameter><ParameterName>tls</ParameterName><ParameterValue>enabled</ParameterValue><Description>FAKE</Description></Parameter><Parameter><ParameterName>tls_version</ParameterName><ParameterValue>TLSv1.2</ParameterValue></Parameter><Parameter><ParameterName>other</ParameterName><ParameterValue>x</ParameterValue></Parameter></Parameters>";

const PARAMETERS_SEAM: &str = r#"{"Parameters": [{"ParameterName": "tls", "ParameterValue": "enabled"},
  {"ParameterName": "tls_version", "ParameterValue": "TLSv1.2"}, {"ParameterName": "other", "ParameterValue": "x"}]}"#;

const SUBNETS_XML: &str = "<DBSubnetGroups><DBSubnetGroup><DBSubnetGroupName>sng</DBSubnetGroupName><Subnets><Subnet><SubnetIdentifier>s-1</SubnetIdentifier><SubnetAvailabilityZone><Name>a</Name></SubnetAvailabilityZone></Subnet><Subnet><SubnetIdentifier>s-2</SubnetIdentifier><SubnetAvailabilityZone><Name>b</Name></SubnetAvailabilityZone></Subnet><Subnet><SubnetIdentifier>s-3</SubnetIdentifier><SubnetAvailabilityZone><Name>c</Name></SubnetAvailabilityZone></Subnet></Subnets></DBSubnetGroup></DBSubnetGroups>";

const SUBNETS_SEAM: &str = r#"{"DBSubnetGroups": [{"Subnets": [{"SubnetAvailabilityZone": {"Name": "a"}},
  {"SubnetAvailabilityZone": {"Name": "b"}}, {"SubnetAvailabilityZone": {"Name": "c"}}]}]}"#;

const TAGS_XML: &str =
    "<TagList><Tag><Key>team</Key><Value>db</Value></Tag><Tag><Key>env</Key><Value>prod</Value></Tag></TagList>";

const TAGS_SEAM: &str =
    r#"{"TagList": [{"Key": "team", "Value": "db"}, {"Key": "env", "Value": "prod"}]}"#;

const GROUPS_XML: &str = r#"<DescribeSecurityGroupsResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>req-1</requestId><securityGroupInfo><item><ownerId>111122223333</ownerId><groupId>sg-0abc</groupId><groupName>db</groupName><ipPermissions><item><ipProtocol>tcp</ipProtocol><fromPort>27017</fromPort><toPort>27017</toPort><ipRanges><item><cidrIp>10.0.0.0/16</cidrIp></item></ipRanges></item></ipPermissions></item></securityGroupInfo></DescribeSecurityGroupsResponse>"#;

const GROUPS_SEAM: &str =
    r#"{"SecurityGroups": [{"IpPermissions": [{"IpRanges": [{"CidrIp": "10.0.0.0/16"}]}]}]}"#;

const SECRETS_JSON: &str = r#"{"SecretList": [{"ARN": "arn:aws:secretsmanager:us-east-1:111122223333:secret:x", "Name": "prod-docdb/credentials", "KmsKeyId": "FAKE-KMS-KEY", "Tags": []}]}"#;

const SECRETS_SEAM: &str = r#"{"SecretList": [{"Name": "prod-docdb/credentials"}]}"#;

fn alarms_cbor() -> V {
    let alarm = |name: &str| cbor::map(vec![("AlarmName", cbor::text(name))]);
    cbor::map(vec![(
        "MetricAlarms",
        V::Array(vec![
            alarm("prod-docdb-cpu"),
            alarm("prod-docdb-memory"),
            alarm("prod-docdb-connections"),
        ]),
    )])
}

const ALARMS_SEAM: &str = r#"{"MetricAlarms": [{"AlarmName": "prod-docdb-cpu"},
  {"AlarmName": "prod-docdb-memory"}, {"AlarmName": "prod-docdb-connections"}]}"#;

/// The value of `statistic` for `metric` in the healthy fixture (`None`: no
/// datapoint at all).
fn metric_value(metric: &str, statistic: &str) -> Option<f64> {
    Some(match (metric, statistic) {
        ("CPUUtilization", "Average") => 40.0,
        ("CPUUtilization", "Maximum") => 60.0,
        ("BufferCacheHitRatio", _) => 99.5,
        ("DatabaseConnections", "Maximum") => 100.0,
        ("DatabaseConnections", _) => 10.0,
        ("ReadIOPS", _) => 100.0,
        ("FreeableMemory", _) => 8.0 * 1_073_741_824.0,
        ("SwapUsage", _) => 0.0,
        ("DiskQueueDepth", _) => 0.5,
        ("IndexBufferCacheHitRatio", _) => 99.9,
        ("AvailableMVCCIds", _) => 1_400_000_000.0,
        _ => return None,
    })
}

/// The metric name and statistics a decoded GetMetricStatistics body asks for.
fn metric_request(body: &[u8]) -> (String, Vec<String>) {
    let decoded = cbor::decode(body);
    let Some(V::Text(name)) = cbor::field(&decoded, "MetricName") else {
        panic!("metric name")
    };
    let Some(V::Array(stats)) = cbor::field(&decoded, "Statistics") else {
        panic!("statistics")
    };
    let stats = stats
        .iter()
        .map(|stat| match stat {
            V::Text(stat) => stat.clone(),
            _ => panic!("statistic"),
        })
        .collect();
    (name.clone(), stats)
}

fn metric_cbor(name: &str, stats: &[String]) -> V {
    let mut points = Vec::new();
    if metric_value(name, &stats[0]).is_some() {
        let mut pairs = vec![("Timestamp".to_string(), cbor::timestamp(NOW_SECONDS))];
        for stat in stats {
            pairs.push((stat.clone(), V::Float(metric_value(name, stat).unwrap())));
        }
        pairs.push(("Unit".to_string(), cbor::text("Count")));
        points.push(V::Map(pairs));
    }
    cbor::map(vec![("Datapoints", V::Array(points))])
}

fn metric_seam(name: &str, stats: &[String]) -> Json {
    let mut points = Vec::new();
    if metric_value(name, &stats[0]).is_some() {
        points.push(Json::Object(
            stats
                .iter()
                .map(|stat| (stat.clone(), Json::Float(metric_value(name, stat).unwrap())))
                .collect(),
        ));
    }
    Json::object(vec![("Datapoints", Json::Array(points))])
}

/// The healthy cluster on the wire.
fn healthy_wire(request: &HttpRequest, _: usize) -> Reply {
    match action(request).as_str() {
        "DescribeDBClusters" => query_ok("DescribeDBClusters", CLUSTER_XML),
        "DescribeDBInstances" => query_ok("DescribeDBInstances", INSTANCES_XML),
        "DescribeEvents" => query_ok("DescribeEvents", "<Events/>"),
        "DescribeDBClusterParameters" => query_ok("DescribeDBClusterParameters", PARAMETERS_XML),
        "DescribeDBSubnetGroups" => query_ok("DescribeDBSubnetGroups", SUBNETS_XML),
        "ListTagsForResource" => query_ok("ListTagsForResource", TAGS_XML),
        "DescribeSecurityGroups" => xml(GROUPS_XML),
        "ListSecrets" => json_ok(SECRETS_JSON),
        "DescribeAlarms" => cbor_reply(200, &alarms_cbor(), vec![]),
        "GetMetricStatistics" => {
            let (name, stats) = metric_request(&request.body);
            cbor_reply(200, &metric_cbor(&name, &stats), vec![])
        }
        other => panic!("unexpected operation {other}"),
    }
}

/// The same cluster as seam objects (the shape of the recorded fixtures).
struct SeamFixture;

impl AwsClient for SeamFixture {
    fn call(&mut self, _: Service, operation: &str, request: &Json) -> Result<Json, AwsError> {
        let text = match operation {
            "DescribeDBClusters" => CLUSTER_SEAM,
            "DescribeDBInstances" => INSTANCES_SEAM,
            "DescribeEvents" => r#"{"Events": []}"#,
            "DescribeDBClusterParameters" => PARAMETERS_SEAM,
            "DescribeDBSubnetGroups" => SUBNETS_SEAM,
            "ListTagsForResource" => TAGS_SEAM,
            "DescribeSecurityGroups" => GROUPS_SEAM,
            "ListSecrets" => SECRETS_SEAM,
            "DescribeAlarms" => ALARMS_SEAM,
            "GetMetricStatistics" => {
                let name = request.get("MetricName").and_then(Json::as_str).unwrap();
                let stats: Vec<String> = match request.get("Statistics") {
                    Some(Json::Array(items)) => items
                        .iter()
                        .map(|s| s.as_str().unwrap().to_string())
                        .collect(),
                    _ => panic!("statistics"),
                };
                return Ok(metric_seam(name, &stats));
            }
            other => panic!("unexpected operation {other}"),
        };
        Ok(json(text))
    }
}

/// Records every seam call and response of the client it wraps.
struct Seen<'c, C> {
    inner: &'c mut C,
    calls: Vec<(Service, String, Json, Result<Json, AwsError>)>,
}

impl<C: AwsClient> AwsClient for Seen<'_, C> {
    fn call(&mut self, service: Service, operation: &str, request: &Json) -> Result<Json, AwsError> {
        let result = self.inner.call(service, operation, request);
        self.calls.push((
            service,
            operation.to_string(),
            request.clone(),
            result.clone(),
        ));
        result
    }
}

// ---- T-DG-04a: the ten encodings ----

/// `(service, operation, seam request, URL, signing name, content type,
/// expected final body bytes)` for every allowed pair.
fn encodings() -> Vec<(Service, &'static str, String, String, &'static str, &'static str, Vec<u8>)> {
    let query = |text: &str| text.as_bytes().to_vec();
    let rds = "https://rds.us-east-1.amazonaws.com/".to_string();
    let metric = cbor::map(vec![
        ("Namespace", cbor::text("AWS/DocDB")),
        ("MetricName", cbor::text("CPUUtilization")),
        (
            "Dimensions",
            V::Array(vec![cbor::map(vec![
                ("Name", cbor::text("DBInstanceIdentifier")),
                ("Value", cbor::text("i-writer")),
            ])]),
        ),
        ("StartTime", cbor::timestamp(START_SECONDS)),
        ("EndTime", cbor::timestamp(NOW_SECONDS)),
        ("Period", V::Uint(3600)),
        (
            "Statistics",
            V::Array(vec![cbor::text("Average"), cbor::text("Maximum")]),
        ),
    ]);
    vec![
        (
            Service::Docdb,
            "DescribeDBClusters",
            r#"{"DBClusterIdentifier": "prod-docdb"}"#.into(),
            rds.clone(),
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=DescribeDBClusters&Version=2014-10-31&DBClusterIdentifier=prod-docdb"),
        ),
        (
            Service::Docdb,
            "DescribeDBInstances",
            r#"{"Filters": [{"Name": "db-cluster-id", "Values": ["prod-docdb"]}]}"#.into(),
            rds.clone(),
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=DescribeDBInstances&Version=2014-10-31&Filters.Filter.1.Name=db-cluster-id&Filters.Filter.1.Values.Value.1=prod-docdb"),
        ),
        (
            Service::Docdb,
            "DescribeEvents",
            format!(r#"{{"SourceIdentifier": "prod-docdb", "SourceType": "db-cluster", "StartTime": "{EVENTS_START}", "EndTime": "{NOW}", "Marker": "m-2"}}"#),
            rds.clone(),
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=DescribeEvents&Version=2014-10-31&SourceIdentifier=prod-docdb&SourceType=db-cluster&StartTime=2026-01-18T12%3A00%3A00Z&EndTime=2026-01-31T12%3A00%3A00Z&Marker=m-2"),
        ),
        (
            Service::Docdb,
            "DescribeDBClusterParameters",
            r#"{"DBClusterParameterGroupName": "prod-params"}"#.into(),
            rds.clone(),
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=DescribeDBClusterParameters&Version=2014-10-31&DBClusterParameterGroupName=prod-params"),
        ),
        (
            Service::Docdb,
            "DescribeDBSubnetGroups",
            r#"{"DBSubnetGroupName": "sng"}"#.into(),
            rds.clone(),
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=DescribeDBSubnetGroups&Version=2014-10-31&DBSubnetGroupName=sng"),
        ),
        (
            Service::Docdb,
            "ListTagsForResource",
            format!(r#"{{"ResourceName": "{ARN}"}}"#),
            rds,
            "rds",
            "application/x-www-form-urlencoded",
            query("Action=ListTagsForResource&Version=2014-10-31&ResourceName=arn%3Aaws%3Ards%3Aus-east-1%3A111122223333%3Acluster%3Aprod-docdb"),
        ),
        (
            Service::CloudWatch,
            "GetMetricStatistics",
            format!(r#"{{"Namespace":"AWS/DocDB","MetricName":"CPUUtilization","Dimensions":[{{"Name":"DBInstanceIdentifier","Value":"i-writer"}}],"StartTime":"{START}","EndTime":"{NOW}","Period":3600,"Statistics":["Average","Maximum"]}}"#),
            "https://monitoring.us-east-1.amazonaws.com/service/GraniteServiceVersion20100801/operation/GetMetricStatistics".into(),
            "monitoring",
            "application/cbor",
            cbor::encode(&metric),
        ),
        (
            Service::CloudWatch,
            "DescribeAlarms",
            r#"{"AlarmNamePrefix": "prod-docdb"}"#.into(),
            "https://monitoring.us-east-1.amazonaws.com/service/GraniteServiceVersion20100801/operation/DescribeAlarms".into(),
            "monitoring",
            "application/cbor",
            cbor::encode(&cbor::map(vec![("AlarmNamePrefix", cbor::text("prod-docdb"))])),
        ),
        (
            Service::Ec2,
            "DescribeSecurityGroups",
            r#"{"GroupIds": ["sg-0abc"]}"#.into(),
            "https://ec2.us-east-1.amazonaws.com/".into(),
            "ec2",
            "application/x-www-form-urlencoded",
            query("Action=DescribeSecurityGroups&Version=2016-11-15&GroupId.1=sg-0abc"),
        ),
        (
            Service::SecretsManager,
            "ListSecrets",
            r#"{"NextToken": "t-2"}"#.into(),
            "https://secretsmanager.us-east-1.amazonaws.com/".into(),
            "secretsmanager",
            "application/x-amz-json-1.1",
            query(r#"{"NextToken":"t-2"}"#),
        ),
    ]
}

#[test]
fn t_dg_04a_all_ten_encodings_reach_the_injected_transport_signed() {
    let cases = encodings();
    assert_eq!(cases.len(), 10);
    assert_eq!(OPERATIONS.len(), 10, "exactly the ten allowed pairs");
    for (service, operation, request, url, signing_name, content_type, body) in cases {
        let transport = Wire::new(|_: &HttpRequest, _| Err(AwsError::connect("fixture: stop here")));
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let before = Instant::now();
        let mut live = client(&transport, &signer, &sleeper);
        let _ = live.call(service, operation, &json(&request));
        let sent = transport.sent.borrow();
        assert!(!sent.is_empty(), "{operation}: nothing reached the transport");
        let (first, limits) = &sent[0];
        assert_eq!(first.url, url, "{operation}: endpoint");
        assert_eq!(first.method.as_str(), "POST", "{operation}: method");
        assert_eq!(
            first.body, body,
            "{operation}: final body bytes\n got: {:?}\nwant: {:?}",
            String::from_utf8_lossy(&first.body),
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            first.headers.get("content-type"),
            Some(content_type),
            "{operation}: protocol"
        );
        if service == Service::SecretsManager {
            assert_eq!(
                first.headers.get("x-amz-target"),
                Some("secretsmanager.ListSecrets")
            );
        }
        // The toolkit signer is the only signer: one signing per send, scope
        // from the operation table, the signed request is exactly what went
        // to the transport, and no framing header was signed or sent.
        let log = signer.log.borrow();
        assert_eq!(log.len(), sent.len(), "{operation}: one signing per send");
        let signing = &log[0];
        assert_eq!(signing.scope.region, REGION);
        assert_eq!(signing.scope.service, signing_name, "{operation}: signing name");
        assert_eq!(&signing.after, first, "{operation}: sent != signed");
        assert_eq!(signing.before.body, body, "{operation}: body changed by signing");
        for name in FRAMING_HEADERS {
            assert!(signing.before.headers.get(name).is_none(), "{operation}: {name} signed");
            assert!(first.headers.get(name).is_none(), "{operation}: {name} sent");
        }
        for name in ["authorization", "x-amz-date"] {
            assert!(
                signing.before.headers.get(name).is_none(),
                "{operation}: the SDK prepared {name}"
            );
            assert!(first.headers.get(name).is_some(), "{operation}: {name} missing");
        }
        let authorization = first.headers.get("authorization").unwrap();
        assert!(
            authorization.contains(&format!("/{REGION}/{signing_name}/aws4_request")),
            "{operation}: credential scope"
        );
        assert!(
            authorization.contains("SignedHeaders=") && authorization.contains("content-type"),
            "{operation}: signed header set"
        );
        // One absolute deadline: the signer and the transport share it, it
        // lies within the operation budget, and the response is capped.
        assert_eq!(Some(signing.deadline), limits.deadline(), "{operation}");
        assert!(signing.deadline <= before + Duration::from_secs(60));
        assert!(signing.deadline > before + Duration::from_secs(50));
        assert_eq!(limits.max_body_bytes(), 16 * 1024 * 1024);
    }
}

#[test]
fn t_dg_04a_unknown_pairs_and_unmapped_request_members_fail_before_the_transport() {
    let transport = Wire::new(|_: &HttpRequest, _| panic!("nothing may be sent"));
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let mut live = client(&transport, &signer, &sleeper);
    for (service, operation, request) in [
        (Service::Docdb, "DeleteDBCluster", "{}"),
        (Service::Docdb, "ModifyDBCluster", "{}"),
        (Service::Ec2, "DescribeDBClusters", "{}"),
        (Service::CloudWatch, "PutMetricAlarm", "{}"),
        (Service::SecretsManager, "GetSecretValue", r#"{"SecretId": "x"}"#),
        (Service::SecretsManager, "BatchGetSecretValue", "{}"),
        (Service::SecretsManager, "DescribeSecret", "{}"),
        (Service::Docdb, "DescribeDBClusters", r#"{"DBClusterIdentifier": "x", "Filters": []}"#),
        (Service::Docdb, "DescribeDBClusters", r#"{"DBClusterIdentifier": 5}"#),
        (Service::Docdb, "DescribeEvents", r#"{"StartTime": "yesterday"}"#),
        (Service::CloudWatch, "GetMetricStatistics", r#"{"Period": "3600"}"#),
        (Service::Ec2, "DescribeSecurityGroups", r#"{"GroupIds": "sg-1"}"#),
        (Service::SecretsManager, "ListSecrets", r#"{"Filters": []}"#),
    ] {
        let error = live
            .call(service, operation, &json(request))
            .expect_err(operation);
        assert!(
            matches!(error, AwsError::InvalidConfig { .. }),
            "{operation} {request}: {error:?}"
        );
        assert!(!error.is_retryable());
        assert!(!error.to_string().contains("yesterday"), "{error}");
    }
    assert_eq!(transport.sends(), 0);
    assert_eq!(signer.count(), 0, "nothing was signed");
}

// ---- T-DG-04b: modeled responses become the seam objects and reports ----

#[test]
fn t_dg_04b_a_live_review_equals_the_seam_fixture_review() {
    let transport = Wire::new(healthy_wire);
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let mut live = client(&transport, &signer, &sleeper);
    let mut seen = Seen {
        inner: &mut live,
        calls: Vec::new(),
    };
    let from_wire = run_infra_checks(&mut seen, "prod-docdb", NOW_SECONDS);
    let mut fixture = SeamFixture;
    let from_seam = run_infra_checks(&mut fixture, "prod-docdb", NOW_SECONDS);
    assert_eq!(from_wire.checks, from_seam.checks, "{:?}", summary(&from_wire));
    // Every seam object equals the fixture's, call by call.
    let mut fixture = SeamFixture;
    for (service, operation, request, result) in &seen.calls {
        let got = result.as_ref().unwrap_or_else(|e| panic!("{operation}: {e:?}"));
        let want = fixture.call(*service, operation, request).unwrap();
        assert_eq!(canonical(got), canonical(&want), "{operation}");
    }
    assert_eq!(seen.calls.len(), transport.sends(), "one send per seam call");
    assert!(sleeper.sleeps().is_empty());
    // The healthy cluster passes (the live mapping, like boto3, has no
    // instance-level IsClusterWriter, so the writer-only check is skipped).
    let labels: Vec<String> = from_wire.checks.iter().map(|c| c.label.clone()).collect();
    for label in [
        "Backup retention period (7 days)",
        "Deletion protection (enabled)",
        "Instances across 2 AZ(s)",
        "TLS minimum version: TLSv1.2",
        "TLS (enabled)",
        "Security groups properly restricted (1 checked)",
        "Secrets Manager references this cluster",
        "Subnet group spans 3 AZ(s)",
        "CloudWatch alarms (3 configured)",
        "Cost allocation tags (2 tags)",
        "Storage type: iopt1",
        "CPU for i-writer (avg 40.0%, P95 60.0%)",
    ] {
        assert!(labels.iter().any(|l| l == label), "missing {label}: {labels:?}");
    }
    assert!(!from_wire.checks.iter().any(|c| c.id == "REL9"));
    let rendered = format!("{:?}", from_wire.checks);
    for leak in ["FAKE-MASTER-USER", "FAKE-KMS-KEY", "secret:x"] {
        assert!(!rendered.contains(leak), "{leak}");
    }
}

#[test]
fn t_dg_04b_zero_and_false_stay_distinct_from_absent() {
    let wire = |_: &HttpRequest, n: usize| {
        if n == 0 {
            query_ok(
                "DescribeDBClusters",
                "<DBClusters><DBCluster><BackupRetentionPeriod>0</BackupRetentionPeriod><DeletionProtection>false</DeletionProtection><VpcSecurityGroups/></DBCluster></DBClusters>",
            )
        } else {
            query_ok("DescribeDBClusters", "<DBClusters><DBCluster></DBCluster></DBClusters>")
        }
    };
    let transport = Wire::new(wire);
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let mut live = client(&transport, &signer, &sleeper);
    let request = json(r#"{"DBClusterIdentifier": "prod-docdb"}"#);
    let zero = live.call(Service::Docdb, "DescribeDBClusters", &request).unwrap();
    assert_seam(
        &zero,
        r#"{"DBClusters": [{"BackupRetentionPeriod": 0, "DeletionProtection": false, "VpcSecurityGroups": []}]}"#,
    );
    let absent = live.call(Service::Docdb, "DescribeDBClusters", &request).unwrap();
    assert_seam(&absent, r#"{"DBClusters": [{}]}"#);

    // Datapoints: a zero statistic is kept, a missing one stays missing.
    let transport = Wire::new(|_: &HttpRequest, _| {
        cbor_reply(
            200,
            &cbor::map(vec![(
                "Datapoints",
                V::Array(vec![cbor::map(vec![
                    ("Timestamp", cbor::timestamp(NOW_SECONDS)),
                    ("Maximum", V::Float(0.0)),
                ])]),
            )]),
            vec![],
        )
    });
    let mut live = client(&transport, &signer, &sleeper);
    let got = live
        .call(
            Service::CloudWatch,
            "GetMetricStatistics",
            &json(&format!(r#"{{"Namespace":"AWS/DocDB","MetricName":"SwapUsage","Dimensions":[{{"Name":"DBInstanceIdentifier","Value":"i-writer"}}],"StartTime":"{START}","EndTime":"{NOW}","Period":3600,"Statistics":["Maximum"]}}"#)),
        )
        .unwrap();
    assert_seam(&got, r#"{"Datapoints": [{"Maximum": 0.0}]}"#);
}

#[test]
fn t_dg_04b_pagination_tokens_pass_through_one_page_per_call() {
    let transport = Wire::new(|request: &HttpRequest, n: usize| {
        let body = String::from_utf8_lossy(&request.body).into_owned();
        match action(request).as_str() {
            "DescribeEvents" if n == 0 => {
                assert!(!body.contains("Marker"), "{body}");
                query_ok(
                    "DescribeEvents",
                    "<Events><Event><Message>Started cross AZ failover</Message><EventCategories><EventCategory>failover</EventCategory></EventCategories><SourceIdentifier>prod-docdb</SourceIdentifier></Event></Events><Marker>m-2</Marker>",
                )
            }
            "DescribeEvents" => {
                assert!(body.ends_with("&Marker=m-2"), "{body}");
                query_ok("DescribeEvents", "<Events/>")
            }
            "ListSecrets" => {
                assert_eq!(body, "{}");
                json_ok(r#"{"SecretList": [], "NextToken": "t-2"}"#)
            }
            other => panic!("{other}"),
        }
    });
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let mut live = client(&transport, &signer, &sleeper);
    let base = format!(
        r#""SourceIdentifier": "prod-docdb", "SourceType": "db-cluster", "StartTime": "{EVENTS_START}", "EndTime": "{NOW}""#
    );
    let first = live
        .call(Service::Docdb, "DescribeEvents", &json(&format!("{{{base}}}")))
        .unwrap();
    assert_seam(
        &first,
        r#"{"Events": [{"Message": "Started cross AZ failover", "EventCategories": ["failover"]}], "Marker": "m-2"}"#,
    );
    let second = live
        .call(
            Service::Docdb,
            "DescribeEvents",
            &json(&format!(r#"{{{base}, "Marker": "m-2"}}"#)),
        )
        .unwrap();
    assert_seam(&second, r#"{"Events": []}"#);
    let secrets = live
        .call(Service::SecretsManager, "ListSecrets", &json("{}"))
        .unwrap();
    assert_seam(&secrets, r#"{"SecretList": [], "NextToken": "t-2"}"#);
    assert_eq!(transport.sends(), 3, "a generated paginator would send more");
}

#[test]
fn t_dg_04b_truncation_wrong_types_and_service_errors_never_succeed() {
    let cluster = r#"{"DBClusterIdentifier": "prod-docdb"}"#;
    let secrets = "{}";
    let alarms = r#"{"AlarmNamePrefix": "prod-docdb"}"#;
    let full_alarms = cbor::encode(&alarms_cbor());
    let cases: Vec<(Service, &str, &str, Reply)> = vec![
        // Query/XML.
        (
            Service::Docdb,
            "DescribeDBClusters",
            cluster,
            xml("<DescribeDBClustersResponse><DescribeDBClustersResult><DBClusters><DBCluster><BackupRetentionPeriod>7</BackupRe"),
        ),
        (
            Service::Docdb,
            "DescribeDBClusters",
            cluster,
            xml("<DescribeDBClustersResponse><DescribeDBClustersResult><DBClusters><DBCluster><BackupRetentionPeriod>7</BackupRetentionPeriod></DBCluster>"),
        ),
        (
            Service::Docdb,
            "DescribeDBClusters",
            cluster,
            query_ok(
                "DescribeDBClusters",
                "<DBClusters><DBCluster><BackupRetentionPeriod>seven</BackupRetentionPeriod></DBCluster></DBClusters>",
            ),
        ),
        (
            Service::Docdb,
            "DescribeDBClusters",
            cluster,
            query_ok(
                "DescribeDBClusters",
                "<DBClusters><DBCluster><StorageEncrypted>maybe</StorageEncrypted></DBCluster></DBClusters>",
            ),
        ),
        (Service::Docdb, "DescribeDBClusters", cluster, xml("")),
        (Service::Docdb, "DescribeDBClusters", cluster, xml("<Unrelated/>")),
        // AWS JSON.
        (
            Service::SecretsManager,
            "ListSecrets",
            secrets,
            json_ok(r#"{"SecretList": [{"Name": "a"}"#),
        ),
        (
            Service::SecretsManager,
            "ListSecrets",
            secrets,
            json_ok(r#"{"SecretList": "a"}"#),
        ),
        (Service::SecretsManager, "ListSecrets", secrets, json_ok("")),
        // RPC-v2 CBOR.
        (
            Service::CloudWatch,
            "DescribeAlarms",
            alarms,
            Ok((
                200,
                vec![("smithy-protocol", "rpc-v2-cbor".into())],
                full_alarms[..full_alarms.len() - 3].to_vec(),
            )),
        ),
        (
            Service::CloudWatch,
            "DescribeAlarms",
            alarms,
            cbor_reply(200, &cbor::map(vec![("MetricAlarms", cbor::text("x"))]), vec![]),
        ),
        (
            Service::CloudWatch,
            "DescribeAlarms",
            alarms,
            Ok((200, vec![("smithy-protocol", "rpc-v2-cbor".into())], Vec::new())),
        ),
    ];
    for (index, (service, operation, request, reply)) in cases.into_iter().enumerate() {
        let reply = std::cell::RefCell::new(Some(reply));
        let transport = Wire::new(|_: &HttpRequest, _| reply.borrow_mut().take().expect("one send"));
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let mut live = client(&transport, &signer, &sleeper);
        let result = live.call(service, operation, &json(request));
        let error = result.expect_err(&format!("case {index} {operation} became a success"));
        assert!(
            matches!(error, AwsError::InvalidResponse { .. }),
            "case {index} {operation}: {error:?}"
        );
        assert_eq!(transport.sends(), 1, "case {index}: malformed responses are not retried");
    }

    // Service errors are typed with a safe code and status; the service's
    // message (a canary) is never kept.
    let errors: Vec<(Service, &str, &str, Reply, u16, &str)> = vec![
        (Service::Docdb, "DescribeDBClusters", cluster, query_error(403, "AccessDenied"), 403, "AccessDenied"),
        (
            Service::Ec2,
            "DescribeSecurityGroups",
            r#"{"GroupIds": ["sg-0abc"]}"#,
            Ok((
                403,
                vec![("content-type", "text/xml".into())],
                format!("<Response><Errors><Error><Code>UnauthorizedOperation</Code><Message>{SECRET}</Message></Error></Errors><RequestID>req-err</RequestID></Response>").into_bytes(),
            )),
            403,
            "UnauthorizedOperation",
        ),
        (
            Service::SecretsManager,
            "ListSecrets",
            secrets,
            Ok((
                400,
                vec![("content-type", "application/x-amz-json-1.1".into())],
                format!(r#"{{"__type": "AccessDeniedException", "message": "{SECRET}"}}"#).into_bytes(),
            )),
            400,
            "AccessDeniedException",
        ),
        (
            Service::CloudWatch,
            "DescribeAlarms",
            alarms,
            cbor_reply(
                400,
                &cbor::map(vec![
                    ("__type", cbor::text("com.amazonaws.cloudwatch#InvalidNextToken")),
                    ("message", cbor::text(SECRET)),
                ]),
                vec![],
            ),
            400,
            "InvalidNextToken",
        ),
    ];
    for (service, operation, request, reply, status, code) in errors {
        let reply = std::cell::RefCell::new(Some(reply));
        let transport = Wire::new(|_: &HttpRequest, _| reply.borrow_mut().take().expect("one send"));
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let mut live = client(&transport, &signer, &sleeper);
        let error = live
            .call(service, operation, &json(request))
            .expect_err(operation);
        let AwsError::Service(detail) = &error else {
            panic!("{operation}: {error:?}")
        };
        assert_eq!(detail.status(), status, "{operation}");
        assert_eq!(detail.code(), Some(code), "{operation}");
        for shown in [format!("{error:?}"), error.to_string(), detail.message().to_string()] {
            assert!(!shown.contains(SECRET), "{operation}: {shown}");
        }
        assert_eq!(transport.sends(), 1, "{operation}: a denial is not retried");
    }
}

// ---- T-DG-04c: one retry owner, four sends ----

fn describe_cluster(live: &mut LiveAwsClient<'_>) -> Result<Json, AwsError> {
    live.call(
        Service::Docdb,
        "DescribeDBClusters",
        &json(r#"{"DBClusterIdentifier": "prod-docdb"}"#),
    )
}

fn secs(values: &[u64]) -> Vec<Duration> {
    values.iter().map(|v| Duration::from_secs(*v)).collect()
}

#[test]
fn t_dg_04c_transient_failures_recover_or_exhaust_at_four_sends() {
    // Recover on the fourth send after throttling, a 5xx and a connect
    // failure; then exhaust on four 503s and on four read timeouts.
    let recover = |_: &HttpRequest, n: usize| match n {
        0 => query_error(400, "Throttling"),
        1 => Ok((503, vec![], Vec::new())),
        2 => Err(AwsError::connect("fixture: connection reset")),
        _ => query_ok("DescribeDBClusters", CLUSTER_XML),
    };
    let exhaust_5xx = |_: &HttpRequest, _| Ok((503, vec![], Vec::new()));
    let exhaust_timeout = |_: &HttpRequest, _| {
        Err(AwsError::timeout(
            toolkit_aws::error::TimeoutPhase::Read,
            "fixture: no response",
        ))
    };
    let cbor_throttle = |_: &HttpRequest, n: usize| {
        if n < 3 {
            cbor_reply(
                400,
                &cbor::map(vec![("__type", cbor::text("Throttling"))]),
                vec![("x-amzn-query-error", "Throttling;Sender".into())],
            )
        } else {
            cbor_reply(200, &alarms_cbor(), vec![])
        }
    };

    let transport = Wire::new(recover);
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let got = describe_cluster(&mut client(&transport, &signer, &sleeper)).expect("recovered");
    assert_seam(&got, CLUSTER_SEAM);
    assert_eq!(transport.sends(), 4);
    assert_eq!(signer.count(), 4, "a fresh signature per attempt");
    assert_eq!(sleeper.sleeps(), secs(&[2, 4, 8]), "only the toolkit backs off");
    let signatures: std::collections::HashSet<String> = transport
        .requests()
        .iter()
        .map(|r| r.headers.get("authorization").unwrap().to_string())
        .collect();
    assert_eq!(signatures.len(), 4, "every attempt is signed afresh");

    for (name, route) in [
        ("5xx", Box::new(exhaust_5xx) as Box<dyn Fn(&HttpRequest, usize) -> Reply>),
        ("timeout", Box::new(exhaust_timeout)),
    ] {
        let transport = Wire::new(route);
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let error = describe_cluster(&mut client(&transport, &signer, &sleeper))
            .expect_err(name);
        assert!(error.is_retryable(), "{name}: {error:?}");
        assert_eq!(transport.sends(), 4, "{name}: exhausted at four sends");
        assert_eq!(signer.count(), 4, "{name}");
        assert_eq!(sleeper.sleeps(), secs(&[2, 4, 8]), "{name}");
    }

    let transport = Wire::new(cbor_throttle);
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let got = client(&transport, &signer, &sleeper)
        .call(
            Service::CloudWatch,
            "DescribeAlarms",
            &json(r#"{"AlarmNamePrefix": "prod-docdb"}"#),
        )
        .expect("CBOR throttling recovers");
    assert_seam(&got, ALARMS_SEAM);
    assert_eq!(transport.sends(), 4);
}

#[test]
fn t_dg_04c_denial_malformed_and_credential_failures_send_once_or_never() {
    let transport = Wire::new(|_: &HttpRequest, _| query_error(403, "AccessDenied"));
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    describe_cluster(&mut client(&transport, &signer, &sleeper)).expect_err("denied");
    assert_eq!((transport.sends(), sleeper.sleeps().len()), (1, 0));

    let transport = Wire::new(|_: &HttpRequest, _| xml("<DescribeDBClustersResponse><Desc"));
    let sleeper = RecordingSleeper::new();
    describe_cluster(&mut client(&transport, &signer, &sleeper)).expect_err("malformed");
    assert_eq!((transport.sends(), sleeper.sleeps().len()), (1, 0));

    for error in [
        AwsError::NoCredentials,
        AwsError::CredentialsExpired,
        AwsError::timeout(
            toolkit_aws::error::TimeoutPhase::Deadline,
            "credential resolution did not finish within its time budget; nothing was sent",
        ),
    ] {
        let transport = Wire::new(|_: &HttpRequest, _| panic!("unsigned requests are never sent"));
        let failing = FailingSigner {
            error: error.clone(),
            calls: Cell::new(0),
        };
        let sleeper = RecordingSleeper::new();
        let mut live =
            LiveAwsClient::new(REGION, &transport, &failing, &sleeper, &NoJitter).expect("client");
        let got = describe_cluster(&mut live).expect_err("no credentials");
        assert_eq!(got.code(), error.code(), "{got:?}");
        assert_eq!(transport.sends(), 0);
        assert_eq!(failing.calls.get(), 1, "no retry of a credential refusal");
        assert!(sleeper.sleeps().is_empty());
    }
}

#[test]
fn t_dg_04c_the_operation_deadline_stops_retries() {
    let transport = Wire::new(|_: &HttpRequest, _| Ok((503, vec![], Vec::new())));
    let signer = ToolkitSigner::new();
    let started = Instant::now();
    let mut live = LiveAwsClient::new(REGION, &transport, &signer, &ThreadSleeper, &NoJitter)
        .expect("client")
        .with_bounds(ReviewBounds {
            operation: Duration::from_millis(200),
            ..ReviewBounds::default()
        });
    let error = describe_cluster(&mut live).expect_err("deadline");
    assert!(
        matches!(
            error,
            AwsError::Timeout {
                phase: toolkit_aws::error::TimeoutPhase::Deadline,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(transport.sends(), 1, "the backoff ran into the deadline");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    let (_, limits) = &transport.sent.borrow()[0];
    let deadline = limits.deadline().expect("deadline");
    assert!(deadline <= started + Duration::from_millis(200));
}

// ---- T-DG-04d: Secrets Manager metadata only ----

#[test]
fn t_dg_04d_a_live_review_only_lists_secret_metadata() {
    let transport = Wire::new(healthy_wire);
    let signer = ToolkitSigner::new();
    let sleeper = RecordingSleeper::new();
    let _ = run_infra_checks(
        &mut client(&transport, &signer, &sleeper),
        "prod-docdb",
        NOW_SECONDS,
    );
    let secrets: Vec<HttpRequest> = transport
        .requests()
        .into_iter()
        .filter(|r| r.url.starts_with("https://secretsmanager."))
        .collect();
    assert_eq!(secrets.len(), 1);
    for request in secrets {
        assert_eq!(
            request.headers.get("x-amz-target"),
            Some("secretsmanager.ListSecrets")
        );
        assert!(!String::from_utf8_lossy(&request.body).contains("SecretId"));
    }
    assert!(OPERATIONS
        .iter()
        .filter(|op| op.service == Service::SecretsManager)
        .all(|op| op.name == "ListSecrets"));
}

#[test]
fn t_dg_04d_repeated_tokens_and_the_page_cap_still_fail_the_check() {
    let unmatched = r#""SecretList": [{"Name": "other", "Description": "unrelated"}]"#;
    let repeat = |_: &HttpRequest, _| json_ok(&format!(r#"{{{unmatched}, "NextToken": "t-1"}}"#));
    let endless =
        |_: &HttpRequest, n: usize| json_ok(&format!(r#"{{{unmatched}, "NextToken": "t-{n}"}}"#));
    for (name, route, sends) in [
        ("repeated", Box::new(repeat) as Box<dyn Fn(&HttpRequest, usize) -> Reply>, 2),
        ("page cap", Box::new(endless), 1000),
    ] {
        let transport = Wire::new(move |request: &HttpRequest, n| {
            if action(request) == "ListSecrets" {
                route(request, n)
            } else {
                healthy_wire(request, n)
            }
        });
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let findings = run_infra_checks(
            &mut client(&transport, &signer, &sleeper),
            "prod-docdb",
            NOW_SECONDS,
        );
        let sec3: Vec<_> = findings.checks.iter().filter(|c| c.id == "SEC3").collect();
        assert_eq!(sec3.len(), 1, "{name}");
        assert!(
            sec3[0].label.starts_with("Cannot check Secrets Manager: "),
            "{name}: {}",
            sec3[0].label
        );
        let listed = transport
            .requests()
            .iter()
            .filter(|r| action(r) == "ListSecrets")
            .count();
        assert_eq!(listed, sends, "{name}");
    }
}

// ---- T-DG-04e: the bridge itself ----

#[test]
fn t_dg_04e_generated_framing_is_removed_and_the_url_and_body_kept() {
    for (service, operation, request, url, _, _, body) in encodings() {
        let op = live::operation(service, operation).expect("allowed");
        let generated =
            live::generated_request(op, REGION, &json(&request)).expect("generated request");
        let names: Vec<String> = generated
            .headers()
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        assert!(
            names.iter().any(|n| n == "content-length"),
            "{operation}: the SDK generated no content-length to remove: {names:?}"
        );
        let bridged = live::toolkit_request(op, REGION, &generated, toolkit_aws::Timeouts::default())
            .expect("bridged");
        assert_eq!(bridged.url, url, "{operation}");
        assert_eq!(bridged.body, body, "{operation}");
        assert_eq!(generated.body().bytes(), Some(body.as_slice()), "{operation}");
        for name in FRAMING_HEADERS {
            assert!(bridged.headers.get(name).is_none(), "{operation}: {name}");
        }
        // Every semantic header the SDK generated is kept unchanged.
        for (name, value) in generated.headers() {
            if !FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                assert_eq!(bridged.headers.get(name), Some(value), "{operation}: {name}");
            }
        }
    }
}

#[test]
fn t_dg_04e_the_bridge_refuses_sdk_signing_streaming_bodies_and_foreign_urls() {
    use aws_smithy_types::body::SdkBody;
    let op = live::operation(Service::Docdb, "DescribeDBClusters").expect("allowed");
    let generated = |url: &str, headers: &[(&str, &str)], body: SdkBody| {
        let mut request = aws_smithy_runtime_api::client::orchestrator::HttpRequest::new(body);
        request.set_method("POST").unwrap();
        request.set_uri(url).unwrap();
        for (name, value) in headers {
            let _ = request
                .headers_mut()
                .insert(name.to_string(), value.to_string());
        }
        request
    };
    let body = || SdkBody::from("Action=DescribeDBClusters&Version=2014-10-31");
    let ok = generated(
        "https://rds.us-east-1.amazonaws.com/",
        &[("content-type", "application/x-www-form-urlencoded"), ("content-length", "44")],
        body(),
    );
    live::toolkit_request(op, REGION, &ok, toolkit_aws::Timeouts::default()).expect("baseline");
    // An SDK-prepared signature means noAuth was not selected: fail closed.
    for header in ["authorization", "x-amz-date", "x-amz-security-token", "x-amz-content-sha256"] {
        let signed = generated(
            "https://rds.us-east-1.amazonaws.com/",
            &[(header, "FAKE-SDK-SIGNATURE")],
            body(),
        );
        let error = live::toolkit_request(op, REGION, &signed, toolkit_aws::Timeouts::default())
            .expect_err(header);
        assert!(!format!("{error:?}").contains("FAKE-SDK-SIGNATURE"));
    }
    // Any URL other than the operation's own endpoint is refused.
    for url in [
        "http://rds.us-east-1.amazonaws.com/",
        "https://rds.us-west-2.amazonaws.com/",
        "https://user:FAKE-PASS@rds.us-east-1.amazonaws.com/",
        "https://rds.us-east-1.amazonaws.com:8443/",
        "https://rds.us-east-1.amazonaws.com/other",
        "https://rds.us-east-1.amazonaws.com/?Action=DeleteDBCluster",
        "https://169.254.169.254/",
        "https://monitoring.us-east-1.amazonaws.com/",
    ] {
        let foreign = generated(url, &[], body());
        let error = live::toolkit_request(op, REGION, &foreign, toolkit_aws::Timeouts::default())
            .expect_err(url);
        assert!(!format!("{error:?}").contains("FAKE-PASS"), "{url}");
    }
    // A streaming body cannot be signed as final bytes: refused.
    let streaming_request =
        generated("https://rds.us-east-1.amazonaws.com/", &[], SdkBody::taken());
    assert!(streaming_request.body().bytes().is_none());
    live::toolkit_request(op, REGION, &streaming_request, toolkit_aws::Timeouts::default())
        .expect_err("streaming body");
}

#[test]
fn t_dg_04e_no_sdk_credential_identity_is_ever_accepted() {
    use aws_smithy_runtime_api::client::identity::Identity;
    let credentials = aws_credential_types::Credentials::new(
        "AKIDFIXTUREEXAMPLE",
        "fixture-secret-key",
        None,
        None,
        "fixture",
    );
    let error = refuse_credential_identity(&Identity::from(credentials)).expect_err("refused");
    assert!(!format!("{error:?}").contains("fixture-secret-key"));
    refuse_credential_identity(&Identity::new((), None)).expect("the noAuth identity");
}

#[test]
fn t_dg_04e_only_toolkit_signing_and_retries_run_for_every_operation() {
    // Every allowed operation through the bridge on a 503: the SDK makes one
    // connector call per toolkit attempt (its own retries are off), the
    // toolkit signer runs once per send, and the SDK never signs (the bridge
    // would refuse an SDK-prepared signature).
    for (service, operation, request, ..) in encodings() {
        let transport = Wire::new(|_: &HttpRequest, _| Ok((503, vec![], Vec::new())));
        let signer = ToolkitSigner::new();
        let sleeper = RecordingSleeper::new();
        let error = client(&transport, &signer, &sleeper)
            .call(service, operation, &json(&request))
            .expect_err(operation);
        let AwsError::Service(detail) = &error else {
            panic!("{operation}: {error:?}")
        };
        assert_eq!(detail.status(), 503, "{operation}");
        assert_eq!(transport.sends(), 4, "{operation}: SDK retries would add sends");
        assert_eq!(signer.count(), 4, "{operation}");
        assert_eq!(sleeper.sleeps(), secs(&[2, 4, 8]), "{operation}");
    }
}
