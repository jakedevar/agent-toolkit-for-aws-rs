//! `safe_query.py`: validators, `build` and the injection corpus.

use super::super::sql::{
    allow, build, ident, integer, integer_text, keyword, literal, literal_with, regex,
    require_read_only, selftest, Pattern, Safe, INT, ISO_DATE, TENANT_SLUG, UUID,
};

fn permissive() -> Pattern {
    Pattern::new(".+", |value| !value.is_empty() && !value.contains('\n'))
}

fn ok(result: Result<Safe, impl std::fmt::Debug>) -> String {
    result.expect("must validate").as_str().to_string()
}

#[test]
fn the_python_selftest_passes() {
    assert_eq!(selftest(), Ok(()));
}

#[test]
fn allow_and_keyword_accept_only_the_allowlist() {
    assert_eq!(ok(allow("tenant-1", &["tenant-1"], "v")), "'tenant-1'");
    assert_eq!(ok(keyword("ASC", &["ASC", "DESC"], "k")), "ASC");
    for hostile in ["evil", "", "ASC ", "asc", "ASC; DROP TABLE t"] {
        assert!(allow(hostile, &["ASC"], "order").is_err(), "{hostile:?}");
        assert!(
            keyword(hostile, &["ASC", "DESC"], "order").is_err(),
            "{hostile:?}"
        );
    }
    // A quote in the developer-controlled set is doubled, never emitted raw.
    assert_eq!(ok(allow("o'k", &["o'k"], "v")), "'o''k'");
    let error = allow("a\u{1b}[31m\nforged", &[], "kind").expect_err("rejected");
    super::assert_single_line_safe(&error.to_string());
    assert!(!error.to_string().contains('\u{1b}'));
}

#[test]
fn tenant_slug_boundaries() {
    let sixty_four = "a".repeat(64);
    assert_eq!(
        ok(regex(&sixty_four, &TENANT_SLUG, "t")),
        format!("'{sixty_four}'")
    );
    assert_eq!(ok(regex("a", &TENANT_SLUG, "t")), "'a'");
    assert_eq!(ok(regex("-", &TENANT_SLUG, "t")), "'-'");
    assert_eq!(ok(regex("a-1-b", &TENANT_SLUG, "t")), "'a-1-b'");
    for bad in [
        "",
        &"a".repeat(65),
        "ACME",
        "acme_corp",
        "acme corp",
        "acme\n",
        "\nacme",
        "acm\u{e9}",
        "acme\0",
        "acme'",
        "acme\\",
        "acme' OR '1'='1",
        "acme; DROP TABLE entities",
        "$dq$",
    ] {
        assert!(regex(bad, &TENANT_SLUG, "req.tenant").is_err(), "{bad:?}");
    }
}

#[test]
fn error_messages_name_the_label_and_escape_the_value() {
    let message = regex("A\u{202e}\r\n\u{1b}", &TENANT_SLUG, "req.tenant")
        .expect_err("rejected")
        .to_string();
    assert!(
        message.starts_with("req.tenant failed pattern '[a-z0-9-]{1,64}': \""),
        "{message}"
    );
    super::assert_single_line_safe(&message);
    assert!(
        message.contains("\\u202E") && message.contains("\\r\\n"),
        "{message}"
    );
}

#[test]
fn quotes_and_backslashes_are_rejected_even_by_a_permissive_pattern() {
    let pattern = permissive();
    for hostile in ["x' OR 1=1 --", "it's", "'", "abc\\", "\\"] {
        assert!(regex(hostile, &pattern, "v").is_err(), "{hostile:?}");
    }
    assert!(regex("a b", &pattern, "v").is_ok());
}

#[test]
fn uuid_int_and_date_patterns() {
    assert!(regex("0f8b2c1a-0000-4000-8000-0123456789AB", &UUID, "v").is_ok());
    for bad in [
        "0f8b2c1a-0000-4000-8000-0123456789a",
        "0f8b2c1a-0000-4000-8000-0123456789abc",
        "0f8b2c1a00004000800001234567890ab",
        "0f8b2c1a-0000-4000-8000-0123456789g1",
        "0f8b2c1a-0000-4000-8000-0123456789ab\n",
    ] {
        assert!(regex(bad, &UUID, "v").is_err(), "{bad:?}");
    }
    assert!(regex("-9223372036854775808", &INT, "v").is_ok());
    assert!(regex(&"9".repeat(19), &INT, "v").is_ok());
    assert!(regex(&"9".repeat(20), &INT, "v").is_err());
    for bad in ["", "-", "+1", "1.0", "1e3", " 1", "1 ", "\u{661}"] {
        assert!(regex(bad, &INT, "v").is_err(), "{bad:?}");
    }
    for good in ["2026-01-01", "2026-12-31", "0000-01-31"] {
        assert!(regex(good, &ISO_DATE, "v").is_ok(), "{good}");
    }
    for bad in [
        "2026-00-10",
        "2026-13-10",
        "2026-01-00",
        "2026-01-32",
        "2026-1-10",
        "26-01-10",
        "2026/01/10",
        "2026-01-10\n",
        "\u{662}026-01-10",
    ] {
        assert!(regex(bad, &ISO_DATE, "v").is_err(), "{bad:?}");
    }
}

