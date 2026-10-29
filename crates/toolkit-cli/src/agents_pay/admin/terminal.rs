//! The terminal seam of the creating commands (design section 7): whether
//! stdin is a terminal, and one line at a time. Prompts are written through
//! `Output::prompt` so they stay in order with the rest of stdout.

/// The interactive terminal.
pub trait Terminal {
    /// Whether stdin is a terminal (the legacy `sys.stdin.isatty()`).
    fn is_tty(&self) -> bool;
    /// One line without its `\n` or `\r\n` terminator; `None` at EOF or on
    /// a read error (D28).
    fn read_line(&self) -> Option<String>;
}
