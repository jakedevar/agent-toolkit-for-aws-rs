//! `tenant_query.py`: tenant isolation, the structural read-only surface and
//! the cursors.

use std::marker::PhantomData;

use toolkit_core::Json;

use super::super::tenant::{
    select_by_tenant, DbError, NoLiveCursor, RecordedCursor, Row, SqlCursor, TenantQueryError,
    TenantRead, TENANT_READ_TEMPLATE,
};
use super::json;

/// A scripted cursor that records every statement it is asked to run.
#[derive(Default)]
struct Spy {
    executed: Vec<String>,
    rows: Vec<Row>,
    fail: Option<String>,
}

impl SqlCursor for Spy {
    fn execute(&mut self, statement: &TenantRead) -> Result<(), DbError> {
        self.executed.push(statement.sql().to_string());
        match &self.fail {
            Some(message) => Err(DbError(message.clone())),
            None => Ok(()),
        }
    }

    fn fetchall(&mut self) -> Result<Vec<Row>, DbError> {
        Ok(self.rows.clone())
    }
}

const ENTITIES_ACME: &str = "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'";

/// The template statement for a valid tenant, spelled out independently of
/// the code under test.
fn template_for(tenant: &str) -> String {
    format!("SELECT * FROM \"entities\" WHERE tenant_id = '{tenant}'")
}

fn tenant_sql(tenant: &str) -> Result<String, String> {
    TenantRead::for_tenant(tenant)
        .map(|statement| statement.sql().to_string())
        .map_err(|error| error.to_string())
}

#[test]
fn the_tenant_filter_is_always_in_the_statement() {
    assert_eq!(tenant_sql("acme").expect("sql"), ENTITIES_ACME);
    assert_eq!(
        TENANT_READ_TEMPLATE,
        "SELECT * FROM {t} WHERE tenant_id = {tid}"
    );
    for tenant in ["a", "tenant-2", &"z".repeat(64)] {
        let sql = tenant_sql(tenant).expect("sql");
        assert!(
            sql.ends_with(&format!("WHERE tenant_id = '{tenant}'")),
            "{sql}"
        );
        assert_eq!(sql.matches("tenant_id").count(), 1);
    }
    let read = TenantRead::for_tenant("acme").expect("read");
    assert_eq!(read.to_string(), ENTITIES_ACME);
}

#[test]
fn select_by_tenant_runs_one_select_and_returns_the_rows() {
    let mut cursor = Spy {
        rows: vec![vec![Json::string("e1"), Json::string("acme")]],
        ..Spy::default()
    };
    let rows = select_by_tenant(&mut cursor, &Json::string("acme")).expect("rows");
    assert_eq!(cursor.executed, [ENTITIES_ACME]);
    assert_eq!(rows, [[Json::string("e1"), Json::string("acme")]]);
}

#[test]
fn an_injection_attempt_never_reaches_the_cursor() {
    let mut cursor = Spy::default();
    for hostile in [
        "acme' OR '1'='1",
        "acme'; DROP TABLE entities; --",
        "ACME",
        "",
        "acme\n",
        "acme\\",
        "*",
        "%",
        "acme or 1=1",
        "../acme",
        "acme\u{1b}[31m",
    ] {
        let error = select_by_tenant(&mut cursor, &Json::string(hostile)).expect_err(hostile);
        assert!(matches!(error, TenantQueryError::Unsafe(_)), "{hostile:?}");
    }
    assert!(cursor.executed.is_empty(), "{:?}", cursor.executed);
}

#[test]
fn a_tenant_that_is_not_a_string_is_rejected_by_python_type_name() {
    let mut cursor = Spy::default();
    for (value, name) in [
        (Json::Null, "NoneType"),
        (Json::Int(7), "int"),
        (Json::Float(1.5), "float"),
        (Json::Bool(true), "bool"),
        (Json::Array(vec![Json::string("acme")]), "list"),
        (Json::Object(vec![]), "dict"),
    ] {
        let error = select_by_tenant(&mut cursor, &value).expect_err(name);
        assert_eq!(error, TenantQueryError::NotAString(name));
        assert_eq!(
            error.to_string(),
            format!("req.tenant must be a str, got {name}")
        );
    }
    assert!(cursor.executed.is_empty());
}

#[test]
fn a_cursor_failure_is_reported_and_is_not_a_validation_error() {
    let mut cursor = Spy {
        fail: Some("connection reset".to_string()),
        ..Spy::default()
    };
    let error = select_by_tenant(&mut cursor, &Json::string("acme")).expect_err("fails");
    assert_eq!(
        error,
        TenantQueryError::Database(DbError("connection reset".into()))
    );
    assert_eq!(cursor.executed.len(), 1);
}

#[test]
fn tenants_are_isolated_each_statement_names_only_its_tenant() {
    let mut cursor = Spy::default();
    select_by_tenant(&mut cursor, &Json::string("tenant-a")).expect("a");
    select_by_tenant(&mut cursor, &Json::string("tenant-b")).expect("b");
    assert!(cursor.executed[0].contains("'tenant-a'") && !cursor.executed[0].contains("tenant-b"));
    assert!(cursor.executed[1].contains("'tenant-b'") && !cursor.executed[1].contains("tenant-a"));
}

