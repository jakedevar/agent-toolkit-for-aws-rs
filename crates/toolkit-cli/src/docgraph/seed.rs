//! `toolkit neptune seed` (port of the Neptune skill's `seed_test_data.py`):
//! seeds a throwaway Neptune test cluster with the social, fraud and product
//! graphs used by the skill's evals.
//!
//! The script opened a WebSocket Gremlin client (`gremlin_python`). Here the
//! statements are data ([`phases`]) sent through a [`GremlinSession`]; the
//! command wires a [`RecordedSession`] (`--recorded FILE`) or
//! [`NoLiveSession`], which sends nothing. `--dry-run` prints the statements
//! instead of sending them. A live session over the Gremlin WebSocket is a
//! follow-up (`docs/migration/docgraph.md`).
//!
//! **The wipe guard is kept and runs first.** `g.V().drop()` empties the whole
//! graph and is on the skill's Never-Auto-Execute list
//! (`references/action-safety.md`). It is sent only after `--confirm-wipe` or
//! `NEPTUNE_SEED_CONFIRM_WIPE=1`; without either, the command fails before any
//! session call, exactly as `clear_graph` raised before any query.
//!
//! # Recorded file
//!
//! `{"calls": [{"request": {"gremlin": "...", "bindings": {...}},
//! "response": {"results": [...]}}, ...]}`: one entry per statement, in the
//! order they are sent. An entry without a `request` accepts any statement.
//! The requests are compared before the response is replayed (a failure entry
//! included), a statement past the last entry fails, and a recording with
//! unused entries is rejected after the run.

use toolkit_aws::recorded::{
    optional_list, optional_text, step_fields, KeyOrder, RecordedOutcome, ReplayError, Step,
    StepQueue, StepShapeError,
};
use toolkit_aws::{AwsError, ServiceError};
use toolkit_core::Json;

use super::checks::clean;

/// The Gremlin port of Neptune.
pub(super) const PORT: u16 = 8182;

/// One statement and the parameter bindings it is sent with.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Statement {
    pub(super) gremlin: String,
    pub(super) bindings: Option<Json>,
}

impl Statement {
    fn plain(gremlin: &str) -> Self {
        Self {
            gremlin: gremlin.to_string(),
            bindings: None,
        }
    }

    fn bound(gremlin: &str, bindings: Vec<(&str, Json)>) -> Self {
        Self {
            gremlin: gremlin.to_string(),
            bindings: Some(Json::object(bindings)),
        }
    }

    /// The request a [`GremlinSession`] receives.
    pub(super) fn request(&self) -> Json {
        let mut members = vec![("gremlin", Json::string(self.gremlin.clone()))];
        if let Some(bindings) = &self.bindings {
            members.push(("bindings", bindings.clone()));
        }
        Json::object(members)
    }
}

/// Sends statements to a Neptune graph and returns the `results` list.
pub(super) trait GremlinSession {
    /// # Errors
    ///
    /// The failure of the call.
    fn submit(&mut self, request: &Json) -> Result<Vec<Json>, AwsError>;
}

/// The session of a run without `--recorded`: nothing is sent.
pub(super) struct NoLiveSession;

impl GremlinSession for NoLiveSession {
    fn submit(&mut self, _: &Json) -> Result<Vec<Json>, AwsError> {
        Err(AwsError::connect(
            "this build includes no live Gremlin client; pass --recorded FILE to replay recorded responses or --dry-run to print the statements",
        ))
    }
}

/// Replays a recorded file.
pub(super) struct RecordedSession {
    queue: StepQueue<()>,
}

