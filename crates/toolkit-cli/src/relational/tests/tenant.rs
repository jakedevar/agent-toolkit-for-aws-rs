//! `tenant_query.py`: tenant isolation, read-only safety and the cursors.

use toolkit_core::Json;

use super::super::tenant::{
    select_by_tenant, tenant_sql, DbError, NoLiveCursor, ReadOnly, RecordedCursor, Row, SqlCursor,
    TenantQueryError,
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
    fn execute(&mut self, sql: &str) -> Result<(), DbError> {
        self.executed.push(sql.to_string());
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

#[test]
fn the_tenant_filter_is_always_in_the_statement() {
    assert_eq!(tenant_sql("acme").expect("sql"), ENTITIES_ACME);
    for tenant in ["a", "tenant-2", &"z".repeat(64)] {
        let sql = tenant_sql(tenant).expect("sql");
        assert!(
            sql.ends_with(&format!("WHERE tenant_id = '{tenant}'")),
            "{sql}"
        );
        assert_eq!(sql.matches("tenant_id").count(), 1);
    }
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

#[test]
fn the_read_only_cursor_refuses_writes_before_they_reach_the_driver() {
    let mut cursor = ReadOnly(Spy::default());
    for write in [
        "DELETE FROM entities WHERE tenant_id = 'acme'",
        "UPDATE entities SET name = 'x'",
        "SELECT 1; DELETE FROM entities",
        "DROP TABLE entities",
    ] {
        assert!(cursor.execute(write).is_err(), "{write}");
    }
    assert!(cursor.0.executed.is_empty());
    cursor.execute(ENTITIES_ACME).expect("select passes");
    assert_eq!(cursor.0.executed, [ENTITIES_ACME]);
    // The statement select_by_tenant builds always satisfies the guard.
    select_by_tenant(&mut cursor, &Json::string("acme")).expect("select");
}

#[test]
fn the_no_live_cursor_fails_every_call_without_a_connection() {
    let mut cursor = NoLiveCursor;
    let error = cursor
        .execute(ENTITIES_ACME)
        .expect_err("no live connection");
    assert!(
        error.0.contains("no live Aurora DSQL connection"),
        "{error}"
    );
    assert!(cursor.fetchall().is_err());
    let error =
        select_by_tenant(&mut ReadOnly(NoLiveCursor), &Json::string("acme")).expect_err("fails");
    assert!(matches!(error, TenantQueryError::Database(_)));
}

fn recorded(text: &str) -> Result<RecordedCursor, String> {
    RecordedCursor::from_document(&json(text))
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
