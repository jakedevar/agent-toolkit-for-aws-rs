//! Rewriting catalogued literal assignments.

use super::{
    template::{self, LiteralKind, Template},
    types,
    values::SubstitutionError,
};
use std::collections::{BTreeMap, BTreeSet};
use toolkit_core::Json;

fn fail(code: &'static str, name: &str) -> SubstitutionError {
    SubstitutionError {
        code,
        names: name.to_owned(),
    }
}

fn invalid_target(name: &str, source: &str, template: &Template) -> SubstitutionError {
    let lines: Vec<_> = source.lines().collect();
    let slot = template.occurrences.iter().find(|occurrence| {
        occurrence.context == template::Context::Quoted
            && lines.get(occurrence.line - 1).is_some_and(|line| {
                line.strip_prefix(name)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
    });
    slot.map_or_else(
        || fail("assign_target_invalid", name),
        |slot| {
            fail(
                "assign_target_invalid",
                &format!("{name}: use values.{}", slot.name),
            )
        },
    )
}

#[allow(dead_code)] // Connected by the render command.
pub(crate) fn rewrite(
    source: &str,
    template: &Template,
    entries: &BTreeMap<String, Json>,
    consent: &BTreeSet<String>,
    replaced: &BTreeSet<usize>,
) -> Result<String, SubstitutionError> {
    let mut errors = Vec::new();
    for (name, value) in entries {
        let single = BTreeMap::from([(name.clone(), value.clone())]);
        if let Err(error) = rewrite_validated(source, template, &single, consent, replaced) {
            errors.push(error);
        }
    }
    if let Some(first) = errors.first() {
        return Err(SubstitutionError {
            code: first.code,
            names: errors
                .iter()
                .map(|error| format!("{} ({})", error.names, error.code))
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    rewrite_validated(source, template, entries, consent, replaced)
}

fn rewrite_validated(
    source: &str,
    template: &Template,
    entries: &BTreeMap<String, Json>,
    consent: &BTreeSet<String>,
    replaced: &BTreeSet<usize>,
) -> Result<String, SubstitutionError> {
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    for (name, value) in entries {
        let target = template
            .targets
            .iter()
            .find(|target| target.name == *name)
            .ok_or_else(|| invalid_target(name, source, template))?;
        if replaced.contains(&target.cell) {
            return Err(fail("assign_target_replaced", name));
        }
        let quote = target.literal.chars().next().unwrap_or('"');
        let literal = if types::slot_type(name).is_some() {
            types::literal(name, value, types::consent_given(consent, name))
        } else if template::is_secret_name(&name.to_ascii_uppercase()) {
            types::secret_literal(value, quote)
        } else {
            match (target.kind, value) {
                (LiteralKind::String, _) => types::quoted_literal(value, quote),
                (LiteralKind::Bool, Json::Bool(v)) => {
                    Ok(if *v { "True" } else { "False" }.to_owned())
                }
                (LiteralKind::Int, Json::Int(v)) if *v >= 0 => Ok(v.to_string()),
                (LiteralKind::Float, Json::Int(v)) => Ok(format!("{v}.0")),
                (LiteralKind::Float, Json::Float(v)) if v.is_finite() => Ok(format!("{v:?}")),
                _ => Err("assign_value_invalid"),
            }
        }
        .map_err(|code| fail(code, name))?;
        let statement = lines
            .get(target.line - 1)
            .ok_or_else(|| fail("assign_target_invalid", name))?;
        let equals = statement
            .find('=')
            .ok_or_else(|| fail("assign_target_invalid", name))?;
        let parenthesized = statement[equals + 1..].trim_start().starts_with('(');
        let line_index = target.line - 1 + usize::from(parenthesized);
        let line = lines
            .get_mut(line_index)
            .ok_or_else(|| fail("assign_target_invalid", name))?;
        let offset = if parenthesized { 0 } else { equals + 1 };
        let remaining = &line[offset..];
        let start = offset + remaining.len() - remaining.trim_start().len();
        if !line[start..].starts_with(&target.literal) {
            return Err(fail("assign_target_invalid", name));
        }
        line.replace_range(start..start + target.literal.len(), &literal);
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::Json;

    #[test]
    fn ap_asn_01_02_04_05_08_literals_consent_and_replaced_cells() {
        let source = "# Cell 0 [markdown]: Test\n# test\n# Cell 1: Config\nBASE_MODEL = 'old' # comment\nACCEPT_EULA = (\n    False # consent\n)\n";
        let template = super::super::template::parse(source);
        let mut entries = BTreeMap::new();
        entries.insert("BASE_MODEL".to_owned(), Json::string("a'b"));
        entries.insert("ACCEPT_EULA".to_owned(), Json::Bool(true));
        assert_eq!(
            rewrite(
                source,
                &template,
                &entries,
                &BTreeSet::new(),
                &BTreeSet::new()
            )
            .unwrap_err()
            .code,
            "consent_not_given"
        );
        let consent = BTreeSet::from(["ACCEPT_EULA".to_owned()]);
        let rendered = rewrite(source, &template, &entries, &consent, &BTreeSet::new()).unwrap();
        assert!(rendered.contains("BASE_MODEL = 'a\\'b' # comment"));
        assert!(rendered.contains("    True # consent"));
        assert_eq!(
            rewrite(source, &template, &entries, &consent, &BTreeSet::from([1]))
                .unwrap_err()
                .code,
            "assign_target_replaced"
        );
        entries.insert("MISSING".to_owned(), Json::Int(1));
        assert_eq!(
            rewrite(source, &template, &entries, &consent, &BTreeSet::new())
                .unwrap_err()
                .code,
            "assign_target_invalid"
        );
    }
}