impl RecordedSession {
    /// # Errors
    ///
    /// A reason naming the entry index and the wrong member, never a value.
    pub(super) fn from_document(document: &Json) -> Result<Self, String> {
        let calls = document
            .get("calls")
            .and_then(super::snapshot::list)
            .ok_or_else(|| "the recorded responses need a \"calls\" array".to_string())?;
        let mut queue = StepQueue::new(KeyOrder::Ignored);
        for (index, call) in calls.iter().enumerate() {
            let bad = |what: &str| format!("recorded call {index}: {what}");
            let fields = step_fields(call).map_err(|error| match error {
                StepShapeError::Request(member) => bad(&member.to_string()),
                StepShapeError::Outcome | StepShapeError::ErrorNotObject => bad(
                    "exactly one of \"response\" (an object) and \"error\" (an object) is required",
                ),
            })?;
            let request = fields.request.cloned();
            let step = match fields.outcome {
                RecordedOutcome::Response(response) => {
                    // The shape is validated up front so a bad response is
                    // rejected with the recording, not in the middle of a run.
                    optional_list(response, "results")
                        .map_err(|error| bad(&format!("response {error}")))?;
                    Step::response(request, response.clone())
                }
                RecordedOutcome::Error(error) => {
                    let message = optional_text(error, "message")
                        .map_err(|member| bad(&format!("error {member}")))?
                        .unwrap_or_default();
                    Step::failure(
                        request,
                        AwsError::Service(Box::new(ServiceError::new(
                            "POST", "/gremlin", 400, None, message, None,
                        ))),
                    )
                }
            };
            queue.push((), step);
        }
        Ok(Self { queue })
    }

    /// Recorded entries the run never used.
    pub(super) fn unused(&self) -> usize {
        self.queue.unconsumed()
    }
}

impl GremlinSession for RecordedSession {
    fn submit(&mut self, request: &Json) -> Result<Vec<Json>, AwsError> {
        match self.queue.replay(&(), request) {
            Ok(Ok(response)) => Ok(optional_list(&response, "results")
                .map_err(|error| AwsError::invalid_response(error.to_string()))?
                .to_vec()),
            Ok(Err(error)) => Err(error),
            Err(ReplayError::Exhausted) => Err(AwsError::connect(
                "no recorded response left for this statement",
            )),
            Err(ReplayError::RequestMismatch) => Err(AwsError::connect(
                "the statement differs from the recorded request",
            )),
        }
    }
}

const USERS: [(&str, &str, i64); 5] = [
    ("U1", "Alice", 30),
    ("U2", "Bob", 28),
    ("U3", "Carol", 35),
    ("U4", "Dave", 25),
    ("U5", "Eve", 32),
];
const FOLLOWS: [(&str, &str); 5] = [
    ("U1", "U2"),
    ("U1", "U3"),
    ("U2", "U4"),
    ("U3", "U4"),
    ("U4", "U5"),
];
const ACCOUNTS: [(&str, &str); 4] = [
    ("A1", "clean"),
    ("A2", "flagged"),
    ("A3", "unknown"),
    ("A4", "clean"),
];
const IDENTIFIERS: [(&str, &str, &str); 3] = [
    ("E1", "email", "shared@example.com"),
    ("P1", "phone", "+15551234567"),
    ("D1", "device", "device-abc-123"),
];
const USES: [(&str, &str); 5] = [
    ("A1", "E1"),
    ("A2", "E1"),
    ("A2", "P1"),
    ("A3", "P1"),
    ("A4", "D1"),
];
const PRODUCTS: [(&str, &str, f64); 3] = [
    ("P1", "Widget", 29.99),
    ("P2", "Gadget", 49.99),
    ("P3", "Doohickey", 9.99),
];
const PURCHASES: [(&str, &str); 4] = [("U1", "P1"), ("U2", "P1"), ("U2", "P2"), ("U3", "P3")];

/// The wipe statement of `clear_graph`.
pub(super) const WIPE: &str = "g.V().drop()";

fn s(value: &str) -> Json {
    Json::string(value)
}

/// One seeding phase: its banner, statements and summary line.
pub(super) struct Phase {
    pub(super) banner: &'static str,
    pub(super) statements: Vec<Statement>,
    pub(super) summary: String,
}

