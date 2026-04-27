use crate::{session_launch::DesktopSessionEvent, workspace};
use workspace::{KeyInput, KeyOutcome};

pub(crate) const SINGLE_SESSION_FONT_FAMILY: &str = "JetBrainsMono Nerd Font";
pub(crate) const SINGLE_SESSION_FONT_WEIGHT: &str = "Light";
pub(crate) const SINGLE_SESSION_FONT_FALLBACKS: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono",
    "monospace",
];
pub(crate) const SINGLE_SESSION_TITLE_FONT_SIZE: f32 = 28.0;
pub(crate) const SINGLE_SESSION_BODY_FONT_SIZE: f32 = 22.0;
pub(crate) const SINGLE_SESSION_META_FONT_SIZE: f32 = 16.0;
pub(crate) const SINGLE_SESSION_CODE_FONT_SIZE: f32 = 21.0;
pub(crate) const SINGLE_SESSION_BODY_LINE_HEIGHT: f32 = 1.45;
pub(crate) const SINGLE_SESSION_CODE_LINE_HEIGHT: f32 = 1.35;
pub(crate) const SINGLE_SESSION_META_LINE_HEIGHT: f32 = 1.25;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SingleSessionTypography {
    pub(crate) family: &'static str,
    pub(crate) weight: &'static str,
    pub(crate) fallbacks: &'static [&'static str],
    pub(crate) title_size: f32,
    pub(crate) body_size: f32,
    pub(crate) meta_size: f32,
    pub(crate) code_size: f32,
    pub(crate) body_line_height: f32,
    pub(crate) code_line_height: f32,
    pub(crate) meta_line_height: f32,
}

