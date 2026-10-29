//! JSON-RPC framing: the bounded stdin line reader, client message
//! classification, oversize-line id recovery, and the compact serialiser
//! whose output length is known (and reserved) before it is allocated.

use std::io::{self, Read};

use toolkit_core::{escape_json_string, parse_json_with_limits, Json};

use super::budget::{BudgetError, Reservation, Wait};
use super::limits::{Limits, ID_RECOVERY_PREFIX, MAX_ID_BYTES};

// ------------------------------------------------------------ serialiser

/// Compact JSON for one stdout line (no newline): the same form as
/// [`Json::to_compact_string`], with U+2028 and U+2029 escaped so no line
/// separator appears raw.
pub(crate) fn to_line(value: &Json) -> String {
    let mut out = String::with_capacity(compact_len(value));
    write_value(value, &mut out);
    out
}

/// The exact byte length of [`to_line`]'s output, computed without
/// allocating it.
pub(crate) fn compact_len(value: &Json) -> usize {
    match value {
        Json::Null => 4,
        Json::Bool(true) => 4,
        Json::Bool(false) => 5,
        Json::Int(_) | Json::Float(_) => {
            let mut counter = Counter(0);
            write_number(value, &mut counter);
            counter.0
        }
        Json::String(text) => string_len(text),
        Json::Array(items) => {
            2 + items.iter().map(compact_len).sum::<usize>() + items.len().saturating_sub(1)
        }
        Json::Object(pairs) => {
            2 + pairs
                .iter()
                .map(|(key, value)| string_len(key) + 1 + compact_len(value))
                .sum::<usize>()
                + pairs.len().saturating_sub(1)
        }
    }
}

struct Counter(usize);

impl std::fmt::Write for Counter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

fn write_number(value: &Json, out: &mut impl std::fmt::Write) {
    match value {
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) if number.is_finite() => {
            // As `Json::to_compact_string`: Debug keeps a fraction or an
            // exponent, so a float never reads back as an integer.
            let _ = write!(out, "{number:?}");
        }
        _ => {
            let _ = out.write_str("null");
        }
    }
}

fn char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0C}' => 2,
        '\u{2028}' | '\u{2029}' => 6,
        c if u32::from(c) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

fn string_len(text: &str) -> usize {
    2 + text.chars().map(char_len).sum::<usize>()
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if char_len(c) == c.len_utf8() && c != '"' && c != '\\' => out.push(c),
            c => out.push_str(&escape_json_string(c.encode_utf8(&mut [0; 4]))),
        }
    }
    out.push('"');
}

fn write_value(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(_) | Json::Float(_) => write_number(value, out),
        Json::String(text) => write_string(text, out),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            out.push('{');
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out);
            }
            out.push('}');
        }
    }
}

// ------------------------------------------------------------- parsing

