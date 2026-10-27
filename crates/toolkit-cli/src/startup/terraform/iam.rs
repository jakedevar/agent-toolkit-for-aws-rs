//! The IAM wildcard readers of the Terraform policy validator: the literal
//! policy JSON of an `aws_iam_policy`-style resource is scanned for an
//! `Allow` statement whose `Action` or `Resource` is a sole `"*"`, in the JSON
//! (`"Action": "*"`) and the `jsonencode` (`Action = "*"`) spelling.

use crate::startup::text::is_space;

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).copied().is_some_and(is_space) {
        at += 1;
    }
    at
}

fn starts_with(chars: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

/// `"?<key>"?\s*[:=]\s*` at `at`: the position after the separator and the
/// spaces that follow it.
fn key_assign(chars: &[char], at: usize, key: &str) -> Option<usize> {
    let name = if chars.get(at) == Some(&'"') {
        at + 1
    } else {
        at
    };
    if !starts_with(chars, name, key) {
        return None;
    }
    let mut pos = name + key.chars().count();
    if chars.get(pos) == Some(&'"') {
        pos += 1;
    }
    let sep = skip_spaces(chars, pos);
    matches!(chars.get(sep), Some(':' | '=')).then(|| skip_spaces(chars, sep + 1))
}

/// `"?<key>"?\s*[:=]\s*<literal>` at `at`: the position after the literal.
fn key_literal(chars: &[char], at: usize, key: &str, literal: &str) -> Option<usize> {
    let value = key_assign(chars, at, key)?;
    starts_with(chars, value, literal).then(|| value + literal.chars().count())
}

/// `\s*,?\s*`.
fn separator(chars: &[char], at: usize) -> usize {
    let mut pos = skip_spaces(chars, at);
    if chars.get(pos) == Some(&',') {
        pos += 1;
    }
    skip_spaces(chars, pos)
}

/// `"?Effect"?\s*[:=]\s*"Allow"` anywhere in the policy.
pub(super) fn has_allow_effect(body: &[char]) -> bool {
    (0..body.len()).any(|at| key_literal(body, at, "Effect", "\"Allow\"").is_some())
}

/// The approved statement `{ Effect = "Allow", Action =
/// "elasticbeanstalk:CreateStorageLocation", Resource = "*" }` ending where it
/// closes. AWS offers no resource-level permission for that action.
fn allowed_statement_end(chars: &[char], at: usize) -> Option<usize> {
    if chars.get(at) != Some(&'{') {
        return None;
    }
    let mut pos = skip_spaces(chars, at + 1);
    pos = separator(chars, key_literal(chars, pos, "Effect", "\"Allow\"")?);
    pos = separator(
        chars,
        key_literal(
            chars,
            pos,
            "Action",
            "\"elasticbeanstalk:CreateStorageLocation\"",
        )?,
    );
    pos = separator(chars, key_literal(chars, pos, "Resource", "\"*\"")?);
    (chars.get(pos) == Some(&'}')).then_some(pos + 1)
}

/// The policy without the narrowly approved statements. The allowlist is
/// statement-shaped, so an approved action cannot hide a second, over-broad
/// action in the same statement.
pub(super) fn remove_allowed_resource_wildcards(body: &[char]) -> Vec<char> {
    let mut kept = Vec::with_capacity(body.len());
    let mut pos = 0;
    while pos < body.len() {
        if let Some(end) = allowed_statement_end(body, pos) {
            pos = end;
        } else {
            kept.push(body[pos]);
            pos += 1;
        }
    }
    kept
}

/// True if an IAM `key` (`Action` or `Resource`) is a sole `"*"`, as a string
/// or as the only element of a list. A list that also holds scoped entries is
/// not a blanket wildcard. Like the regular expressions it replaces, only the
/// first list-form occurrence is examined.
pub(super) fn key_is_wildcard(body: &[char], key: &str) -> bool {
    if (0..body.len()).any(|at| key_literal(body, at, key, "\"*\"").is_some()) {
        return true;
    }
    let first_list = (0..body.len()).find_map(|at| {
        let open = key_assign(body, at, key)?;
        if body.get(open) != Some(&'[') {
            return None;
        }
        let close = open + 1 + body[open + 1..].iter().position(|c| *c == ']')?;
        Some(&body[open + 1..close])
    });
    first_list.is_some_and(|inner| {
        let text: String = inner.iter().collect();
        crate::manifests::py_strip(&text) == "\"*\""
    })
}
