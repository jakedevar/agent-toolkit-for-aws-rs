//! `test_credential_lifecycle.py`: every terminal exit of the usage-discovery
//! sub-files deletes the admin-key file, or documents why it must not.

use crate::common::{check, ensure, name, window, Plugin, Verdict};
use crate::{edit_replace, edit_window};

const ANTHROPIC: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md",
    "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md",
];
const OPENAI: &str = "skills/azure-to-aws/references/phases/discover/discover-openai-api.md";
const ALL: [&str; 3] = [ANTHROPIC[0], ANTHROPIC[1], OPENAI];

fn declares_retry_pending(p: &Plugin) -> Verdict {
    for f in ALL {
        ensure!(
            p.read(f)?.contains("\"retry_pending\""),
            "{}: manifest.json schema no longer declares retry_pending",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_manifest_schema_declares_retry_pending() {
    check(
        declares_retry_pending,
        &[
            (
                edit_replace!(ANTHROPIC[0], "\"retry_pending\"", "\"retry\""),
                "retry_pending",
            ),
            (
                edit_replace!(OPENAI, "\"retry_pending\"", "\"retry\""),
                "retry_pending",
            ),
        ],
    );
}

fn only_retry_carve_out(p: &Plugin) -> Verdict {
    for f in ALL {
        let text = p.read(f)?;
        ensure!(
            text.contains("KEY_INVALID_MID_RUN"),
            "{}: missing KEY_INVALID_MID_RUN branch",
            name(f)
        );
        ensure!(
            text.contains("retry_pending: true"),
            "{}: KEY_INVALID_MID_RUN branch no longer carves out retry_pending: true",
            name(f)
        );
        ensure!(
            text.contains("retry_pending: false"),
            "{}: other terminal exits no longer explicitly set retry_pending: false",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_key_invalid_mid_run_is_the_only_documented_retry_carve_out() {
    check(
        only_retry_carve_out,
        &[
            (
                edit_replace!(ANTHROPIC[1], "KEY_INVALID_MID_RUN", "KEY_X"),
                "missing KEY_INVALID_MID_RUN",
            ),
            (
                edit_replace!(OPENAI, "retry_pending: true", "retry_pending: yes"),
                "carves out",
            ),
            (
                edit_replace!(ANTHROPIC[0], "retry_pending: false", "retry_pending: no"),
                "retry_pending: false",
            ),
        ],
    );
}

/// The branch starting at `anchor` (`len` characters) must name `file` and
/// say "delete".
fn deletes(text: &str, anchor: &str, len: usize, file: &str, what: &str, f: &str) -> Verdict {
    let branch = window(text, anchor, len)?;
    ensure!(
        branch.contains(file) && branch.to_lowercase().contains("delete"),
        "{}: {what} no longer deletes the admin-key file",
        name(f)
    );
    Ok(())
}

fn org_type_fork_deletes(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        deletes(
            &p.read(f)?,
            "Org-type fork handling",
            600,
            ".anthropic-admin-env",
            "Enterprise-org-type-fork exit",
            f,
        )?;
    }
    Ok(())
}

#[test]
fn test_anthropic_org_type_fork_deletes_key_file() {
    check(
        org_type_fork_deletes,
        &[(
            edit_window!(
                ANTHROPIC[0],
                "Org-type fork handling",
                600,
                "delete",
                "keep"
            ),
            "no longer deletes",
        )],
    );
}

fn all_rows_failed_deletes(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        deletes(
            &p.read(f)?,
            "If EVERY row",
            200,
            ".anthropic-admin-env",
            "all-rows-failed exit",
            f,
        )?;
    }
    Ok(())
}

#[test]
fn test_anthropic_all_rows_failed_deletes_key_file() {
    check(
        all_rows_failed_deletes,
        &[(
            edit_window!(
                ANTHROPIC[1],
                "If EVERY row",
                200,
                ".anthropic-admin-env",
                ".x"
            ),
            "no longer deletes",
        )],
    );
}

fn keeps_key_file(text: &str, f: &str) -> Verdict {
    let branch = window(text, "abort the remaining calls", 700)?;
    ensure!(
        branch.contains("retry_pending: true") && branch.contains("NOT delete"),
        "{}: KEY_INVALID_MID_RUN branch no longer documents keeping the key file via retry_pending: true",
        name(f)
    );
    Ok(())
}

fn anthropic_keeps(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        keeps_key_file(&p.read(f)?, f)?;
    }
    Ok(())
}

#[test]
fn test_anthropic_key_invalid_mid_run_keeps_key_file() {
    check(
        anthropic_keeps,
        &[(
            edit_replace!(ANTHROPIC[0], "NOT delete", "delete"),
            "no longer documents keeping",
        )],
    );
}

fn openai_all_failed_deletes(p: &Plugin) -> Verdict {
    deletes(
        &p.read(OPENAI)?,
        "If EVERY usage row",
        200,
        ".openai-admin-env",
        "all-usage-failed exit",
        OPENAI,
    )
}

#[test]
fn test_azure_openai_all_usage_failed_deletes_key_file() {
    check(
        openai_all_failed_deletes,
        &[(
            edit_window!(OPENAI, "If EVERY usage row", 200, "delete", "keep"),
            "no longer deletes",
        )],
    );
}

fn openai_keeps(p: &Plugin) -> Verdict {
    keeps_key_file(&p.read(OPENAI)?, OPENAI)
}

#[test]
fn test_azure_openai_key_invalid_mid_run_keeps_key_file() {
    check(
        openai_keeps,
        &[(
            edit_replace!(OPENAI, "retry_pending: true", "retry_pending: 1"),
            "no longer documents keeping",
        )],
    );
}

fn step4_success_deletion(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        let text = p.read(f)?;
        ensure!(
            text.contains("**Clean up (default, not optional):** delete"),
            "{}: success-path cleanup sentence changed",
            name(f)
        );
        ensure!(
            text.contains(".anthropic-admin-env` now"),
            "{}: success-path key file name changed",
            name(f)
        );
    }
    let text = p.read(OPENAI)?;
    ensure!(
        text.contains("**Clean up (default, not optional):** delete"),
        "{}: success-path cleanup sentence changed",
        name(OPENAI)
    );
    ensure!(
        text.contains(".openai-admin-env` now"),
        "{}: success-path key file name changed",
        name(OPENAI)
    );
    Ok(())
}

#[test]
fn test_step4_success_path_deletion_language_is_unchanged() {
    check(
        step4_success_deletion,
        &[
            (
                edit_replace!(
                    ANTHROPIC[0],
                    "**Clean up (default, not optional):** delete",
                    "**Clean up:** delete"
                ),
                "cleanup sentence",
            ),
            (
                edit_replace!(OPENAI, ".openai-admin-env` now", ".openai-admin-env` later"),
                "key file name",
            ),
        ],
    );
}