#[cfg(test)]
static PARSE_THREADS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Names of the threads that parsed upstream messages (test hook for
/// "parsing never happens on the stdin reader or the stdout writer").
#[cfg(test)]
pub(crate) fn parse_threads() -> Vec<String> {
    PARSE_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Why an upstream message was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dropped {
    /// Not UTF-8, not JSON, too deep or too many nodes.
    Malformed,
    /// JSON, but not an object or a non-empty batch of objects.
    NotRpc,
}

/// Parse one upstream message (on the calling worker thread).
pub(crate) fn parse_upstream(bytes: &[u8], limits: &Limits) -> Result<Json, Dropped> {
    #[cfg(test)]
    {
        let name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let mut threads = PARSE_THREADS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !threads.contains(&name) {
            threads.push(name);
        }
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Dropped::Malformed)?;
    let value = parse_json_with_limits(text, &limits.json()).map_err(|_| Dropped::Malformed)?;
    if is_message(&value) {
        Ok(value)
    } else {
        Err(Dropped::NotRpc)
    }
}

/// An object, or a non-empty array of objects.
pub(crate) fn is_message(value: &Json) -> bool {
    match value {
        Json::Object(_) => true,
        Json::Array(items) => {
            !items.is_empty() && items.iter().all(|i| matches!(i, Json::Object(_)))
        }
        _ => false,
    }
}

/// Whether `id` is an id the proxy can echo in a control line: a string or
/// a number, at most [`MAX_ID_BYTES`] of compact JSON.
pub(crate) fn is_valid_id(id: &Json) -> bool {
    matches!(id, Json::String(_) | Json::Int(_) | Json::Float(_)) && compact_len(id) <= MAX_ID_BYTES
}

/// One client JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClientMessage {
    /// A request: it gets exactly one response.
    Request {
        id: Json,
        method: String,
        /// `params.name` of a `tools/call`.
        tool: Option<String>,
    },
    /// A notification (no id).
    Notification { method: String },
    /// The client's response to a server-initiated request.
    Response,
}

/// Classify one object. `None` when it is not a JSON-RPC message the proxy
/// accepts.
pub(crate) fn classify_one(value: &Json) -> Option<ClientMessage> {
    let Json::Object(_) = value else {
        return None;
    };
    match value.get("method") {
        Some(Json::String(method)) => match value.get("id") {
            None => Some(ClientMessage::Notification {
                method: method.clone(),
            }),
            Some(id) if is_valid_id(id) => {
                let tool = (method == "tools/call")
                    .then(|| {
                        value
                            .get("params")
                            .and_then(|p| p.get("name"))
                            .and_then(Json::as_str)
                            .map(str::to_string)
                    })
                    .map(Option::unwrap_or_default);
                Some(ClientMessage::Request {
                    id: id.clone(),
                    method: method.clone(),
                    tool,
                })
            }
            Some(_) => None,
        },
        Some(_) => None,
        None => {
            let answered = value.get("result").is_some() || value.get("error").is_some();
            (value.get("id").is_some() && answered).then_some(ClientMessage::Response)
        }
    }
}

/// Classify a client line's value: one message or a batch. `Err` carries the
/// id to answer with, when one is recoverable.
pub(crate) fn classify(value: &Json) -> Result<Vec<ClientMessage>, Option<Json>> {
    match value {
        Json::Array(items) if !items.is_empty() => items
            .iter()
            .map(classify_one)
            .collect::<Option<Vec<_>>>()
            .ok_or(None),
        Json::Object(_) => classify_one(value)
            .map(|m| vec![m])
            .ok_or_else(|| value.get("id").filter(|id| is_valid_id(id)).cloned()),
        _ => Err(None),
    }
}

// -------------------------------------------------------- id recovery

/// Recover the top-level `"id"` of a line from its first bytes (an oversize
/// line is never parsed whole). Only a valid id is returned.
pub(crate) fn recover_id(prefix: &[u8]) -> Option<Json> {
    let mut scan = Scanner {
        bytes: prefix,
        pos: 0,
    };
    scan.ws();
    scan.eat(b'{')?;
    loop {
        scan.ws();
        let key_start = scan.pos;
        scan.string()?;
        let key = &prefix[key_start..scan.pos];
        scan.ws();
        scan.eat(b':')?;
        scan.ws();
        let value_start = scan.pos;
        scan.value()?;
        if key == b"\"id\"" {
            let text = std::str::from_utf8(&prefix[value_start..scan.pos]).ok()?;
            let id = parse_json_with_limits(text, &toolkit_core::JsonLimits::default()).ok()?;
            return is_valid_id(&id).then_some(id);
        }
        scan.ws();
        if scan.eat(b',').is_none() {
            return None;
        }
    }
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.peek()? == byte).then(|| self.pos += 1)
    }

    fn string(&mut self) -> Option<()> {
        self.eat(b'"')?;
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(());
                }
                b'\\' => self.pos += 2,
                _ => self.pos += 1,
            }
        }
    }

    /// Skip one value; it must end inside the prefix.
    fn value(&mut self) -> Option<()> {
        match self.peek()? {
            b'"' => self.string(),
            b'{' | b'[' => {
                let mut depth = 0_usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.pos += 1;
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
            }
            _ => {
                let start = self.pos;
                while matches!(self.peek()?, b'-' | b'+' | b'.' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
                {
                    self.pos += 1;
                }
                (self.pos > start).then_some(())
            }
        }
    }
}

// ------------------------------------------------------- line reader

/// One stdin read result.
#[derive(Debug)]
pub(crate) enum Line {
    /// A complete line (newline and a trailing CR removed) and its bytes'
    /// reservation.
    Complete(Vec<u8>, Reservation),
    /// A line over the bound: its first bytes, for id recovery; the rest
    /// was discarded unbuffered.
    Oversize(Vec<u8>),
    /// End of input.
    Eof,
}

/// Reads newline-delimited lines, reserving each line's bytes from the
/// budget before buffering them. Reads at most 8 KiB at a time.
pub(crate) struct LineReader<R> {
    input: R,
    chunk: Vec<u8>,
    start: usize,
    end: usize,
    max_line: usize,
}

const READ_CHUNK: usize = 8192;

impl<R: Read> LineReader<R> {
    pub(crate) fn new(input: R, max_line: usize) -> Self {
        Self {
            input,
            chunk: vec![0; READ_CHUNK],
            start: 0,
            end: 0,
            max_line,
        }
    }

