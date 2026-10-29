//! Typed placeholder substitution.

use super::{
    template::{Context, Template},
    types,
};
use std::collections::{BTreeMap, BTreeSet};
use toolkit_core::Json;

#[derive(Debug)]
pub(crate) struct SubstitutionError {
    pub code: &'static str,
    pub names: String,
}

fn error(code: &'static str, names: impl IntoIterator<Item = String>) -> SubstitutionError {
    SubstitutionError {
        code,
        names: names
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Substitutes lexer-identified spans, leaving comments and sentinel comparisons intact.
#[allow(dead_code)] // Connected by the render command.
pub(crate) fn substitute(
    source: &str,
    template: &Template,
    values: &BTreeMap<String, Json>,
    consent: &BTreeSet<String>,
    allow_unresolved: bool,
) -> Result<String, SubstitutionError> {
    let used: BTreeSet<_> = template
        .slots
        .iter()
        .filter(|s| s.code > 0)
        .map(|s| s.name.as_str())
        .collect();
    let unknown: Vec<_> = values
        .keys()
        .filter(|name| !used.contains(name.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(error("unknown_value", unknown));
    }
    let invalid_consent: Vec<_> = consent
        .iter()
        .filter(|name| {
            types::slot_type(name) != Some(types::SlotType::Consent)
                || !used.contains(name.as_str())
        })
        .cloned()
        .collect();
    if !invalid_consent.is_empty() {
        return Err(error("consent_name_invalid", invalid_consent));
    }
    let missing_bare: Vec<_> = template
        .occurrences
        .iter()
        .filter(|o| o.context == Context::Bare && !values.contains_key(&o.name))
        .map(|o| o.name.clone())
        .collect();
    if !missing_bare.is_empty() {
        return Err(error("typed_slot_unresolved", missing_bare));
    }
    let missing: Vec<_> = template
        .occurrences
        .iter()
        .filter(|o| o.context == Context::Quoted && !values.contains_key(&o.name))
        .map(|o| o.name.clone())
        .collect();
    if !allow_unresolved && !missing.is_empty() {
        return Err(error("slot_unresolved", missing));
    }

    let normalized = source.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_owned).collect();
    for occurrence in template.occurrences.iter().rev() {
        if !matches!(occurrence.context, Context::Bare | Context::Quoted) {
            continue;
        }
        let Some(value) = values.get(&occurrence.name) else {
            continue;
        };
        let literal = match occurrence.context {
            Context::Bare => {
                types::literal(&occurrence.name, value, consent.contains(&occurrence.name))
            }
            Context::Quoted if super::template::is_secret_name(&occurrence.name) => {
                types::secret_literal(value, occurrence.quote.unwrap_or('"'))
            }
            Context::Quoted => types::quoted_literal(value, occurrence.quote.unwrap_or('"')),
            _ => unreachable!(),
        }
        .map_err(|code| error(code, [occurrence.name.clone()]))?;
        let Some(line) = lines.get_mut(occurrence.line - 1) else {
            return Err(error("slot_position_invalid", [occurrence.name.clone()]));
        };
        let Some(start) = line
            .char_indices()
            .nth(occurrence.column - 1)
            .map(|(i, _)| i)
        else {
            return Err(error("slot_position_invalid", [occurrence.name.clone()]));
        };
        let expected = if let Some(quote) = occurrence.quote {
            format!("{quote}[{}]{quote}", occurrence.name)
        } else {
            format!("[{}]", occurrence.name)
        };
        if !line[start..].starts_with(&expected) {
            return Err(error("slot_position_invalid", [occurrence.name.clone()]));
        }
        line.replace_range(start..start + expected.len(), &literal);
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use toolkit_core::{parse_json, Json};

    const SOURCE: &str = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\nACCEPT_EULA = [ACCEPT_EULA]\nx = \"[ROLE_ARN]\"\n# [ROLE_ARN]\n";

    #[test]
    fn ap_sub_10_13_20_21_con_01_substitutes_only_code_and_refuses_missing_bare() {
        let template = super::super::template::parse(SOURCE);
        let mut values = BTreeMap::new();
        assert_eq!(
            substitute(SOURCE, &template, &values, &BTreeSet::new(), true)
                .unwrap_err()
                .code,
            "typed_slot_unresolved"
        );
        values.insert("ACCEPT_EULA".into(), Json::Bool(false));
        values.insert("ROLE_ARN".into(), Json::string("a\"b\\c"));
        let output = substitute(SOURCE, &template, &values, &BTreeSet::new(), false).unwrap();
        assert!(output.contains("ACCEPT_EULA = False"));
        assert!(output.contains("x = \"a\\\"b\\\\c\""));
        assert!(output.contains("# [ROLE_ARN]"));
        values.remove("ROLE_ARN");
        assert!(
            substitute(SOURCE, &template, &values, &BTreeSet::new(), true)
                .unwrap()
                .contains("\"[ROLE_ARN]\"")
        );
        assert_eq!(
            substitute(SOURCE, &template, &values, &BTreeSet::new(), false)
                .unwrap_err()
                .code,
            "slot_unresolved"
        );
    }

    #[test]
    fn ap_sec_01_02_03_sub_15_secret_and_unknown_values_fail_without_echo() {
        let source = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\ntoken = \"[HF_TOKEN_OR_NONE]\"\nif token == \"[HF_TOKEN_OR_NONE]\": pass\n";
        let template = super::super::template::parse(source);
        let mut values = BTreeMap::new();
        values.insert("HF_TOKEN_OR_NONE".into(), Json::string("secret-sentinel"));
        let error = substitute(source, &template, &values, &BTreeSet::new(), false).unwrap_err();
        assert_eq!(error.code, "secret_value_invalid");
        assert!(!error.names.contains("secret-sentinel"));
        values.insert("HF_TOKEN_OR_NONE".into(), parse_json("null").unwrap());
        let output = substitute(source, &template, &values, &BTreeSet::new(), false).unwrap();
        assert!(output.contains("token = None"));
        assert!(output.contains("== \"[HF_TOKEN_OR_NONE]\""));
        values.insert("TYPO".into(), Json::Bool(true));
        assert_eq!(
            substitute(source, &template, &values, &BTreeSet::new(), false)
                .unwrap_err()
                .code,
            "unknown_value"
        );
    }
}