#[test]
fn identifiers_are_double_quoted_and_bounded() {
    assert_eq!(ok(ident("entities")), "\"entities\"");
    assert_eq!(ok(ident("_x9")), "\"_x9\"");
    assert_eq!(ok(ident("MixedCase")), "\"MixedCase\"");
    assert!(ident(&"a".repeat(63)).is_ok());
    for bad in [
        "",
        &"a".repeat(64),
        "9a",
        "a-b",
        "a b",
        "x\" OR 1=1 --",
        "a\"",
        "a.b",
        "a\n",
        "\u{17f}x",
        "\u{212a}x",
        "ent\u{e9}",
    ] {
        assert!(ident(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn integers() {
    assert_eq!(integer(42).as_str(), "42");
    assert_eq!(integer(-7).as_str(), "-7");
    assert_eq!(integer(i64::MIN).as_str(), "-9223372036854775808");
    assert_eq!(ok(integer_text("-7")), "-7");
    assert_eq!(ok(integer_text("007")), "007");
    for bad in ["1; DROP", "", "-", "1 ", "0x10", "١"] {
        assert!(integer_text(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn literals_are_dollar_quoted_with_a_tag_absent_from_the_value() {
    let text = ok(literal("o'reilly \\ \"x\""));
    let tag_end = text[1..].find('$').expect("closing $ of the tag") + 2;
    let boundary = &text[..tag_end];
    assert!(
        boundary.starts_with("$dq_") && boundary.ends_with('$'),
        "{text}"
    );
    assert_eq!(text, format!("{boundary}o'reilly \\ \"x\"{boundary}"));

    // A value that holds the first tags forces a fresh one.
    let mut tags = vec!["dq_00000001", "dq_00000002", "dq_00000003"].into_iter();
    let value = "has $dq_00000001$ and $dq_00000002$ inside";
    let safe = literal_with(value, || tags.next().expect("tag").to_string()).expect("literal");
    assert_eq!(safe.as_str(), format!("$dq_00000003${value}$dq_00000003$"));

    // Eight collisions are refused instead of looping or weakening the quote.
    let value = "$dq_deadbeef$";
    let error = literal_with(value, || "dq_deadbeef".to_string()).expect_err("collisions");
    assert!(error.to_string().contains("unique dollar-quote tag"));
}

#[test]
fn build_substitutes_only_matching_validated_parts() {
    let table = ident("entities").expect("ident");
    let tenant = regex("acme", &TENANT_SLUG, "t").expect("tenant");
    let sql = build(
        "SELECT * FROM {t} WHERE tenant_id = {tid} AND x = {tid}",
        &[("t", &table), ("tid", &tenant)],
    )
    .expect("build");
    assert_eq!(
        sql,
        "SELECT * FROM \"entities\" WHERE tenant_id = 'acme' AND x = 'acme'"
    );
    // Braces are literal when doubled; `{x:}` is a plain field like `{x}`.
    assert_eq!(
        build("{{\"a\": {t}}}", &[("t", &table)]).expect("braces"),
        "{\"a\": \"entities\"}"
    );
    assert_eq!(
        build("SELECT {t:}", &[("t", &table)]).expect("empty spec"),
        "SELECT \"entities\""
    );
    // A substituted value is never re-scanned as template text.
    let braces = literal("{tid}").expect("literal");
    let out = build("SELECT {v}", &[("v", &braces)]).expect("build");
    assert!(
        out.contains("{tid}") && out.starts_with("SELECT $dq_"),
        "{out}"
    );
}

#[test]
fn build_rejects_every_mismatch_and_every_format_feature() {
    let col = ident("col").expect("ident");
    let extra = ident("extra").expect("ident");
    let message = |template: &str, parts: &[(&str, &Safe)]| {
        build(template, parts).expect_err(template).to_string()
    };
    assert!(message("SELECT {x}", &[("x", &col), ("y", &extra)]).contains("extra ['y']"));
    assert!(message("SELECT {x} FROM {y}", &[("x", &col)]).contains("missing ['y']"));
    assert!(message("SELECT 1", &[("x", &col)]).contains("extra ['x']"));
    assert!(message("SELECT {x!r}", &[("x", &col)]).contains("conversion flag"));
    assert!(message("SELECT {x:>30}", &[("x", &col)]).contains("format spec"));
    assert!(message("SELECT {}", &[("x", &col)]).contains("positional"));
    assert!(message("SELECT {0}", &[("x", &col)]).contains("positional"));
    assert!(message("SELECT {x.attr}", &[("x", &col)]).contains("mismatch"));
    assert!(message("SELECT {x[0]}", &[("x", &col)]).contains("mismatch"));
    assert!(message("SELECT }", &[("x", &col)]).contains("Single '}'"));
    assert!(message("SELECT {x", &[("x", &col)]).contains("expected '}'"));
    assert!(message("SELECT {x}", &[("x", &col), ("x", &col)]).contains("duplicate"));
}

#[test]
fn read_only_guard_allows_one_select_and_nothing_else() {
    for good in [
        "SELECT * FROM \"entities\" WHERE tenant_id = 'acme'",
        "  select 1",
        "SELECT(1)",
        "SELECT\n1",
    ] {
        assert!(require_read_only(good).is_ok(), "{good:?}");
    }
    for bad in [
        "",
        "DELETE FROM entities",
        "UPDATE entities SET x = 1",
        "INSERT INTO entities VALUES (1)",
        "DROP TABLE entities",
        "WITH x AS (SELECT 1) DELETE FROM entities",
        "SELECT 1; DROP TABLE entities",
        "SELECT 1;",
        "SELECTED",
        "SELECT 1\0",
        "/* SELECT */ DELETE FROM t",
        "EXPLAIN ANALYZE DELETE FROM t",
    ] {
        assert!(require_read_only(bad).is_err(), "{bad:?}");
    }
}