/// Statements that must never reach a cursor: writes, stacked statements and
/// the write-capable `SELECT` forms of #4738 that a `SELECT`-prefix filter let
/// through.
const NOT_A_TENANT_READ: &[&str] = &[
    "",
    "DELETE FROM entities",
    "DELETE FROM entities WHERE tenant_id = 'acme'",
    "UPDATE entities SET name = 'x'",
    "INSERT INTO entities VALUES (1)",
    "DROP TABLE entities",
    "WITH x AS (SELECT 1) DELETE FROM entities",
    "SELECT 1; DROP TABLE entities",
    "SELECT 1;",
    "SELECT 1\0",
    "/* SELECT */ DELETE FROM t",
    "EXPLAIN ANALYZE DELETE FROM t",
    "SELECT * INTO copied_entities FROM entities",
    "SELECT nextval('tenant_counter')",
    "SELECT setval('tenant_counter', 1)",
    "SELECT * FROM entities FOR UPDATE",
    "SELECT * FROM \"entities\" WHERE tenant_id = 'acme' FOR UPDATE",
    "WITH d AS (DELETE FROM entities RETURNING *) SELECT * FROM d",
    "SELECT pg_terminate_backend(1)",
    "SELECT lo_import('/etc/passwd')",
    "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'",
];

#[test]
fn only_template_statements_for_valid_tenants_reach_the_inner_cursor() {
    let long = "9".repeat(64);
    let valid = [
        "a",
        "0",
        "-",
        "acme",
        "tenant-a",
        "tenant-b",
        "a-b-c-1-2-3",
        "--",
        long.as_str(),
    ];
    let mut cursor = Spy::default();
    for tenant in valid {
        select_by_tenant(&mut cursor, &Json::string(tenant)).expect(tenant);
    }
    // Every SQL text a hostile caller could try is offered as the only input
    // the public surface takes, the tenant: none of it reaches the cursor.
    for hostile in NOT_A_TENANT_READ {
        let error = select_by_tenant(&mut cursor, &Json::string(*hostile)).expect_err(hostile);
        assert!(matches!(error, TenantQueryError::Unsafe(_)), "{hostile:?}");
        assert!(TenantRead::for_tenant(hostile).is_err(), "{hostile:?}");
    }
    for invalid in [
        "A",
        "acme ",
        " acme",
        "x".repeat(65).as_str(),
        "t\u{e9}",
        "a_b",
    ] {
        assert!(
            select_by_tenant(&mut cursor, &Json::string(invalid)).is_err(),
            "{invalid:?}"
        );
    }
    let expected: Vec<String> = valid.iter().map(|tenant| template_for(tenant)).collect();
    assert_eq!(cursor.executed, expected);
    // Each executed statement is exactly the template around one slug.
    for sql in &cursor.executed {
        let slug = sql
            .strip_prefix("SELECT * FROM \"entities\" WHERE tenant_id = '")
            .and_then(|rest| rest.strip_suffix('\''))
            .expect("template shape");
        assert!(
            (1..=64).contains(&slug.len())
                && slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{sql}"
        );
    }
}

// The cursor's input type cannot be made from text. These probes resolve at
// compile time: the inherent constant exists only when the bound holds, so
// the trait default (`false`) is used otherwise. The assertions are const
// blocks: if `TenantRead` ever gains a conversion from text, the tests stop
// compiling.
trait Fallback {
    const FROM_STRING: bool = false;
    const FROM_STR_REF: bool = false;
    const PARSE: bool = false;
    const DEFAULT: bool = false;
}

struct FromStringProbe<T>(PhantomData<T>);
impl<T> Fallback for FromStringProbe<T> {}
impl<T: From<String>> FromStringProbe<T> {
    const FROM_STRING: bool = true;
}

struct FromStrRefProbe<T>(PhantomData<T>);
impl<T> Fallback for FromStrRefProbe<T> {}
impl<T: for<'a> From<&'a str>> FromStrRefProbe<T> {
    const FROM_STR_REF: bool = true;
}

struct ParseProbe<T>(PhantomData<T>);
impl<T> Fallback for ParseProbe<T> {}
impl<T: std::str::FromStr> ParseProbe<T> {
    const PARSE: bool = true;
}

struct DefaultProbe<T>(PhantomData<T>);
impl<T> Fallback for DefaultProbe<T> {}
impl<T: Default> DefaultProbe<T> {
    const DEFAULT: bool = true;
}