/// The three seeding phases, in the order of the script.
pub(super) fn phases() -> Vec<Phase> {
    let mut social = Vec::new();
    for (uid, name, age) in USERS {
        social.push(Statement::bound(
            "g.addV('Person').property('id', uid).property('name', name).property('age', age)",
            vec![("uid", s(uid)), ("name", s(name)), ("age", Json::Int(age))],
        ));
    }
    for (src, dst) in FOLLOWS {
        social.push(Statement::bound(
            "g.V().has('Person','id',src).addE('FOLLOWS').to(g.V().has('Person','id',dst))",
            vec![("src", s(src)), ("dst", s(dst))],
        ));
    }

    let mut fraud = Vec::new();
    for (aid, status) in ACCOUNTS {
        fraud.push(Statement::bound(
            "g.addV('Account').property('id', aid).property('status', status)",
            vec![("aid", s(aid)), ("status", s(status))],
        ));
    }
    for (iid, itype, value) in IDENTIFIERS {
        fraud.push(Statement::bound(
            "g.addV('Identifier').property('id', iid).property('type', itype).property('value', value)",
            vec![("iid", s(iid)), ("itype", s(itype)), ("value", s(value))],
        ));
    }
    for (aid, iid) in USES {
        fraud.push(Statement::bound(
            "g.V().has('Account','id',aid).addE('USES').to(g.V().has('Identifier','id',iid))",
            vec![("aid", s(aid)), ("iid", s(iid))],
        ));
    }

    let mut product = Vec::new();
    for (pid, name, price) in PRODUCTS {
        product.push(Statement::bound(
            "g.addV('Product').property('id', pid).property('name', name).property('price', price)",
            vec![
                ("pid", s(pid)),
                ("name", s(name)),
                ("price", Json::Float(price)),
            ],
        ));
    }
    for (uid, pid) in PURCHASES {
        product.push(Statement::bound(
            "g.V().has('Person','id',uid).addE('PURCHASED').to(g.V().has('Product','id',pid))",
            vec![("uid", s(uid)), ("pid", s(pid))],
        ));
    }

    vec![
        Phase {
            banner: "Seeding social graph...",
            statements: social,
            summary: format!(
                "  Created {} users, {} follows edges",
                USERS.len(),
                FOLLOWS.len()
            ),
        },
        Phase {
            banner: "Seeding fraud graph...",
            statements: fraud,
            summary: format!(
                "  Created {} accounts, {} identifiers, {} USES edges",
                ACCOUNTS.len(),
                IDENTIFIERS.len(),
                USES.len()
            ),
        },
        Phase {
            banner: "Seeding product graph...",
            statements: product,
            summary: format!(
                "  Created {} products, {} PURCHASED edges",
                PRODUCTS.len(),
                PURCHASES.len()
            ),
        },
    ]
}

const VERTEX_COUNT: &str = "g.V().count()";
const EDGE_COUNT: &str = "g.E().count()";
const FRAUD_RING: &str =
    "g.V().has('Account','id','A2').out('USES').in('USES').dedup().values('id').toList()";
const FRIENDS_OF_FRIENDS: &str =
    "g.V().has('Person','id','U1').out('FOLLOWS').out('FOLLOWS').dedup().values('name').toList()";

