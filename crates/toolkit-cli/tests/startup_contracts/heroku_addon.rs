//! `test_heroku_discover_addon_identity.py`: same-plan add-ons on one app do
//! not collide on `resource_id`.

use crate::common::{between, check, ensure, idx, Plugin, Verdict};
use crate::edit_replace;

const SCHEMA: &str = "skills/heroku-to-aws/references/shared/schema-discover-heroku.md";
const LIVE: &str = "skills/heroku-to-aws/references/phases/discover/discover-live.md";
const TERRAFORM: &str = "skills/heroku-to-aws/references/phases/discover/discover-terraform.md";
const ASSEMBLE: &str = "skills/heroku-to-aws/references/phases/discover/discover-assemble.md";

fn schema_format(p: &Plugin) -> Verdict {
    ensure!(
        p.read(SCHEMA)?
            .contains("addon:{app_name}:{addon_service}:{plan}:{addon_name}"),
        "schema resource_id format lost the addon_name discriminator"
    );
    Ok(())
}

#[test]
fn test_schema_resource_id_format_includes_addon_name() {
    check(
        schema_format,
        &[(
            edit_replace!(
                SCHEMA,
                "addon:{app_name}:{addon_service}:{plan}:{addon_name}",
                "addon:{app_name}:{addon_service}:{plan}"
            ),
            "discriminator",
        )],
    );
}

fn schema_required(p: &Plugin) -> Verdict {
    let text = p.read(SCHEMA)?;
    ensure!(text.contains("`addon_name`"), "addon_name not documented");
    idx(&text, "| `addon_name`    | string | ✅")?;
    Ok(())
}

#[test]
fn test_schema_addon_name_is_required_config_field() {
    check(
        schema_required,
        &[(
            edit_replace!(
                SCHEMA,
                "| `addon_name`    | string | ✅",
                "| `addon_name`    | string | ❌"
            ),
            "not found",
        )],
    );
}

fn live_entry(p: &Plugin) -> Verdict {
    let text = p.read(LIVE)?;
    ensure!(
        text.contains(":{addon_name}\""),
        "resource entry lost the :{{addon_name}} suffix"
    );
    ensure!(text.contains("addons.json"), "addons.json no longer named");
    ensure!(
        text.contains("pg-advanced-<app>-<addon_name>.out"),
        "pg-advanced capture filename lost the addon_name"
    );
    Ok(())
}

#[test]
fn test_discover_live_resource_entry_includes_addon_name_discriminator() {
    check(
        live_entry,
        &[
            (edit_replace!(LIVE, ":{addon_name}\"", "\""), "suffix"),
            (
                edit_replace!(LIVE, "addons.json", "addons.txt"),
                "addons.json",
            ),
            (
                edit_replace!(
                    LIVE,
                    "pg-advanced-<app>-<addon_name>.out",
                    "pg-advanced-<app>.out"
                ),
                "capture filename",
            ),
        ],
    );
}

fn terraform_entry(p: &Plugin) -> Verdict {
    let text = p.read(TERRAFORM)?;
    ensure!(
        text.contains(":{addon_name}"),
        "resource_id lost :{{addon_name}}"
    );
    ensure!(
        text.contains("tf_resource_name_fallback"),
        "tf_resource_name_fallback missing"
    );
    let section = between(&text, "#### `heroku_addon`", "#### `heroku_formation`")?;
    ensure!(
        section.contains("addon_name"),
        "heroku_addon attribute table lost addon_name"
    );
    Ok(())
}

#[test]
fn test_discover_terraform_resource_id_and_attribute_table_include_addon_name() {
    check(
        terraform_entry,
        &[
            (
                edit_replace!(TERRAFORM, "tf_resource_name_fallback", "tf_fallback"),
                "tf_resource_name_fallback",
            ),
            (
                edit_replace!(
                    TERRAFORM,
                    "#### `heroku_formation`",
                    "addon_name\n#### `heroku_formation`"
                ),
                "unchanged",
            ),
        ],
    );
}

fn rule_1b(p: &Plugin) -> Verdict {
    let text = p.read(ASSEMBLE)?;
    ensure!(
        text.contains("ambiguous_addon_groups"),
        "ambiguous_addon_groups missing"
    );
    ensure!(
        text.contains("do **not** pair heuristically")
            || text.contains("do not pair heuristically"),
        "heuristic pairing is no longer forbidden"
    );
    ensure!(text.contains("1b"), "rule 1b missing");
    Ok(())
}

#[test]
fn test_discover_assemble_rule_1b_reports_ambiguous_addon_groups() {
    check(
        rule_1b,
        &[
            (
                edit_replace!(ASSEMBLE, "ambiguous_addon_groups", "groups"),
                "ambiguous_addon_groups",
            ),
            (
                edit_replace!(
                    ASSEMBLE,
                    "do **not** pair heuristically",
                    "may pair heuristically"
                ),
                "heuristic",
            ),
        ],
    );
}

fn rule_1a(p: &Plugin) -> Verdict {
    let text = p.read(ASSEMBLE)?;
    let rule_1a = between(&text, "1a.", "1b.")?;
    ensure!(
        rule_1a.contains("exactly one live-sourced and one"),
        "rule 1a no longer pairs only when exactly one live-sourced and one Terraform entry exist"
    );
    Ok(())
}

#[test]
fn test_discover_assemble_rule_1a_pairs_only_when_unambiguous() {
    check(
        rule_1a,
        &[(
            edit_replace!(
                ASSEMBLE,
                "exactly one live-sourced and one",
                "any live-sourced and any"
            ),
            "rule 1a",
        )],
    );
}
