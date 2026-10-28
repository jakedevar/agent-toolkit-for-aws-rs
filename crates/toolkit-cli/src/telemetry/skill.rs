//! The `PostToolUse` (`Skill`) hook: report that one of this plugin's skills was
//! invoked (`metric_emission/skill_invoked.py`).
//!
//! The flow stops at the first thing that is not true: the consent record
//! exists, parses and has a UUID `installId`; its status is exactly
//! `ACCEPTED`; the endpoint is resolved; the skill name maps onto a
//! `PluginSkillId`. Silent, and it never fails: the user asked for the skill,
//! not for this.

use std::time::Duration;

use toolkit_core::Json;

use super::consent::{self, Status};
use super::{client, Ctx};
use crate::startup::artifacts::pyjson;

/// Skill directory name to `PluginSkillId`. An allowlist is what stops a skill
/// the service cannot represent from 400ing the whole request; `skills/shared`
/// is absent on purpose (it is imported by skills, not one itself).
pub(super) const SKILL_IDS: [(&str, &str); 12] = [
    ("agent-advisor", "AGENT_ADVISOR"),
    ("architect-for-startups", "ARCHITECT_FOR_STARTUPS"),
    ("azure-to-aws", "AZURE_TO_AWS"),
    (
        "contextual-offers-for-startups",
        "CONTEXTUAL_OFFERS_FOR_STARTUPS",
    ),
    ("gcp-to-aws", "GCP_TO_AWS"),
    ("heroku-to-aws", "HEROKU_TO_AWS"),
    ("knowledge-base-for-startups", "KNOWLEDGE_BASE_FOR_STARTUPS"),
    ("llm-to-bedrock", "LLM_TO_BEDROCK"),
    ("operate-on-aws", "OPERATE_ON_AWS"),
    ("prompt-library-for-startups", "PROMPT_LIBRARY_FOR_STARTUPS"),
    ("start-building-for-startups", "START_BUILDING_FOR_STARTUPS"),
    ("tf-best-practices", "TF_BEST_PRACTICES"),
];

/// A host may hand a hook a pipe it holds open, and a bare read then blocks
/// until an EOF that never comes, stalling the turn after every skill call.
pub(super) const STDIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The hook's stdin parsed as an object, or an empty object.
pub(super) fn read_hook_payload(ctx: &Ctx<'_>) -> Json {
    match pyjson::loads(&(ctx.stdin)(STDIN_TIMEOUT)) {
        Ok(payload @ Json::Object(_)) => payload,
        _ => Json::Object(Vec::new()),
    }
}

/// The skill the host says was invoked, or `None`. `--skill` wins, so this is
/// runnable by hand and usable from a host that does not write the Claude Code
/// payload shape.
pub(super) fn skill_name(args: &[&str], payload: &Json) -> Option<String> {
    if let Some(first) = args.first() {
        if *first == "--skill" && args.len() >= 2 {
            return Some(args[1].to_string());
        }
        if let Some(value) = first.strip_prefix("--skill=") {
            return Some(value.to_string());
        }
    }
    match payload.get("tool_input")?.get("skill")? {
        Json::String(name) => Some(name.clone()),
        _ => None,
    }
}

/// Map a host's skill name onto a `PluginSkillId`, or `None` if it is not
/// ours. Handles the qualified form `aws-startup-advisor:gcp-to-aws`: an
/// unrecognised qualifier is not a reason to drop the event, an unrecognised
/// name is.
pub(super) fn skill_id(name: Option<&str>) -> Option<&'static str> {
    let name = name?;
    let unqualified = name
        .trim()
        .rsplit(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    SKILL_IDS
        .iter()
        .find(|(directory, _)| *directory == unqualified)
        .map(|(_, id)| *id)
}

/// The hook entry point. Never fails and prints nothing.
pub(super) fn main(args: &[&str], ctx: &Ctx<'_>) {
    let Some(record) = consent::read_state(ctx) else {
        return;
    };
    if record.status != Status::Accepted {
        return;
    }
    let Some(install_id) = record.install_id else {
        return;
    };
    let payload = read_hook_payload(ctx);
    let Some(skill) = skill_id(skill_name(args, &payload).as_deref()) else {
        return;
    };
    client::emit_skill_invocation_metric(ctx, &install_id, skill, &client::endpoint(ctx));
}
