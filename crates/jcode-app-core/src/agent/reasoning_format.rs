//! Shared formatting for streamed reasoning/thinking content.
//!
//! Reasoning is rendered as dim, italic text (no blockquote gutter, no header,
//! no footer): each complete line is wrapped in `*…*` with an invisible
//! `REASONING_SENTINEL` prefix that the markdown renderer strips and styles dim.
//! The formatter is stateful across streamed deltas so emphasis markers always
//! wrap a whole line even when a line is split across deltas.
//!
//! This is used by the server streaming paths (mpsc/broadcast) so that remote
//! clients receive ready-to-render markdown. The local TUI turn loop has an
//! equivalent implementation operating directly on its streaming buffer.

use jcode_tui_markdown::reasoning_line_markup;

/// Incrementally formats reasoning deltas into dim+italic markdown lines.
#[derive(Debug, Default)]
pub struct ReasoningStreamFormatter {
    /// Whether a reasoning region is currently open.
    open: bool,
    /// Buffered trailing partial line awaiting its newline.
    pending_line: String,
}

impl ReasoningStreamFormatter {
    pub fn new() -> Self {
        Self {
            open: false,
            pending_line: String::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Markup for one complete reasoning line. Empty lines stay bare (no empty
    /// emphasis run). Embedded markdown is escaped so the dim/italic styling
    /// covers the whole line. Shared with the renderer via `jcode-tui-markdown`.
    fn line_markup(line: &str) -> String {
        reasoning_line_markup(line)
    }

    /// Format a reasoning delta, opening the region on first use. Complete lines
    /// are emitted immediately; a trailing partial line is buffered. Returns the
    /// markdown text to emit, or an empty string when nothing is ready yet.
    pub fn push_delta(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        self.open = true;
        let mut out = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut self.pending_line);
                out.push_str(&Self::line_markup(&line));
            } else {
                self.pending_line.push(ch);
            }
        }
        out
    }

    /// Close the region, flushing any buffered partial line, then a blank line so
    /// following text renders as a normal paragraph. The `_footer` argument is
    /// ignored (the "Thought for Xs" footer was removed) and kept for
    /// call-site compatibility. Returns an empty string if not open.
    pub fn finish(&mut self, _footer: Option<&str>) -> String {
        if !self.open {
            return String::new();
        }
        let mut out = String::new();
        let pending = std::mem::take(&mut self.pending_line);
        if !pending.is_empty() {
            out.push_str(&Self::line_markup(&pending));
        }
        // Blank line terminates the reasoning block.
        out.push('\n');
        self.open = false;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_tui_markdown::REASONING_SENTINEL;

    #[test]
    fn wraps_lines_dim_italic_without_header_or_gutter() {
        let mut f = ReasoningStreamFormatter::new();
        let mut s = f.push_delta("alpha\nbeta");
        s.push_str(&f.finish(None));
        assert!(!s.contains("Thinking"), "no header expected: {s:?}");
        assert!(!s.contains('>'), "no blockquote gutter expected: {s:?}");
        assert!(!s.contains("Thought for"), "no footer expected: {s:?}");
        // Each line wrapped in italic with the sentinel inside both ends.
        assert!(s.contains(&format!(
            "*{0}alpha{0}*",
            REASONING_SENTINEL
        )));
        assert!(s.contains(&format!("*{0}beta{0}*", REASONING_SENTINEL)));
        assert!(s.ends_with("\n\n"));
    }

    #[test]
    fn line_split_across_deltas_stays_one_emphasis_run() {
        let mut f = ReasoningStreamFormatter::new();
        let mut s = f.push_delta("one ");
        s.push_str(&f.push_delta("two\n"));
        // No emphasis emitted until the line completed; then a single run.
        assert!(
            s.contains(&format!("*{0}one two{0}*", REASONING_SENTINEL)),
            "{s:?}"
        );
    }

    #[test]
    fn finish_without_open_is_empty() {
        let mut f = ReasoningStreamFormatter::new();
        assert_eq!(f.finish(None), "");
        assert!(!f.is_open());
    }

    #[test]
    fn finish_flushes_pending_partial_line() {
        let mut f = ReasoningStreamFormatter::new();
        f.push_delta("trailing");
        let s = f.finish(None);
        assert!(
            s.contains(&format!("*{0}trailing{0}*", REASONING_SENTINEL)),
            "{s:?}"
        );
    }
}
