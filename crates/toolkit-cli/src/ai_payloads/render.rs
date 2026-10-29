//! Render command: validate all inputs before atomic publication.
use super::{assign, notebook, place, script, template, types, values};
use crate::{Invocation, Output};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use toolkit_core::{
    parse_json, read_regular_file, sha256_hex, Json, ToolkitError, MAX_TEXT_FILE_BYTES,
};

fn fail(code: &'static str, message: impl Into<String>) -> ToolkitError {
    ToolkitError::runtime(code, format!("{code}: {}", super::safe(&message.into())))
}
fn usage(message: &str) -> ToolkitError {
    ToolkitError::usage("render_usage", message)
}
fn read(path: &Path, cap: u64, code: &'static str) -> Result<String, ToolkitError> {
    read_regular_file(path, cap)
        .map_err(|_| fail(code, "input cannot be read as a bounded regular UTF-8 file"))
}
fn object(value: Option<&Json>) -> Result<BTreeMap<String, Json>, ToolkitError> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Json::Object(entries)) => Ok(entries.iter().cloned().collect()),
        _ => Err(fail("values_invalid", "values and assign must be objects")),
    }
}
fn version(dir: &Path, name: &str) -> Result<(PathBuf, u8), ToolkitError> {
    let entries =
        fs::read_dir(dir).map_err(|_| fail("append_source_missing", "no notebook series"))?;
    let mut latest = None;
    for entry in entries {
        let entry =
            entry.map_err(|_| fail("append_source_invalid", "cannot read notebook directory"))?;
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        let n = if filename == format!("{name}.ipynb") {
            Some(0)
        } else {
            filename
                .strip_prefix(&format!("{name}-"))
                .and_then(|s| s.strip_suffix(".ipynb"))
                .and_then(|s| {
                    if s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()) {
                        s.parse::<u8>().ok().filter(|n| *n > 0)
                    } else {
                        None
                    }
                })
        };
        if let Some(n) = n {
            if latest.as_ref().is_none_or(|(_, old)| n > *old) {
                latest = Some((entry.path(), n));
            }
        }
    }
    latest.ok_or_else(|| fail("append_source_missing", "no notebook series"))
}
fn safe_dir(project: &Path, part: &str) -> Result<PathBuf, ToolkitError> {
    let dir = project.join(part);
    match fs::symlink_metadata(&dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => Err(fail(
            "path_invalid",
            "output directory is not a real directory",
        )),
        Ok(_) => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(dir),
        Err(_) => Err(fail("path_invalid", "cannot inspect output directory")),
    }
}
fn source_ok(text: &str) -> Result<String, ToolkitError> {
    let normalized = text.replace("\r\n", "\n");
    let mut line = 1;
    let mut col = 1;
    for ch in normalized.chars() {
        if ch != '\n' && ch != '\t' && toolkit_core::is_unsafe_char(ch) {
            return Err(fail(
                "source_char",
                format!("line {line}, column {col}: U+{:04X}", ch as u32),
            ));
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Ok(normalized)
}

pub(crate) fn command(inv: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let path = Path::new(
        inv.positional(0)
            .ok_or_else(|| usage("TEMPLATE is required"))?,
    );
    let mode = inv
        .value("--mode")
        .ok_or_else(|| usage("--mode is required"))?;
    if !["script", "notebook"].contains(&mode) {
        return Err(usage("--mode must be notebook or script"));
    }
    if inv.has_flag("--append") && (inv.has_flag("--new") || mode != "notebook") {
        return Err(usage("--append requires notebook mode and excludes --new"));
    }
    let project = fs::canonicalize(
        inv.value("--project-dir")
            .ok_or_else(|| usage("--project-dir is required"))?,
    )
    .map_err(|_| fail("project_invalid", "project directory does not exist"))?;
    if !project.is_dir() {
        return Err(fail("project_invalid", "project is not a directory"));
    }
    let name = inv
        .value("--name")
        .or_else(|| project.file_name().and_then(|x| x.to_str()))
        .ok_or_else(|| usage("provide --name"))?;
    let b = name.as_bytes();
    if b.is_empty()
        || b.len() > 63
        || !(b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        || !b
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
    {
        return Err(usage("invalid --name"));
    }
    if b.len() >= 3 && b[b.len() - 3] == b'-' && b[b.len() - 2..].iter().all(u8::is_ascii_digit) {
        return Err(fail(
            "name_reserved_suffix",
            "choose a name without a version suffix",
        ));
    }
    let text = source_ok(&read(path, MAX_TEXT_FILE_BYTES, "template_invalid")?)?;
    let original = template::parse(&text);
    if let Some(v) = original.violations.first() {
        return Err(fail(
            v.code,
            format!("template line {} column {}", v.line, v.column),
        ));
    }
    if original.targets.iter().any(|target| {
        types::slot_type(&target.name) == Some(types::SlotType::Consent)
            && target.literal != "False"
    }) {
        return Err(fail(
            "consent_template_invalid",
            "consent template literals must be False",
        ));
    }
    let json = if let Some(file) = inv.value("--values") {
        let data = read(Path::new(file), MAX_TEXT_FILE_BYTES, "values_invalid")?;
        parse_json(&data).map_err(|_| fail("values_invalid", "invalid JSON"))?
    } else {
        Json::object(vec![])
    };
    let Json::Object(ref fields) = json else {
        return Err(fail("values_invalid", "expected an object"));
    };
    if fields
        .iter()
        .any(|(name, _)| name != "values" && name != "assign")
    {
        return Err(fail("values_invalid", "unknown top-level key"));
    }
    let mut slots = object(json.get("values"))?;
    let assignments = object(json.get("assign"))?;
    if original
        .slots
        .iter()
        .any(|s| s.name == "PROJECT_DIR" && s.code > 0)
        && !slots.contains_key("PROJECT_DIR")
    {
        slots.insert(
            "PROJECT_DIR".to_owned(),
            Json::string(project.to_string_lossy()),
        );
    }
    let consents: BTreeSet<_> = inv.values_of("--consent").map(str::to_owned).collect();
    for name in &consents {
        if types::slot_type(name) != Some(types::SlotType::Consent)
            || !(original.slots.iter().any(|s| s.name == *name && s.code > 0)
                || assignments.contains_key(name))
        {
            return Err(fail("consent_name_invalid", name));
        }
    }
    let module_spec = template::MODULE_CELLS
        .iter()
        .find(|(suffix, _, _, _)| path.ends_with(suffix));
    let module = module_spec.is_some();
    let mut replacements = BTreeMap::new();
    for arg in inv.values_of("--cell") {
        let (n, file) = arg
            .split_once('=')
            .ok_or_else(|| usage("--cell requires N=FILE"))?;
        let n = n
            .parse::<usize>()
            .map_err(|_| usage("--cell requires N=FILE"))?;
        if n == 0 || !original.cells.iter().any(|c| c.number == n) || replacements.contains_key(&n)
        {
            return Err(usage("invalid or duplicate --cell"));
        }
        let cell = original
            .cells
            .iter()
            .find(|c| c.number == n)
            .expect("checked");
        let end = original
            .cells
            .iter()
            .find(|c| c.number == n + 1)
            .map_or(usize::MAX, |c| c.line);
        if original.occurrences.iter().any(|o| {
            o.line > cell.line
                && o.line < end
                && types::slot_type(&o.name) == Some(types::SlotType::Consent)
                && matches!(
                    o.context,
                    template::Context::Quoted | template::Context::Bare
                )
        }) || original
            .targets
            .iter()
            .any(|t| t.cell == n && types::slot_type(&t.name) == Some(types::SlotType::Consent))
        {
            return Err(fail("cell_replaces_consent", format!("cell {n}")));
        }
        if let Some((_, _, module_path, _)) = module_spec.filter(|(_, number, _, _)| n == *number) {
            let expected = project.join(module_path);
            let resolved =
                fs::canonicalize(file).map_err(|_| fail("module_missing", "module missing"))?;
            if resolved != expected {
                return Err(fail(
                    "module_path_mismatch",
                    "module must be scripts/transform_fn.py",
                ));
            }
            toolkit_core::resolve_within(&project, module_path)
                .map_err(|_| fail("module_missing", "module is missing or symlinked"))?;
        }
        let body = source_ok(&read(
            Path::new(file),
            MAX_TEXT_FILE_BYTES,
            if module && n == 2 {
                "module_missing"
            } else {
                "cell_invalid"
            },
        )?)?;
        if template::contains_consent_identifier(&body) {
            return Err(fail("cell_binds_consent", format!("cell {n}")));
        }
        replacements.insert(n, (file.to_owned(), body));
    }
    if let Some((_, number, module_path, definition)) = module_spec {
        let (file, body) = replacements
            .get_mut(number)
            .ok_or_else(|| fail("module_cell_required", "cell 2 must name the module"))?;
        let expected = project.join(module_path);
        let resolved =
            fs::canonicalize(&*file).map_err(|_| fail("module_missing", "module missing"))?;
        if resolved != expected {
            return Err(fail(
                "module_path_mismatch",
                "module must be scripts/transform_fn.py",
            ));
        }
        if !body.lines().any(|l| l.starts_with(*definition)) {
            return Err(fail(
                "module_definition_missing",
                "module definition missing",
            ));
        }
        if mode == "notebook" {
            let path = expected.to_string_lossy();
            if !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._/+@,=-".contains(&c))
            {
                return Err(fail(
                    "writefile_path_unsafe",
                    "module path is unsafe for writefile",
                ));
            }
            *body = format!("%%writefile {path}\n{body}");
        } else {
            *body =
                "# transform_dataset is defined in scripts/transform_fn.py (imported in Cell 3)."
                    .to_owned();
        }
    }
    let replaced: BTreeSet<_> = replacements.keys().copied().collect();
    let assigned = assign::rewrite(&text, &original, &assignments, &consents, &replaced)
        .map_err(|e| fail(e.code, e.names))?;
    let lines: Vec<_> = assigned.split('\n').collect();
    let mut rebuilt = String::new();
    for (i, cell) in original.cells.iter().enumerate() {
        let end = original
            .cells
            .get(i + 1)
            .map_or(lines.len(), |next| next.line - 1);
        rebuilt.push_str(lines[cell.line - 1]);
        rebuilt.push('\n');
        if let Some((_, body)) = replacements.get(&cell.number) {
            rebuilt.push_str(body.trim_end_matches('\n'));
            rebuilt.push('\n');
        } else {
            for line in &lines[cell.line..end] {
                rebuilt.push_str(line);
                rebuilt.push('\n');
            }
        }
    }
    let parsed = template::parse(&rebuilt);
    let rendered = values::substitute(
        &rebuilt,
        &parsed,
        &slots,
        &consents,
        inv.has_flag("--allow-unresolved"),
    )
    .map_err(|e| fail(e.code, e.names))?;
    let parsed = template::parse(&rendered);
    let directory = safe_dir(
        &project,
        if mode == "script" {
            "scripts"
        } else {
            "notebooks"
        },
    )?;
    let mut source_report = None;
    let (filename, bytes) = if mode == "script" {
        let number =
            script::next_number(&directory).map_err(|e| fail(e, "cannot number script"))?;
        (
            format!("{number:02}_{name}.py"),
            script::render(&rendered, &parsed).into_bytes(),
        )
    } else if inv.has_flag("--append") {
        let (source, number) = version(&directory, name)?;
        if number == 99 {
            return Err(fail("notebook_versions_exhausted", "series is full"));
        }
        let text = read(&source, 16 * 1024 * 1024, "append_source_invalid")?;
        let hash = sha256_hex(text.as_bytes());
        let mut ids = notebook::ContentIds(hash.clone());
        let value = notebook::append(&text, notebook::cells(&rendered, &parsed), &mut ids)
            .map_err(|e| fail(e, "invalid source notebook"))?;
        source_report = Some((source, hash));
        (
            format!("{name}-{:02}.ipynb", number + 1),
            format!("{}\n", value.to_pretty_string()).into_bytes(),
        )
    } else {
        (
            format!("{name}.ipynb"),
            format!(
                "{}\n",
                notebook::new(notebook::cells(&rendered, &parsed)).to_pretty_string()
            )
            .into_bytes(),
        )
    };
    if !inv.has_flag("--dry-run") {
        toolkit_core::treesync::ensure_dir_chain(
            &project,
            if mode == "script" {
                "scripts"
            } else {
                "notebooks"
            },
        )?;
        let (actual, published) = if mode == "script" {
            script::publish(&directory, name, &bytes, &mut place::NoHooks)
                .map_err(|e| fail(e, "script publication failed"))?
        } else {
            let result = place::publish_new(&directory, &filename, &bytes, &mut place::NoHooks)
                .map_err(|e| {
                    fail(
                        if e.code == "destination_exists" && inv.has_flag("--append") {
                            "notebook_version_taken"
                        } else {
                            e.code
                        },
                        "notebook publication failed",
                    )
                })?;
            (filename.clone(), result)
        };
        if let Some(temp) = published.leftover_temp {
            out.line(&format!(
                "warning: leftover temporary file {}",
                temp.display()
            ))?;
        }
        out.line(&format!("output: {}", directory.join(&actual).display()))?;
        if let Some((source, hash)) = source_report {
            out.line(&format!("source: {} (sha256 {hash})", source.display()))?;
            out.line(&format!("open: {}", directory.join(actual).display()))?;
        }
    } else {
        out.line(&format!(
            "output: {} ({} bytes)",
            directory.join(filename).display(),
            bytes.len()
        ))?;
    }
    for edit in original.manual_edits {
        out.line(&format!(
            "manual: Cell {} line {}: {}",
            edit.cell.unwrap_or(0),
            edit.line,
            super::safe(&edit.text)
        ))?;
    }
    Ok(())
}
