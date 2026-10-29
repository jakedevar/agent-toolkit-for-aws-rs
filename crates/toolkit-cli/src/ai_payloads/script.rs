//! Script rendering and numbered publication.

use super::{
    place::{self, PublishHooks},
    template::Template,
};
use std::{fs, path::Path};

#[allow(dead_code)]
pub(crate) fn render(source: &str, template: &Template) -> String {
    let lines: Vec<_> = source.split('\n').collect();
    let title = template.title.replace("\"\"\"", "\\\"\\\"\\\"");
    let text = template
        .cells
        .first()
        .and_then(|cell| cell.text.as_deref())
        .unwrap_or("")
        .replace("\"\"\"", "\\\"\\\"\\\"");
    let mut result = format!("\"\"\"{title}\n\n{text}\n\"\"\"\n");
    for (index, cell) in template.cells.iter().enumerate().skip(1) {
        if cell.notebook_only_section {
            continue;
        }
        result.push_str(&format!("\n# %% Cell {}: {}\n", cell.number, cell.label));
        let end = template
            .cells
            .get(index + 1)
            .map_or(lines.len(), |next| next.line - 1);
        let section = &lines[cell.line..end];
        let first = section.iter().position(|s| !s.trim().is_empty());
        let last = section.iter().rposition(|s| !s.trim().is_empty());
        if let (Some(first), Some(last)) = (first, last) {
            for line in &section[first..=last] {
                if !line.trim_end().ends_with("# NOTEBOOK_ONLY") {
                    result.push_str(line);
                    result.push('\n');
                }
            }
        }
    }
    result
}

#[allow(dead_code)]
pub(crate) fn next_number(dir: &Path) -> Result<u8, &'static str> {
    let mut max = 0u8;
    if !dir.exists() {
        return Ok(1);
    }
    for entry in fs::read_dir(dir).map_err(|_| "script_directory_invalid")? {
        let entry = entry.map_err(|_| "script_directory_invalid")?;
        if !entry
            .file_type()
            .map_err(|_| "script_directory_invalid")?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let bytes = name.as_bytes();
        if bytes.len() > 3
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == b'_'
            && name.ends_with(".py")
        {
            max = max.max((bytes[0] - b'0') * 10 + bytes[1] - b'0');
        }
    }
    if max >= 99 {
        Err("script_numbers_exhausted")
    } else {
        Ok(max + 1)
    }
}

#[allow(dead_code)]
pub(crate) fn publish(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    hooks: &mut dyn PublishHooks,
) -> Result<(String, place::Published), &'static str> {
    for _ in 0..8 {
        let number = next_number(dir)?;
        let filename = format!("{number:02}_{name}.py");
        match place::publish_new(dir, &filename, bytes, hooks) {
            Ok(published) => return Ok((filename, published)),
            Err(error) if error.code == "destination_exists" => continue,
            Err(error) => return Err(error.code),
        }
    }
    Err("script_number_contended")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ap_scr_01_02_notebook_only_is_dropped() {
        let source = "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nkeep()\nmagic() # NOTEBOOK_ONLY\n# Cell 2: Skip # NOTEBOOK_ONLY_SECTION\nsecret()\n";
        let template = super::super::template::parse(source);
        let result = render(source, &template);
        assert!(result.contains("Example"));
        assert!(result.contains("# %% Cell 1: Code\nkeep()"));
        assert!(!result.contains("magic()"));
        assert!(!result.contains("secret()"));
    }
}