/// Why a seeding run stopped.
#[derive(Debug)]
pub(super) enum SeedError {
    /// The wipe was not confirmed; nothing was sent.
    WipeNotConfirmed,
    /// A call failed.
    Call(AwsError),
    /// A verification query returned an unexpected result.
    Verification(&'static str),
}

impl From<AwsError> for SeedError {
    fn from(error: AwsError) -> Self {
        Self::Call(error)
    }
}

/// The text of the wipe refusal (the `RuntimeError` of `clear_graph`).
pub(super) const WIPE_REFUSAL: &str = "Refusing to wipe the graph: g.V().drop() requires explicit confirmation. Re-run with --confirm-wipe or set NEPTUNE_SEED_CONFIRM_WIPE=1. Never run this against a graph holding data you need.";

/// A result item as Python `str()` prints it, cleaned for the terminal.
fn py_str(item: &Json) -> String {
    match item {
        Json::String(text) => clean(text),
        Json::Int(n) => n.to_string(),
        Json::Float(n) => format!("{n:?}"),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Null => "None".to_string(),
        other => clean(&other.to_compact_string()),
    }
}

/// A result list as Python `repr(list)` prints it, cleaned for the terminal.
fn py_list(items: &[Json]) -> String {
    let rendered: Vec<String> = items
        .iter()
        .map(|item| match item {
            Json::String(text) => format!("'{}'", clean(text).replace('\'', "\\'")),
            other => py_str(other),
        })
        .collect();
    format!("[{}]", rendered.join(", "))
}

/// Runs the seeding the script ran: wipe, three seeds, verification. `line`
/// receives each stdout line.
///
/// # Errors
///
/// [`SeedError::WipeNotConfirmed`] before any call; the first failing call;
/// a verification mismatch.
pub(super) fn run(
    session: &mut dyn GremlinSession,
    endpoint: &str,
    confirmed: bool,
    line: &mut dyn FnMut(&str),
) -> Result<(), SeedError> {
    line(&format!("Connecting to Neptune at {endpoint}:{PORT}..."));
    if !confirmed {
        return Err(SeedError::WipeNotConfirmed);
    }
    line(&format!("Clearing ALL graph data at {endpoint}..."));
    session.submit(&Statement::plain(WIPE).request())?;
    for phase in phases() {
        line(phase.banner);
        for statement in &phase.statements {
            session.submit(&statement.request())?;
        }
        line(&phase.summary);
    }
    verify(session, line)?;
    line("");
    line("Seed complete. Neptune test cluster is ready for eval runs.");
    line(&format!("Endpoint: wss://{endpoint}:{PORT}/gremlin"));
    Ok(())
}

fn first(results: &[Json]) -> Result<&Json, SeedError> {
    results.first().ok_or_else(|| {
        SeedError::Call(AwsError::invalid_response(
            "a count query returned no result",
        ))
    })
}

fn verify(session: &mut dyn GremlinSession, line: &mut dyn FnMut(&str)) -> Result<(), SeedError> {
    line("");
    line("Verifying seeded data...");
    let vertices = session.submit(&Statement::plain(VERTEX_COUNT).request())?;
    let edges = session.submit(&Statement::plain(EDGE_COUNT).request())?;
    line(&format!("  Vertices: {}", py_str(first(&vertices)?)));
    line(&format!("  Edges: {}", py_str(first(&edges)?)));

    let ring = session.submit(&Statement::plain(FRAUD_RING).request())?;
    line(&format!(
        "  Accounts sharing identifiers with A2: {}",
        py_list(&ring)
    ));
    if ring.len() < 2 {
        return Err(SeedError::Verification(
            "Fraud ring query returned unexpected results",
        ));
    }

    let friends = session.submit(&Statement::plain(FRIENDS_OF_FRIENDS).request())?;
    line(&format!(
        "  Friends-of-friends for Alice: {}",
        py_list(&friends)
    ));
    if friends.is_empty() {
        return Err(SeedError::Verification(
            "Friends-of-friends query returned unexpected results",
        ));
    }
    line("");
    line("\u{2705} Seed data verified successfully");
    Ok(())
}

/// The statements `--dry-run` prints: the wipe, then every phase.
pub(super) fn dry_run_lines() -> Vec<String> {
    let mut lines = vec![
        "Dry run: nothing is sent to Neptune.".to_string(),
        format!("1. [destructive, needs --confirm-wipe] {WIPE}"),
    ];
    let mut number = 2;
    for phase in phases() {
        for statement in &phase.statements {
            let bindings = statement
                .bindings
                .as_ref()
                .map(|bindings| format!(" {}", bindings.to_compact_string()))
                .unwrap_or_default();
            lines.push(format!("{number}. {}{bindings}", statement.gremlin));
            number += 1;
        }
    }
    for query in [VERTEX_COUNT, EDGE_COUNT, FRAUD_RING, FRIENDS_OF_FRIENDS] {
        lines.push(format!("{number}. {query}"));
        number += 1;
    }
    lines
}
