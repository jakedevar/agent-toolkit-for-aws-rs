//! A bounded Server-Sent Events parser (the `text/event-stream` body of an
//! MCP Streamable HTTP response).
//!
//! It follows the WHATWG event-stream rules the proxy needs and nothing more:
//!
//! - Lines end with CRLF, LF or CR. A CR at the end of a chunk is held until
//!   the next byte shows whether an LF follows, so a CRLF split across chunks
//!   is one line end.
//! - A leading UTF-8 byte-order mark is skipped.
//! - `data:` lines are joined with LF; a blank line dispatches the event. An
//!   event with no `data` field is not dispatched.
//! - Comments (`:` lines) and every other field (`event`, `id`, `retry`) are
//!   ignored. No field is ever used as a URL or a reconnection delay.
//! - An incomplete event at the end of the stream is discarded.
//!
//! Bounds: a line longer than `max_line` bytes is [`SseError::LineTooLong`]
//! and an event whose joined data exceeds `max_event` bytes is
//! [`SseError::EventTooLarge`]. Both are checked as bytes arrive, before
//! buffers grow past them, and the parser refuses everything after an error.
//! [`SseParser::buffered`] is what the caller keeps reserved from the R-MEM
//! budget.

/// Why the parser stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseError {
    /// A line passed its bound.
    LineTooLong,
    /// One event's data passed its bound.
    EventTooLarge,
}

/// The parser state. Bytes in, complete event data out.
#[derive(Debug)]
pub struct SseParser {
    max_line: usize,
    max_event: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    after_cr: bool,
    started: bool,
    failed: Option<SseError>,
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

impl SseParser {
    /// A parser with the given bounds.
    #[must_use]
    pub fn new(max_line: usize, max_event: usize) -> Self {
        Self {
            max_line,
            max_event,
            line: Vec::new(),
            data: Vec::new(),
            has_data: false,
            after_cr: false,
            started: false,
            failed: None,
        }
    }

    /// Bytes held in partial lines and undispatched data.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.line.len() + self.data.len()
    }