pub(crate) const fn single_session_typography() -> SingleSessionTypography {
    SingleSessionTypography {
        family: SINGLE_SESSION_FONT_FAMILY,
        weight: SINGLE_SESSION_FONT_WEIGHT,
        fallbacks: SINGLE_SESSION_FONT_FALLBACKS,
        title_size: SINGLE_SESSION_TITLE_FONT_SIZE,
        body_size: SINGLE_SESSION_BODY_FONT_SIZE,
        meta_size: SINGLE_SESSION_META_FONT_SIZE,
        code_size: SINGLE_SESSION_CODE_FONT_SIZE,
        body_line_height: SINGLE_SESSION_BODY_LINE_HEIGHT,
        code_line_height: SINGLE_SESSION_CODE_LINE_HEIGHT,
        meta_line_height: SINGLE_SESSION_META_LINE_HEIGHT,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SingleSessionApp {
    pub(crate) session: Option<workspace::SessionCard>,
    pub(crate) draft: String,
    pub(crate) draft_cursor: usize,
    pub(crate) detail_scroll: usize,
    pub(crate) live_session_id: Option<String>,
    pub(crate) messages: Vec<SingleSessionMessage>,
    pub(crate) streaming_response: String,
    pub(crate) status: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) is_processing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SingleSessionMessage {
    role: &'static str,
    content: String,
}

impl SingleSessionApp {
    pub(crate) fn new(session: Option<workspace::SessionCard>) -> Self {
        Self {
            session,
            draft: String::new(),
            draft_cursor: 0,
            detail_scroll: 0,
            live_session_id: None,
            messages: Vec::new(),
            streaming_response: String::new(),
            status: None,
            error: None,
            is_processing: false,
        }
    }

    pub(crate) fn replace_session(&mut self, session: Option<workspace::SessionCard>) {
        self.session = session;
        if let Some(session) = &self.session {
            self.live_session_id = Some(session.session_id.clone());
        }
        self.detail_scroll = 0;
    }

    pub(crate) fn reset_fresh_session(&mut self) {
        self.session = None;
        self.draft.clear();
        self.draft_cursor = 0;
        self.detail_scroll = 0;
        self.live_session_id = None;
        self.messages.clear();
        self.streaming_response.clear();
        self.status = None;
        self.error = None;
        self.is_processing = false;
    }

    pub(crate) fn status_title(&self) -> String {
        let title = self.title();
        format!(
            "Jcode Desktop · single session · {title} · Ctrl+Enter send · Enter newline · Ctrl+; spawn · Ctrl+R refresh · Esc quit · --workspace for Niri layout"
        )
    }

    pub(crate) fn title(&self) -> String {
        if let Some(session) = &self.session {
            session.title.clone()
        } else if let Some(session_id) = &self.live_session_id {
            format!("session {}", short_session_id(session_id))
        } else {
            "fresh session".to_string()
        }
    }

    pub(crate) fn has_background_work(&self) -> bool {
        self.is_processing
    }

    pub(crate) fn handle_key(&mut self, key: KeyInput) -> KeyOutcome {
        match key {
            KeyInput::SpawnPanel => KeyOutcome::SpawnSession,
            KeyInput::RefreshSessions => KeyOutcome::Redraw,
            KeyInput::SubmitDraft => self.submit_draft(),
            KeyInput::Escape => KeyOutcome::Exit,
            KeyInput::Enter => {
                self.insert_draft_text("\n");
                KeyOutcome::Redraw
            }
            KeyInput::Backspace => {
                self.delete_previous_char();
                KeyOutcome::Redraw
            }
            KeyInput::DeletePreviousWord => {
                self.delete_previous_word();
                KeyOutcome::Redraw
            }
            KeyInput::DeleteNextChar => {
                self.delete_next_char();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorLeft => {
                self.move_cursor_left();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorRight => {
                self.move_cursor_right();
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineStart => {
                self.move_to_line_start();
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineEnd => {
                self.move_to_line_end();
                KeyOutcome::Redraw
            }
            KeyInput::DeleteToLineStart => {
                self.delete_to_line_start();
                KeyOutcome::Redraw
            }
            KeyInput::DeleteToLineEnd => {
                self.delete_to_line_end();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) => {
                self.insert_draft_text(&text);
                KeyOutcome::Redraw
            }
            _ => KeyOutcome::None,
        }
    }

    pub(crate) fn body_lines(&self) -> Vec<String> {
        if !self.messages.is_empty() || !self.streaming_response.is_empty() || self.error.is_some()
        {
            let mut lines = vec!["conversation".to_string()];
            for message in &self.messages {
                lines.push(format!("{}: {}", message.role, message.content.trim()));
            }
            if !self.streaming_response.is_empty() {
                lines.push(format!("assistant: {}", self.streaming_response.trim_end()));
            }
            if let Some(status) = &self.status {
                lines.push(format!("status: {status}"));
            }
            if let Some(error) = &self.error {
                lines.push(format!("error: {error}"));
            }
            return lines;
        }

        single_session_lines(self.session.as_ref())
    }

    pub(crate) fn apply_session_event(&mut self, event: DesktopSessionEvent) {
        match event {
            DesktopSessionEvent::Status(status) => self.status = Some(status),
            DesktopSessionEvent::SessionStarted { session_id } => {
                self.live_session_id = Some(session_id);
                self.status = Some("connected".to_string());
            }
            DesktopSessionEvent::TextDelta(text) => {
                self.streaming_response.push_str(&text);
                self.status = Some("receiving".to_string());
            }
            DesktopSessionEvent::TextReplace(text) => {
                self.streaming_response = text;
                self.status = Some("receiving".to_string());
            }
            DesktopSessionEvent::Done => {
                self.finish_streaming_response();
                self.is_processing = false;
                self.status = Some("ready".to_string());
            }
            DesktopSessionEvent::Error(error) => {
                self.finish_streaming_response();
                self.is_processing = false;
                self.status = Some("error".to_string());
                self.error = Some(error);
            }
        }
    }

    pub(crate) fn draft_cursor_line_col(&self) -> (usize, usize) {
        let before_cursor = &self.draft[..self.draft_cursor.min(self.draft.len())];
        let line = before_cursor.chars().filter(|ch| *ch == '\n').count();
        let column = before_cursor
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        (line, column)
    }

    fn submit_draft(&mut self) -> KeyOutcome {
        let message = self.draft.trim().to_string();
        if message.is_empty() {
            return KeyOutcome::None;
        }
        self.record_user_submit(&message);
        let Some(session) = &self.session else {
            return KeyOutcome::StartFreshSession { message };
        };
        let session_id = session.session_id.clone();
        let title = session.title.clone();
        KeyOutcome::SendDraft {
            session_id,
            title,
            message,
        }
    }

    fn record_user_submit(&mut self, message: &str) {
        self.messages.push(SingleSessionMessage {
            role: "user",
            content: message.to_string(),
        });
        self.draft.clear();
        self.draft_cursor = 0;
        self.streaming_response.clear();
        self.status = Some("sending".to_string());
        self.error = None;
        self.is_processing = true;
    }

    fn finish_streaming_response(&mut self) {
        let response = self.streaming_response.trim().to_string();
        if !response.is_empty() {
            self.messages.push(SingleSessionMessage {
                role: "assistant",
                content: response,
            });
        }
        self.streaming_response.clear();
    }

    fn insert_draft_text(&mut self, text: &str) {
        self.clamp_draft_cursor();
        self.draft.insert_str(self.draft_cursor, text);
        self.draft_cursor += text.len();
    }

    fn delete_previous_char(&mut self) {
        self.clamp_draft_cursor();
        if self.draft_cursor == 0 {
            return;
        }
        let previous = previous_char_boundary(&self.draft, self.draft_cursor);
        self.draft.replace_range(previous..self.draft_cursor, "");
        self.draft_cursor = previous;
    }

    fn delete_next_char(&mut self) {
        self.clamp_draft_cursor();
        if self.draft_cursor >= self.draft.len() {
            return;
        }
        let next = next_char_boundary(&self.draft, self.draft_cursor);
        self.draft.replace_range(self.draft_cursor..next, "");
    }

    fn delete_previous_word(&mut self) {
        self.clamp_draft_cursor();
        let start = previous_word_start(&self.draft, self.draft_cursor);
        self.draft.replace_range(start..self.draft_cursor, "");
        self.draft_cursor = start;
    }

    fn move_cursor_left(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = previous_char_boundary(&self.draft, self.draft_cursor);
    }

    fn move_cursor_right(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = next_char_boundary(&self.draft, self.draft_cursor);
    }

    fn move_to_line_start(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = line_start(&self.draft, self.draft_cursor);
    }

    fn move_to_line_end(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = line_end(&self.draft, self.draft_cursor);
    }

    fn delete_to_line_start(&mut self) {
        self.clamp_draft_cursor();
        let start = line_start(&self.draft, self.draft_cursor);
        self.draft.replace_range(start..self.draft_cursor, "");
        self.draft_cursor = start;
    }

    fn delete_to_line_end(&mut self) {
        self.clamp_draft_cursor();
        let end = line_end(&self.draft, self.draft_cursor);
        self.draft.replace_range(self.draft_cursor..end, "");
    }

    fn clamp_draft_cursor(&mut self) {
        self.draft_cursor = self.draft_cursor.min(self.draft.len());
        while !self.draft.is_char_boundary(self.draft_cursor) {
            self.draft_cursor -= 1;
        }
    }
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| cursor + offset)
        .unwrap_or(text.len())
}

fn previous_word_start(text: &str, cursor: usize) -> usize {
    let mut start = cursor.min(text.len());
    while start > 0 {
        let previous = previous_char_boundary(text, start);
        let ch = text[previous..start].chars().next().unwrap_or_default();
        if !ch.is_whitespace() {
            break;
        }
        start = previous;
    }
    while start > 0 {
        let previous = previous_char_boundary(text, start);
        let ch = text[previous..start].chars().next().unwrap_or_default();
        if ch.is_whitespace() {
            break;
        }
        start = previous;
    }
    start
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor.min(text.len())..]
        .find('\n')
        .map(|offset| cursor + offset)
        .unwrap_or(text.len())
}

fn short_session_id(session_id: &str) -> &str {
    session_id
        .strip_prefix("session_")
        .and_then(|rest| rest.split('_').next())
        .filter(|name| !name.is_empty())
        .unwrap_or(session_id)
}

pub(crate) fn single_session_surface(
    session: Option<&workspace::SessionCard>,
) -> workspace::Surface {
    let lines = single_session_lines(session);
    workspace::Surface {
        id: 1,
        title: session
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "new jcode session".to_string()),
        body_lines: lines.clone(),
        detail_lines: lines,
        session_id: session.map(|session| session.session_id.clone()),
        lane: 0,
        column: 0,
        color_index: 0,
    }
}

pub(crate) fn single_session_lines(session: Option<&workspace::SessionCard>) -> Vec<String> {
    let Some(session) = session else {
        return vec![
            "single session mode".to_string(),
            "fresh desktop-native session draft".to_string(),
            "type here without nav or insert modes".to_string(),
            "ctrl+enter will send once desktop-native execution is connected".to_string(),
            "ctrl+; clears this draft and starts another fresh desktop session".to_string(),
            "run with --workspace for the niri layout wrapper".to_string(),
        ];
    };

    let mut lines = vec![
        "single session mode".to_string(),
        session.subtitle.clone(),
        session.detail.clone(),
    ];
    if !session.preview_lines.is_empty() {
        lines.push("recent transcript".to_string());
        lines.extend(session.preview_lines.clone());
    }
    if !session.detail_lines.is_empty() {
        lines.push("expanded transcript".to_string());
        lines.extend(session.detail_lines.clone());
    }
    lines
}