    /// The next line. A budget error (shutdown) ends reading.
    pub(crate) fn next(
        &mut self,
        held: Reservation,
        wait: &Wait<'_>,
    ) -> io::Result<Result<Line, BudgetError>> {
        let mut held = held;
        let mut line: Vec<u8> = Vec::new();
        let mut oversize: Option<Vec<u8>> = None;
        loop {
            if self.start >= self.end {
                let n = loop {
                    match self.input.read(&mut self.chunk) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        other => break other?,
                    }
                };
                if n == 0 {
                    // A final line without a newline is still a line.
                    return Ok(Ok(match oversize {
                        Some(prefix) => Line::Oversize(prefix),
                        None if line.is_empty() => Line::Eof,
                        None => Line::Complete(strip_cr(line), held),
                    }));
                }
                self.start = 0;
                self.end = n;
            }
            let available = &self.chunk[self.start..self.end];
            let newline = available.iter().position(|b| *b == b'\n');
            let take = newline.unwrap_or(available.len());
            let piece = &available[..take];
            if let Some(prefix) = oversize.as_mut() {
                let room = ID_RECOVERY_PREFIX.saturating_sub(prefix.len());
                prefix.extend_from_slice(&piece[..room.min(piece.len())]);
            } else if line.len() + piece.len() > self.max_line {
                // Keep only a prefix for id recovery; drop the buffer.
                let mut prefix = std::mem::take(&mut line);
                prefix.truncate(ID_RECOVERY_PREFIX);
                let room = ID_RECOVERY_PREFIX.saturating_sub(prefix.len());
                prefix.extend_from_slice(&piece[..room.min(piece.len())]);
                held.shrink_to(0);
                oversize = Some(prefix);
            } else {
                if let Err(error) = held.grow(piece.len(), wait) {
                    return Ok(Err(error));
                }
                line.extend_from_slice(piece);
            }
            self.start += take;
            if newline.is_some() {
                self.start += 1;
                return Ok(Ok(match oversize {
                    Some(prefix) => Line::Oversize(prefix),
                    None => Line::Complete(strip_cr(line), held),
                }));
            }
        }
    }
}

fn strip_cr(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    #[test]
    fn to_line_matches_the_compact_form_and_its_length_is_exact() {
        for text in [
            r#"{"a":[1,-2,3.5,1e300,true,false,null,"x\"\\\n\t\u0001é😀"],"b":{}}"#,
            r#"[]"#,
            r#""  ""#,
            r#"0.1"#,
            r#"123456789012345678901234567890"#,
        ] {
            let value = parse_json(text).unwrap();
            let line = to_line(&value);
            assert_eq!(line.len(), compact_len(&value), "{text}");
            assert_eq!(parse_json(&line).unwrap(), value);
            assert_eq!(
                line,
                value
                    .to_compact_string()
                    .replace('\u{2028}', "\\u2028")
                    .replace('\u{2029}', "\\u2029")
            );
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn ids_are_recovered_from_a_prefix() {
        let id = |text: &str| recover_id(text.as_bytes());
        assert_eq!(
            id(r#"{"jsonrpc":"2.0","id":7,"method":"x","params":{"a":"#),
            Some(Json::Int(7))
        );
        assert_eq!(
            id(r#"{"params":{"id":1,"x":[{"}":"]"}]},"id":"abc","more"#),
            Some(Json::string("abc"))
        );
        assert_eq!(id(r#"{"method":"x","params":"aaaaaaaa"#), None);
        assert_eq!(id(r#"{"id":{"x":1}}"#), None);
        assert_eq!(id(r#"{"id":null}"#), None);
        assert_eq!(id(r#"[{"id":1}]"#), None);
        assert_eq!(id(&format!(r#"{{"id":"{}"}}"#, "a".repeat(300))), None);
    }

    #[test]
    fn classification() {
        let one = |text: &str| classify(&parse_json(text).unwrap());
        assert_eq!(
            one(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"n"}}"#),
            Ok(vec![ClientMessage::Request {
                id: Json::Int(1),
                method: "tools/call".into(),
                tool: Some("n".into())
            }])
        );
        assert_eq!(
            one(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#).unwrap()[0],
            ClientMessage::Request {
                id: Json::Int(1),
                method: "tools/call".into(),
                tool: Some(String::new())
            }
        );
        assert_eq!(
            one(r#"{"jsonrpc":"2.0","method":"n"}"#),
            Ok(vec![ClientMessage::Notification { method: "n".into() }])
        );
        assert_eq!(
            one(r#"{"jsonrpc":"2.0","id":"s","result":{}}"#),
            Ok(vec![ClientMessage::Response])
        );
        assert_eq!(one(r#"{"jsonrpc":"2.0","id":3}"#), Err(Some(Json::Int(3))));
        assert_eq!(one(r#"{"id":null,"method":"x"}"#), Err(None));
        assert_eq!(one(r#"{"method":5,"id":2}"#), Err(Some(Json::Int(2))));
        assert_eq!(one("[]"), Err(None));
        assert_eq!(one("3"), Err(None));
    }
}
