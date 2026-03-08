use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileContentSignature {
    len_bytes: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct FileDiffCacheKey {
    pub(super) file_path: String,
    pub(super) msg_index: usize,
}

pub(super) struct FileDiffViewCacheEntry {
    pub(super) file_sig: Option<FileContentSignature>,
    pub(super) file_lines: Vec<Line<'static>>,
    pub(super) first_change_line: usize,
    pub(super) additions: usize,
    pub(super) deletions: usize,
}

#[derive(Default)]
pub(super) struct FileDiffViewCacheState {
    pub(super) entries: HashMap<FileDiffCacheKey, FileDiffViewCacheEntry>,
    pub(super) order: VecDeque<FileDiffCacheKey>,
}

impl FileDiffViewCacheState {
    pub(super) fn insert(&mut self, key: FileDiffCacheKey, entry: FileDiffViewCacheEntry) {
        if !self.entries.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, entry);

        while self.order.len() > FILE_DIFF_CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

const FILE_DIFF_CACHE_LIMIT: usize = 8;

static FILE_DIFF_CACHE: OnceLock<Mutex<FileDiffViewCacheState>> = OnceLock::new();

pub(super) fn file_diff_cache() -> &'static Mutex<FileDiffViewCacheState> {
    FILE_DIFF_CACHE.get_or_init(|| Mutex::new(FileDiffViewCacheState::default()))
}

pub(super) fn file_content_signature(file_path: &str) -> Option<FileContentSignature> {
    let metadata = std::fs::metadata(file_path).ok()?;
    Some(FileContentSignature {
        len_bytes: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn find_visible_edit_tool<'a>(
    edit_ranges: &'a [EditToolRange],
    scroll: usize,
    visible_height: usize,
) -> Option<&'a EditToolRange> {
    if edit_ranges.is_empty() {
        return None;
    }

    let visible_start = scroll;
    let visible_end = scroll + visible_height;
    let visible_mid = scroll + visible_height / 2;

    let mut best: Option<&EditToolRange> = None;
    let mut best_overlap = 0usize;
    let mut best_distance = usize::MAX;

    for range in edit_ranges {
        let overlap_start = range.start_line.max(visible_start);
        let overlap_end = range.end_line.min(visible_end);
        let overlap = if overlap_end > overlap_start {
            overlap_end - overlap_start
        } else {
            0
        };

        let range_mid = (range.start_line + range.end_line) / 2;
        let distance = if range_mid > visible_mid {
            range_mid - visible_mid
        } else {
            visible_mid - range_mid
        };

        if overlap > best_overlap || (overlap == best_overlap && distance < best_distance) {
            best = Some(range);
            best_overlap = overlap;
            best_distance = distance;
        }
    }

    best
}

pub(super) fn active_file_diff_context(
    prepared: &PreparedMessages,
    scroll: usize,
    visible_height: usize,
) -> Option<ActiveFileDiffContext> {
    let range = find_visible_edit_tool(&prepared.edit_tool_ranges, scroll, visible_height)?;
    let edit_index = prepared.edit_tool_ranges.iter().position(|candidate| {
        candidate.msg_index == range.msg_index
            && candidate.start_line == range.start_line
            && candidate.end_line == range.end_line
            && candidate.file_path == range.file_path
    })? + 1;

    Some(ActiveFileDiffContext {
        edit_index,
        msg_index: range.msg_index,
        file_path: range.file_path.clone(),
    })
}

pub(super) fn draw_file_diff_view(
    frame: &mut Frame,
    area: Rect,
    app: &dyn TuiState,
    prepared: &PreparedMessages,
    pane_scroll: usize,
    focused: bool,
) {
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    if area.width < 10 || area.height < 3 {
        return;
    }

    let scroll_offset = app.scroll_offset();
    let visible_height = area.height as usize;

    let scroll = if app.auto_scroll_paused() {
        scroll_offset
    } else {
        prepared.wrapped_lines.len().saturating_sub(visible_height)
    };

    let active_context = active_file_diff_context(prepared, scroll, visible_height);

    let Some(active_context) = active_context else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(dim_color()))
            .title(Line::from(vec![Span::styled(
                " file ",
                Style::default().fg(tool_color()),
            )]));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let msg = Paragraph::new(Line::from(Span::styled(
            "No edits visible",
            Style::default().fg(dim_color()),
        )));
        frame.render_widget(msg, inner);
        return;
    };

    let file_path = &active_context.file_path;
    let msg_index = active_context.msg_index;
    let cache_key = FileDiffCacheKey {
        file_path: file_path.clone(),
        msg_index,
    };
    let file_sig = file_content_signature(file_path);

    let mut cache = match file_diff_cache().lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };

    let needs_rebuild = cache
        .entries
        .get(&cache_key)
        .map(|cached| cached.file_sig != file_sig)
        .unwrap_or(true);

    if needs_rebuild {
        let display_messages = app.display_messages();
        let msg = display_messages.get(msg_index);

        let (diff_lines, file_content) = if let Some(msg) = msg {
            let tc = msg.tool_data.as_ref();
            let diffs = if let Some(tc) = tc {
                let from_content = collect_diff_lines(&msg.content);
                if !from_content.is_empty() {
                    from_content
                } else {
                    generate_diff_lines_from_tool_input(tc)
                }
            } else {
                Vec::new()
            };

            let content = std::fs::read_to_string(file_path).unwrap_or_default();
            (diffs, content)
        } else {
            (Vec::new(), String::new())
        };

        let file_ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str());

        struct DiffHunk {
            dels: Vec<String>,
            adds: Vec<String>,
        }

        let mut hunks: Vec<DiffHunk> = Vec::new();
        {
            let mut current_dels: Vec<String> = Vec::new();
            let mut current_adds: Vec<String> = Vec::new();
            for dl in &diff_lines {
                match dl.kind {
                    DiffLineKind::Del => {
                        if !current_adds.is_empty() {
                            hunks.push(DiffHunk {
                                dels: current_dels,
                                adds: current_adds,
                            });
                            current_dels = Vec::new();
                            current_adds = Vec::new();
                        }
                        current_dels.push(dl.content.clone());
                    }
                    DiffLineKind::Add => {
                        current_adds.push(dl.content.clone());
                    }
                }
            }
            if !current_dels.is_empty() || !current_adds.is_empty() {
                hunks.push(DiffHunk {
                    dels: current_dels,
                    adds: current_adds,
                });
            }
        }

        let mut add_to_dels: std::collections::HashMap<usize, Vec<String>> =
            std::collections::HashMap::new();
        let mut orphan_dels: Vec<String> = Vec::new();
        let file_lines_vec: Vec<&str> = file_content.lines().collect();

        let mut used_file_lines: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for hunk in &hunks {
            if hunk.adds.is_empty() {
                orphan_dels.extend(hunk.dels.clone());
                continue;
            }

            let first_add_trimmed = hunk.adds[0].trim();
            if first_add_trimmed.is_empty() {
                orphan_dels.extend(hunk.dels.clone());
                continue;
            }
            let mut found_idx = None;
            for (fi, fl) in file_lines_vec.iter().enumerate() {
                if !used_file_lines.contains(&fi) && fl.trim() == first_add_trimmed {
                    found_idx = Some(fi);
                    break;
                }
            }

            if let Some(idx) = found_idx {
                for (ai, _) in hunk.adds.iter().enumerate() {
                    used_file_lines.insert(idx + ai);
                }
                if !hunk.dels.is_empty() {
                    add_to_dels.insert(idx, hunk.dels.clone());
                }
            } else {
                orphan_dels.extend(hunk.dels.clone());
            }
        }

        let mut rendered_lines: Vec<Line<'static>> = Vec::new();
        let mut first_change_line = usize::MAX;
        let mut del_count = 0usize;
        let mut add_count = 0usize;

        let line_num_width = file_lines_vec.len().to_string().len().max(3);
        let gutter_pad: String = " ".repeat(line_num_width);

        for (i, line_text) in file_lines_vec.iter().enumerate() {
            let line_num = i + 1;

            if let Some(dels) = add_to_dels.get(&i) {
                for del_text in dels {
                    let mut del_spans: Vec<Span<'static>> = vec![Span::styled(
                        format!("{} │-", gutter_pad),
                        Style::default().fg(diff_del_color()),
                    )];
                    let highlighted = markdown::highlight_line(del_text, file_ext);
                    for span in highlighted {
                        let tinted = tint_span_with_diff_color(span, diff_del_color());
                        del_spans.push(tinted);
                    }
                    if first_change_line == usize::MAX {
                        first_change_line = rendered_lines.len();
                    }
                    del_count += 1;
                    rendered_lines.push(Line::from(del_spans));
                }
            }

            let is_added = used_file_lines.contains(&i);

            if is_added {
                let mut spans: Vec<Span<'static>> = vec![Span::styled(
                    format!("{:>width$} │+", line_num, width = line_num_width),
                    Style::default().fg(diff_add_color()),
                )];
                let highlighted = markdown::highlight_line(line_text, file_ext);
                for span in highlighted {
                    let tinted = tint_span_with_diff_color(span, diff_add_color());
                    spans.push(tinted);
                }
                if first_change_line == usize::MAX {
                    first_change_line = rendered_lines.len();
                }
                add_count += 1;
                rendered_lines.push(Line::from(spans));
            } else {
                let mut spans: Vec<Span<'static>> = vec![Span::styled(
                    format!("{:>width$} │ ", line_num, width = line_num_width),
                    Style::default().fg(dim_color()),
                )];
                let highlighted = markdown::highlight_line(line_text, file_ext);
                spans.extend(highlighted);
                rendered_lines.push(Line::from(spans));
            }
        }

        for del_text in &orphan_dels {
            let mut del_spans: Vec<Span<'static>> = vec![Span::styled(
                format!("{} │-", gutter_pad),
                Style::default().fg(diff_del_color()),
            )];
            let highlighted = markdown::highlight_line(del_text, file_ext);
            for span in highlighted {
                let tinted = tint_span_with_diff_color(span, diff_del_color());
                del_spans.push(tinted);
            }
            if first_change_line == usize::MAX {
                first_change_line = rendered_lines.len();
            }
            del_count += 1;
            rendered_lines.push(Line::from(del_spans));
        }

        if rendered_lines.is_empty() {
            rendered_lines.push(Line::from(Span::styled(
                "File not found or empty",
                Style::default().fg(dim_color()),
            )));
        }

        cache.insert(
            cache_key.clone(),
            FileDiffViewCacheEntry {
                file_sig: file_sig.clone(),
                file_lines: rendered_lines,
                first_change_line,
                additions: add_count,
                deletions: del_count,
            },
        );
    }

    let cached = cache
        .entries
        .get(&cache_key)
        .expect("file diff cache entry should exist after build");

    let short_path = file_path
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/");

    let mut title_parts = vec![
        Span::styled(" ", Style::default().fg(dim_color())),
        Span::styled(
            short_path,
            Style::default()
                .fg(rgb(180, 200, 255))
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
    ];
    if cached.additions > 0 || cached.deletions > 0 {
        title_parts.push(Span::styled(" ", Style::default().fg(dim_color())));
        if cached.additions > 0 {
            title_parts.push(Span::styled(
                format!("+{}", cached.additions),
                Style::default().fg(diff_add_color()),
            ));
        }
        if cached.deletions > 0 {
            if cached.additions > 0 {
                title_parts.push(Span::styled(" ", Style::default().fg(dim_color())));
            }
            title_parts.push(Span::styled(
                format!("-{}", cached.deletions),
                Style::default().fg(diff_del_color()),
            ));
        }
    }
    title_parts.push(Span::styled(
        format!(" {}L ", cached.file_lines.len()),
        Style::default().fg(dim_color()),
    ));
    title_parts.push(Span::styled(
        format!(" edit#{} ", active_context.edit_index),
        Style::default().fg(file_link_color()),
    ));

    let border_color = if focused { tool_color() } else { dim_color() };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(title_parts));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let total_lines = cached.file_lines.len();
    PINNED_PANE_TOTAL_LINES.store(total_lines, Ordering::Relaxed);

    let max_scroll = total_lines.saturating_sub(inner.height as usize);

    let effective_scroll = if pane_scroll == usize::MAX && cached.first_change_line != usize::MAX {
        let target = cached
            .first_change_line
            .saturating_sub(inner.height as usize / 3);
        target.min(max_scroll)
    } else if pane_scroll == usize::MAX {
        max_scroll
    } else {
        pane_scroll.min(max_scroll)
    };
    LAST_DIFF_PANE_EFFECTIVE_SCROLL.store(effective_scroll, Ordering::Relaxed);

    let visible_lines: Vec<Line<'static>> = cached
        .file_lines
        .iter()
        .skip(effective_scroll)
        .take(inner.height as usize)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);
}
