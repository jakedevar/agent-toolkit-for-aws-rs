//! Notebook serialization and versioned append.

use super::template::Template;
use std::collections::BTreeSet;
use toolkit_core::{parse_json, sha256_hex, Json};

#[allow(dead_code)]
pub(crate) trait IdSource {
    fn id(&mut self, index: usize, attempt: usize) -> String;
}
#[allow(dead_code)]
pub(crate) struct ContentIds(pub String);
impl IdSource for ContentIds {
    fn id(&mut self, index: usize, attempt: usize) -> String {
        sha256_hex(format!("{}:{index}:{attempt}", self.0).as_bytes())[..8].to_owned()
    }
}

#[allow(dead_code)]
pub(crate) fn cells(source: &str, template: &Template) -> Vec<Json> {
    let lines: Vec<_> = source.split('\n').collect();
    template
        .cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let end = template
                .cells
                .get(index + 1)
                .map_or(lines.len(), |next| next.line - 1);
            let section = &lines[cell.line..end];
            let first = section.iter().position(|s| !s.trim().is_empty());
            let last = section.iter().rposition(|s| !s.trim().is_empty());
            let body = match (first, last) {
                (Some(first), Some(last)) => &section[first..=last],
                _ => &[],
            };
            let text = body
                .iter()
                .map(|line| {
                    if cell.markdown {
                        line.strip_prefix("# ")
                            .or_else(|| (*line == "#").then_some(""))
                            .unwrap_or(line)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>();
            let sources = text
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    Json::string(if i + 1 < text.len() {
                        format!("{line}\n")
                    } else {
                        (*line).to_owned()
                    })
                })
                .collect();
            let mut fields = vec![
                (
                    "cell_type",
                    Json::string(if cell.markdown { "markdown" } else { "code" }),
                ),
                ("metadata", Json::object(vec![])),
                ("source", Json::Array(sources)),
            ];
            if !cell.markdown {
                fields.insert(1, ("execution_count", Json::Null));
                fields.insert(3, ("outputs", Json::Array(vec![])));
            }
            Json::object(fields)
        })
        .collect()
}

#[allow(dead_code)]
pub(crate) fn new(cells: Vec<Json>) -> Json {
    Json::object(vec![
        ("cells", Json::Array(cells)),
        (
            "metadata",
            Json::object(vec![
                (
                    "kernelspec",
                    Json::object(vec![
                        ("display_name", Json::string("Python 3")),
                        ("language", Json::string("python")),
                        ("name", Json::string("python3")),
                    ]),
                ),
                (
                    "language_info",
                    Json::object(vec![("name", Json::string("python"))]),
                ),
            ]),
        ),
        ("nbformat", Json::Int(4)),
        ("nbformat_minor", Json::Int(4)),
    ])
}

#[allow(dead_code)]
pub(crate) fn append(
    source: &str,
    mut added: Vec<Json>,
    ids: &mut dyn IdSource,
) -> Result<Json, &'static str> {
    let mut notebook = parse_json(source).map_err(|_| "notebook_invalid")?;
    if notebook.get("nbformat") != Some(&Json::Int(4)) {
        return Err("notebook_version_unsupported");
    }
    let minor = match notebook.get("nbformat_minor") {
        Some(Json::Int(v @ 0..=5)) => *v,
        _ => return Err("notebook_version_unsupported"),
    };
    let Json::Object(ref mut fields) = notebook else {
        return Err("notebook_invalid");
    };
    let Some((_, Json::Array(existing))) = fields.iter_mut().find(|(name, _)| name == "cells")
    else {
        return Err("notebook_invalid");
    };
    if minor == 5 {
        let mut seen = BTreeSet::new();
        for cell in existing.iter() {
            let Some(id) = cell.get("id").and_then(Json::as_str) else {
                return Err("notebook_cell_ids_invalid");
            };
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || !seen.insert(id.to_owned())
            {
                return Err("notebook_cell_ids_invalid");
            }
        }
        for (index, cell) in added.iter_mut().enumerate() {
            let mut selected = None;
            for attempt in 0..64 {
                let id = ids.id(index, attempt);
                if seen.insert(id.clone()) {
                    selected = Some(id);
                    break;
                }
            }
            let id = selected.ok_or("cell_id_exhausted")?;
            let Json::Object(fields) = cell else {
                return Err("notebook_invalid");
            };
            fields.push(("id".to_owned(), Json::string(id)));
        }
    }
    existing.extend(added);
    Ok(notebook)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ap_nb_08_10_11_12_append_versions_and_ids() {
        let cells = vec![Json::object(vec![("cell_type", Json::string("code"))])];
        let source = r#"{"cells":[{"id":"a1"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        let mut ids = FixedIds(vec!["a1", "b2"].into_iter());
        let copy = append(source, cells.clone(), &mut ids).unwrap();
        assert!(copy.to_compact_string().contains("\"id\":\"b2\""));
        let invalid = source.replace("\"a1\"", "\"bad id!\"");
        assert_eq!(
            append(&invalid, cells.clone(), &mut ids).unwrap_err(),
            "notebook_cell_ids_invalid"
        );
        let old = source.replace("\"nbformat_minor\":5", "\"nbformat_minor\":4");
        let copy = append(&old, cells.clone(), &mut ids).unwrap();
        assert!(!copy.to_compact_string().contains("\"id\":\"b2\""));
        let unsupported = source.replace("\"nbformat_minor\":5", "\"nbformat_minor\":6");
        assert_eq!(
            append(&unsupported, cells, &mut ids).unwrap_err(),
            "notebook_version_unsupported"
        );
    }
    #[test]
    fn ap_nb_09_id_collision_limit_and_content_ids_are_deterministic() {
        let source = r#"{"cells":[{"id":"a1"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        let cell = Json::object(vec![("cell_type", Json::string("code"))]);
        let mut collisions = FixedIds(vec!["a1"; 64].into_iter());
        assert_eq!(
            append(source, vec![cell.clone()], &mut collisions).unwrap_err(),
            "cell_id_exhausted"
        );
        let mut first = ContentIds("snapshot".into());
        let mut second = ContentIds("snapshot".into());
        assert_eq!(
            append(source, vec![cell.clone()], &mut first).unwrap(),
            append(source, vec![cell], &mut second).unwrap()
        );
    }
    struct FixedIds(std::vec::IntoIter<&'static str>);
    impl IdSource for FixedIds {
        fn id(&mut self, _: usize, _: usize) -> String {
            self.0.next().unwrap_or("a1").to_owned()
        }
    }
}