    /// Feed one chunk; every event it completes is appended to `events`.
    ///
    /// # Errors
    /// [`SseError`] when a bound is passed, and on every call after that.
    pub fn feed(&mut self, chunk: &[u8], events: &mut Vec<Vec<u8>>) -> Result<(), SseError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.feed_inner(chunk, events);
        if let Err(error) = result {
            self.failed = Some(error);
            self.line = Vec::new();
            self.data = Vec::new();
        }
        result
    }

    fn feed_inner(&mut self, chunk: &[u8], events: &mut Vec<Vec<u8>>) -> Result<(), SseError> {
        let mut rest = chunk;
        if !self.started {
            // Hold back a possible partial byte-order mark.
            let mut probe = self.line.clone();
            probe.extend_from_slice(rest);
            if probe.len() < BOM.len() && BOM.starts_with(&probe) {
                self.line = probe;
                return Ok(());
            }
            self.started = true;
            let stripped = probe.strip_prefix(BOM).unwrap_or(&probe).to_vec();
            self.line.clear();
            return self.scan(&stripped, events);
        }
        if rest.is_empty() {
            return Ok(());
        }
        if self.after_cr {
            self.after_cr = false;
            if rest[0] == b'\n' {
                rest = &rest[1..];
            }
        }
        self.scan(rest, events)
    }

    fn scan(&mut self, mut rest: &[u8], events: &mut Vec<Vec<u8>>) -> Result<(), SseError> {
        while let Some(end) = rest.iter().position(|b| *b == b'\n' || *b == b'\r') {
            self.push_line_bytes(&rest[..end])?;
            let line = std::mem::take(&mut self.line);
            self.process_line(&line, events)?;
            if rest[end] == b'\r' {
                match rest.get(end + 1) {
                    Some(b'\n') => rest = &rest[end + 2..],
                    Some(_) => rest = &rest[end + 1..],
                    None => {
                        self.after_cr = true;
                        rest = &[];
                    }
                }
            } else {
                rest = &rest[end + 1..];
            }
        }
        self.push_line_bytes(rest)
    }

    fn push_line_bytes(&mut self, bytes: &[u8]) -> Result<(), SseError> {
        if self.line.len() + bytes.len() > self.max_line {
            return Err(SseError::LineTooLong);
        }
        self.line.extend_from_slice(bytes);
        Ok(())
    }

    fn process_line(&mut self, line: &[u8], events: &mut Vec<Vec<u8>>) -> Result<(), SseError> {
        if line.is_empty() {
            if self.has_data {
                let mut data = std::mem::take(&mut self.data);
                if data.last() == Some(&b'\n') {
                    data.pop();
                }
                events.push(data);
            }
            self.data.clear();
            self.has_data = false;
            return Ok(());
        }
        if line[0] == b':' {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|b| *b == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &b""[..]),
        };
        if field == b"data" {
            // `data` ends with the LF that separates this value from the
            // previous one, so this is the joined length after the append
            // (the final LF is removed at dispatch and never counts).
            if self.data.len() + value.len() > self.max_event {
                return Err(SseError::EventTooLarge);
            }
            self.data.extend_from_slice(value);
            self.data.push(b'\n');
            self.has_data = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_chunks(chunks: &[&[u8]]) -> Result<Vec<String>, SseError> {
        let mut parser = SseParser::new(1024, 1024);
        let mut events = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut events)?;
        }
        Ok(events
            .into_iter()
            .map(|e| String::from_utf8(e).unwrap())
            .collect())
    }

    #[test]
    fn data_lines_join_with_lf_and_a_blank_line_dispatches() {
        assert_eq!(
            parse_chunks(&[b"data: {\"a\":\ndata: 1}\n\n"]).unwrap(),
            vec!["{\"a\":\n1}".to_string()]
        );
    }

    #[test]
    fn crlf_cr_and_lf_all_end_lines() {
        assert_eq!(
            parse_chunks(&[b"data: a\r\n\r\ndata: b\r\rdata: c\n\n"]).unwrap(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn comments_and_other_fields_are_ignored() {
        assert_eq!(
            parse_chunks(&[b": keep-alive\nevent: message\nid: 7\nretry: 1\ndata:x\n\n"]).unwrap(),
            vec!["x"]
        );
        // An event with no data field is not dispatched.
        assert!(parse_chunks(&[b"event: message\nid: 9\n\n"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_event_split_byte_by_byte_parses_the_same() {
        let stream = "\u{FEFF}data: {\"t\":\"é\u{2028}\"}\r\n\r\n";
        let bytes = stream.as_bytes();
        let chunks: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(
            parse_chunks(&chunks).unwrap(),
            vec!["{\"t\":\"é\u{2028}\"}".to_string()]
        );
    }

    #[test]
    fn a_crlf_split_across_chunks_is_one_line_end() {
        assert_eq!(
            parse_chunks(&[b"data: a\r", b"\n\r", b"\ndata: b\n\n"]).unwrap(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn an_incomplete_event_at_the_end_is_not_dispatched() {
        assert!(parse_chunks(&[b"data: never\n"]).unwrap().is_empty());
    }

    #[test]
    fn line_bound_at_limit_and_limit_plus_one() {
        let mut parser = SseParser::new(10, 100);
        let mut events = Vec::new();
        // "data: abcd" is 10 bytes.
        parser.feed(b"data: abcd\n\n", &mut events).unwrap();
        assert_eq!(events, vec![b"abcd".to_vec()]);
        let mut parser = SseParser::new(10, 100);
        assert_eq!(
            parser.feed(b"data: abcde", &mut events),
            Err(SseError::LineTooLong)
        );
        assert_eq!(
            parser.feed(b"\n\n", &mut events),
            Err(SseError::LineTooLong)
        );
        assert_eq!(parser.buffered(), 0);
    }

    #[test]
    fn event_bound_at_limit_and_limit_plus_one() {
        let mut parser = SseParser::new(100, 5);
        let mut events = Vec::new();
        parser.feed(b"data: ab\ndata: cd\n\n", &mut events).unwrap();
        assert_eq!(events, vec![b"ab\ncd".to_vec()]);
        let mut parser = SseParser::new(100, 5);
        assert_eq!(
            parser.feed(b"data: ab\ndata: cde\n", &mut events),
            Err(SseError::EventTooLarge)
        );
    }

    #[test]
    fn buffered_counts_partial_lines_and_pending_data() {
        let mut parser = SseParser::new(100, 100);
        let mut events = Vec::new();
        parser.feed(b"data: abc\nda", &mut events).unwrap();
        assert_eq!(parser.buffered(), 4 + 2);
        parser.feed(b"ta: d\n\n", &mut events).unwrap();
        assert_eq!(parser.buffered(), 0);
        assert_eq!(events, vec![b"abc\nd".to_vec()]);
    }
}
