//! Shared formatting for streamed reasoning/thinking content.
//!
//! Reasoning is rendered as a self-contained markdown blockquote so it shows up
//! with a dim `│ ` gutter and a dim/italic body, plus a `Thought for Xs` footer,
//! instead of an emoji prefix that scatters when reasoning and output interleave.
//! The formatter is stateful across streamed deltas so that every line (including
//! the footer) stays inside one quote block.
//!
//! This is used by the server streaming paths (mpsc/broadcast) so that remote
//! clients receive ready-to-render markdown. The local TUI turn loop has an
//! equivalent implementation operating directly on its streaming buffer.

/// Incrementally formats reasoning deltas into blockquote markdown.
#[derive(Debug, Default)]
pub struct ReasoningStreamFormatter {
    /// Whether a reasoning blockquote is currently open.
    open: bool,
    /// Whether the next character starts a new line (and thus needs a `> `).
    at_line_start: bool,
}

impl ReasoningStreamFormatter {
    pub fn new() -> Self {
        Self {
            open: false,
            at_line_start: true,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Format a reasoning delta, opening the blockquote on first use. Returns the
    /// markdown text to emit, or an empty string for empty input.
    pub fn push_delta(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !self.open {
            self.open = true;
            self.at_line_start = true;
        }
        for ch in text.chars() {
            if self.at_line_start {
                out.push_str("> ");
                self.at_line_start = false;
            }
            out.push(ch);
            if ch == '\n' {
                self.at_line_start = true;
            }
        }
        out
    }

    /// Close the blockquote, optionally writing a footer line (e.g. the elapsed
    /// `*Thought for Xs*`) inside the quote, then a blank line so following text
    /// renders as a normal paragraph. Returns empty string if not open.
    pub fn finish(&mut self, footer: Option<&str>) -> String {
        if !self.open {
            return String::new();
        }
        let mut out = String::new();
        if !self.at_line_start {
            out.push('\n');
            self.at_line_start = true;
        }
        if let Some(footer) = footer {
            out.push_str("> ");
            out.push_str(footer);
            out.push('\n');
        }
        // Blank line terminates the blockquote in markdown.
        out.push('\n');
        self.open = false;
        self.at_line_start = true;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_lines_in_blockquote_without_header() {
        let mut f = ReasoningStreamFormatter::new();
        let mut s = f.push_delta("alpha\nbeta");
        s.push_str(&f.finish(Some("*Thought for 1.5s*")));
        assert!(!s.contains("Thinking"), "no header expected: {s:?}");
        assert!(s.contains("> alpha"));
        assert!(s.contains("> beta"));
        assert!(s.contains("> *Thought for 1.5s*"));
        assert!(s.ends_with("\n\n"));
    }

    #[test]
    fn no_header_and_continuation_stays_on_one_line() {
        let mut f = ReasoningStreamFormatter::new();
        let mut s = f.push_delta("one ");
        s.push_str(&f.push_delta("two"));
        assert!(!s.contains("Thinking"), "no header expected: {s:?}");
        // Mid-line continuation must not inject a stray gutter.
        assert!(s.contains("> one two"));
    }

    #[test]
    fn finish_without_open_is_empty() {
        let mut f = ReasoningStreamFormatter::new();
        assert_eq!(f.finish(None), "");
        assert!(!f.is_open());
    }

    #[test]
    fn delta_after_newline_gets_gutter() {
        let mut f = ReasoningStreamFormatter::new();
        let mut s = f.push_delta("line1\n");
        s.push_str(&f.push_delta("line2"));
        assert!(s.contains("> line1\n> line2"));
    }
}