#[test]
fn a_tenant_read_cannot_be_made_from_text() {
    // The probes detect the conversions on a type that has them.
    const { assert!(FromStringProbe::<String>::FROM_STRING) };
    const { assert!(FromStrRefProbe::<String>::FROM_STR_REF) };
    const { assert!(ParseProbe::<String>::PARSE) };
    const { assert!(DefaultProbe::<String>::DEFAULT) };
    // TenantRead has none of them; its field is private to `tenant.rs`, so
    // `TenantRead::for_tenant` is the only constructor.
    const { assert!(!FromStringProbe::<TenantRead>::FROM_STRING) };
    const { assert!(!FromStrRefProbe::<TenantRead>::FROM_STR_REF) };
    const { assert!(!ParseProbe::<TenantRead>::PARSE) };
    const { assert!(!DefaultProbe::<TenantRead>::DEFAULT) };
}

#[test]
fn the_no_live_cursor_fails_every_call_without_a_connection() {
    let mut cursor = NoLiveCursor;
    let read = TenantRead::for_tenant("acme").expect("read");
    let error = cursor.execute(&read).expect_err("no live connection");
    assert!(
        error.0.contains("no live Aurora DSQL connection"),
        "{error}"
    );
    assert!(cursor.fetchall().is_err());
    let error = select_by_tenant(&mut NoLiveCursor, &Json::string("acme")).expect_err("fails");
    assert!(matches!(error, TenantQueryError::Database(_)));
}

fn recorded(text: &str) -> Result<RecordedCursor, String> {
    RecordedCursor::from_document(&json(text))
}

#[test]
fn a_recorded_write_is_only_compared_never_run() {
    // A recording may hold any SQL text; it is matched against the tenant
    // read and never executed, so a write in it is simply unmatched.
    let mut cursor = recorded(
        r#"{"statements": [
            {"sql": "SELECT * INTO copied_entities FROM entities", "rows": [["x"]]},
            {"sql": "SELECT nextval('tenant_counter')", "rows": [[1]]}
        ]}"#,
    )
    .expect("valid");
    let error = select_by_tenant(&mut cursor, &Json::string("acme")).expect_err("unmatched");
    assert_eq!(
        error.to_string(),
        "no recorded statement matches the executed SQL"
    );
}

#[test]
fn a_recorded_cursor_replays_by_exact_sql() {
    let mut cursor = recorded(&format!(
        r#"{{"statements": [
            {{"sql": {ENTITIES_ACME:?}, "rows": [["e1", "acme"], ["e2", "acme"]]}},
            {{"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'down'",
              "error": {{"message": "connection reset"}}}},
            {{"sql": "SELECT * FROM \"entities\" WHERE tenant_id = 'empty'"}}
        ]}}"#,
    ))
    .expect("valid");
    let rows = select_by_tenant(&mut cursor, &Json::string("acme")).expect("rows");
    assert_eq!(rows.len(), 2);
    let error = select_by_tenant(&mut cursor, &Json::string("down")).expect_err("error");
    assert_eq!(error.to_string(), "connection reset");
    assert_eq!(
        select_by_tenant(&mut cursor, &Json::string("empty")).expect("empty"),
        Vec::<Row>::new()
    );
    // Another tenant has no recording: the failure does not echo the SQL.
    let error = select_by_tenant(&mut cursor, &Json::string("other")).expect_err("unmatched");
    assert_eq!(
        error.to_string(),
        "no recorded statement matches the executed SQL"
    );
    // fetchall needs an execute first, and consumes its result.
    assert!(cursor.fetchall().is_err());
}

#[test]
fn a_recording_with_a_wrong_typed_optional_member_is_rejected() {
    assert!(recorded(r#"{"statements": []}"#).is_ok());
    for (text, reason) in [
        (r#"{}"#, "need a \"statements\" array"),
        (r#"{"statements": null}"#, "need a \"statements\" array"),
        (
            r#"{"statements": [{"rows": []}]}"#,
            "\"sql\" must be a string",
        ),
        (
            r#"{"statements": [{"sql": 1}]}"#,
            "\"sql\" must be a string",
        ),
        (
            r#"{"statements": [{"sql": "x", "rows": null}]}"#,
            "\"rows\" must be an array when present (it is null)",
        ),
        (
            r#"{"statements": [{"sql": "x", "rows": "none"}]}"#,
            "\"rows\" must be an array when present (it is string)",
        ),
        (
            r#"{"statements": [{"sql": "x", "rows": ["e1"]}]}"#,
            "every row must be an array",
        ),
        (
            r#"{"statements": [{"sql": "x", "error": null}]}"#,
            "\"error\" must be an object when present (it is null)",
        ),
        (
            r#"{"statements": [{"sql": "x", "error": []}]}"#,
            "\"error\" must be an object when present (it is array)",
        ),
        (
            r#"{"statements": [{"sql": "x", "error": {"message": 7}}]}"#,
            "\"message\" must be a string when present (it is integer)",
        ),
        (
            r#"{"statements": [{"sql": "x", "rows": [], "error": {}}]}"#,
            "exclusive",
        ),
    ] {
        let Err(message) = recorded(text) else {
            panic!("{text} must be rejected");
        };
        assert!(message.contains(reason), "{text}: {message}");
        assert!(
            message.starts_with("the recorded rows") || message.starts_with("recorded statement"),
            "{message}"
        );
    }
}
