//! The two web helpers `register.py` takes from Python's standard library:
//! `urllib.parse.parse_qs` and `base64.b64decode`.

/// `urllib.parse.unquote` of a form component: `+` is a space, `%XX` bytes are
/// decoded and invalid UTF-8 becomes U+FFFD (`errors="replace"`).
fn unquote(component: &str) -> String {
    let bytes = component.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'+' {
            out.push(b' ');
        } else if byte == b'%' && at + 2 < bytes.len() {
            let high = (bytes[at + 1] as char).to_digit(16);
            let low = (bytes[at + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push(u8::try_from(high * 16 + low).unwrap_or(0));
                at += 3;
                continue;
            }
            out.push(byte);
        } else {
            out.push(byte);
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `parse_qs(body)`: the non-empty values of each name, in order of
/// appearance. A pair without `=` or with an empty value is dropped.
pub(super) fn parse_qs(body: &str) -> Vec<(String, Vec<String>)> {
    let mut form: Vec<(String, Vec<String>)> = Vec::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let (name, value) = (unquote(name), unquote(value));
        match form.iter_mut().find(|(known, _)| *known == name) {
            Some((_, values)) => values.push(value),
            None => form.push((name, vec![value])),
        }
    }
    form
}

/// The values of a form name.
pub(super) fn form_values<'f>(form: &'f [(String, Vec<String>)], name: &str) -> &'f [String] {
    form.iter()
        .find(|(known, _)| known == name)
        .map_or(&[], |(_, values)| values.as_slice())
}

/// `base64.b64decode(text)`: characters outside the alphabet are discarded; a
/// length that padding cannot complete is an error.
pub(super) fn b64decode(text: &str) -> Result<Vec<u8>, &'static str> {
    let mut sextets: Vec<u8> = Vec::new();
    let mut padding = 0_usize;
    for c in text.chars() {
        let value = match c {
            'A'..='Z' => c as u8 - b'A',
            'a'..='z' => c as u8 - b'a' + 26,
            '0'..='9' => c as u8 - b'0' + 52,
            '+' => 62,
            '/' => 63,
            '=' => {
                padding += 1;
                continue;
            }
            _ => continue,
        };
        if padding > 0 {
            // Data after padding starts a new quantum, as binascii does.
            padding = 0;
        }
        sextets.push(value);
    }
    match sextets.len() % 4 {
        1 => return Err("Invalid base64-encoded string"),
        2 if padding < 2 => return Err("Incorrect padding"),
        3 if padding < 1 => return Err("Incorrect padding"),
        _ => {}
    }
    let mut out = Vec::with_capacity(sextets.len() / 4 * 3 + 2);
    for group in sextets.chunks(4) {
        let mut acc = 0_u32;
        for sextet in group {
            acc = (acc << 6) | u32::from(*sextet);
        }
        acc <<= 6 * (4 - group.len() as u32);
        let bytes = acc.to_be_bytes();
        let count = match group.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => 0,
        };
        out.extend_from_slice(&bytes[1..=count]);
    }
    Ok(out)
}
