use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SingleSessionTextKey {
    pub(crate) size: (u32, u32),
    pub(crate) fresh_welcome_visible: bool,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) welcome_hero: String,
    pub(crate) welcome_hint: Vec<SingleSessionStyledLine>,
    pub(crate) activity_active: bool,
    pub(crate) welcome_handoff_visible: bool,
    pub(crate) text_scale_bits: u32,
    pub(crate) body: Vec<SingleSessionStyledLine>,
    pub(crate) draft: String,
    pub(crate) status: String,
}

#[cfg(test)]
pub(crate) fn build_single_session_vertices(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    focus_pulse: f32,
    spinner_tick: u64,
) -> Vec<Vertex> {
    build_single_session_vertices_with_scroll(app, size, focus_pulse, spinner_tick, 0.0)
}

#[cfg(test)]
pub(crate) fn build_single_session_vertices_with_scroll(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    focus_pulse: f32,
    spinner_tick: u64,
    smooth_scroll_lines: f32,
) -> Vec<Vertex> {
    let welcome_hero_reveal_progress = welcome_hero_reveal_progress_for_tick(spinner_tick);
    build_single_session_vertices_with_scroll_and_reveal(
        app,
        size,
        focus_pulse,
        spinner_tick,
        smooth_scroll_lines,
        welcome_hero_reveal_progress,
    )
}

pub(crate) fn build_single_session_vertices_with_scroll_and_reveal(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    focus_pulse: f32,
    spinner_tick: u64,
    smooth_scroll_lines: f32,
    welcome_hero_reveal_progress: f32,
) -> Vec<Vertex> {
    let width = size.width as f32;
    let height = size.height as f32;
    let mut vertices = Vec::new();

    push_gradient_rect(
        &mut vertices,
        Rect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
        BACKGROUND_TOP_LEFT,
        BACKGROUND_BOTTOM_LEFT,
        BACKGROUND_BOTTOM_RIGHT,
        BACKGROUND_TOP_RIGHT,
        size,
    );

    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width: width.max(1.0),
        height: height.max(1.0),
    };
    let surface = single_session_surface(app.session.as_ref());
    push_single_session_surface_without_bottom_rule(
        &mut vertices,
        rect,
        surface.color_index,
        focus_pulse,
        size,
    );

    let welcome_chrome_offset = if app.is_welcome_timeline_visible() {
        welcome_timeline_visual_offset_pixels(app, size, smooth_scroll_lines)
    } else {
        0.0
    };
    if welcome_timeline_chrome_visible(app, size, welcome_chrome_offset) {
        push_fresh_welcome_ambient(&mut vertices, size, spinner_tick, welcome_chrome_offset);
        push_handwritten_welcome_hero_with_offset(
            &mut vertices,
            &app.welcome_hero_text(),
            size,
            app.text_scale(),
            welcome_hero_reveal_progress,
            welcome_chrome_offset,
        );
    }

    if app.has_activity_indicator() {
        push_native_activity_spinner(&mut vertices, app, size, spinner_tick);
    }
    push_single_session_transcript_cards(
        &mut vertices,
        app,
        size,
        spinner_tick,
        smooth_scroll_lines,
    );
    push_single_session_streaming_shimmer(
        &mut vertices,
        app,
        size,
        spinner_tick,
        smooth_scroll_lines,
    );
    push_single_session_selection(&mut vertices, app, size);
    push_single_session_scrollbar(&mut vertices, app, size, spinner_tick, smooth_scroll_lines);

    vertices
}

pub(crate) fn build_single_session_vertices_with_cached_body(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    focus_pulse: f32,
    spinner_tick: u64,
    smooth_scroll_lines: f32,
    welcome_hero_reveal_progress: f32,
    rendered_body_lines: &[SingleSessionStyledLine],
) -> Vec<Vertex> {
    let width = size.width as f32;
    let height = size.height as f32;
    let mut vertices = Vec::with_capacity(2048);

    push_gradient_rect(
        &mut vertices,
        Rect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
        BACKGROUND_TOP_LEFT,
        BACKGROUND_BOTTOM_LEFT,
        BACKGROUND_BOTTOM_RIGHT,
        BACKGROUND_TOP_RIGHT,
        size,
    );

    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width: width.max(1.0),
        height: height.max(1.0),
    };
    let surface = single_session_surface(app.session.as_ref());
    push_single_session_surface_without_bottom_rule(
        &mut vertices,
        rect,
        surface.color_index,
        focus_pulse,
        size,
    );

    let welcome_chrome_offset = if app.is_welcome_timeline_visible() {
        welcome_timeline_visual_offset_pixels_for_total_lines(
            app,
            size,
            smooth_scroll_lines,
            rendered_body_lines.len(),
        )
    } else {
        0.0
    };
    if welcome_timeline_chrome_visible(app, size, welcome_chrome_offset) {
        push_fresh_welcome_ambient(&mut vertices, size, spinner_tick, welcome_chrome_offset);
        push_handwritten_welcome_hero_with_offset(
            &mut vertices,
            &app.welcome_hero_text(),
            size,
            app.text_scale(),
            welcome_hero_reveal_progress,
            welcome_chrome_offset,
        );
    }

    if app.has_activity_indicator() {
        push_native_activity_spinner(&mut vertices, app, size, spinner_tick);
    }

    let viewport = single_session_body_viewport_from_lines(
        app,
        size,
        smooth_scroll_lines,
        rendered_body_lines,
    );
    push_single_session_transcript_cards_from_viewport(
        &mut vertices,
        app,
        size,
        &viewport,
        rendered_body_lines.len(),
    );
    push_single_session_streaming_shimmer_from_viewport(
        &mut vertices,
        app,
        size,
        spinner_tick,
        &viewport,
    );
    push_single_session_selection(&mut vertices, app, size);
    push_single_session_scrollbar_for_total_lines(
        &mut vertices,
        app,
        size,
        smooth_scroll_lines,
        rendered_body_lines.len(),
    );

    vertices
}

#[cfg(test)]
pub(crate) fn welcome_hero_reveal_progress_for_tick(spinner_tick: u64) -> f32 {
    let elapsed =
        Duration::from_millis(spinner_tick.saturating_mul(DESKTOP_SPINNER_FRAME_MS as u64));
    welcome_hero_reveal_progress_for_elapsed(elapsed)
}

pub(crate) fn welcome_hero_reveal_progress_for_elapsed(elapsed: Duration) -> f32 {
    const REVEAL_DURATION: Duration = Duration::from_millis(1350);
    const FIRST_INK_PROGRESS: f32 = 0.018;

    let raw = (elapsed.as_secs_f32() / REVEAL_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    if raw >= 1.0 {
        return 1.0;
    }

    let eased = ease_in_out_cubic(raw);
    FIRST_INK_PROGRESS + (1.0 - FIRST_INK_PROGRESS) * eased
}

pub(crate) fn welcome_hero_reveal_is_active(progress: f32) -> bool {
    progress < 0.999
}

fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn push_single_session_surface_without_bottom_rule(
    vertices: &mut Vec<Vertex>,
    rect: Rect,
    color_index: usize,
    focus_pulse: f32,
    size: PhysicalSize<u32>,
) {
    let accent = panel_accent_color(color_index, true);
    push_rounded_rect(
        vertices,
        rect,
        PANEL_RADIUS,
        with_alpha(accent, 0.105),
        size,
    );
    push_rounded_rect(
        vertices,
        Rect {
            x: rect.x,
            y: rect.y,
            width: 5.0_f32.min(rect.width),
            height: rect.height,
        },
        PANEL_RADIUS,
        with_alpha(accent, 0.78),
        size,
    );

    let stroke_width = FOCUSED_BORDER_WIDTH + focus_pulse * 2.5;
    push_top_and_side_surface_outline(vertices, rect, stroke_width, accent, size);

    if focus_pulse > 0.0 {
        let pulse_rect = inset_rect(rect, -3.0 * focus_pulse);
        push_top_and_side_surface_outline(
            vertices,
            pulse_rect,
            1.0,
            with_alpha(FOCUS_RING_COLOR, 0.32 * focus_pulse),
            size,
        );
    }
}

fn push_top_and_side_surface_outline(
    vertices: &mut Vec<Vertex>,
    rect: Rect,
    stroke_width: f32,
    color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    let stroke_width = stroke_width.max(1.0).min(rect.width).min(rect.height);
    push_rect(
        vertices,
        Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: stroke_width,
        },
        color,
        size,
    );
    push_rect(
        vertices,
        Rect {
            x: rect.x,
            y: rect.y,
            width: stroke_width,
            height: rect.height,
        },
        color,
        size,
    );
    push_rect(
        vertices,
        Rect {
            x: rect.x + rect.width - stroke_width,
            y: rect.y,
            width: stroke_width,
            height: rect.height,
        },
        color,
        size,
    );
}

fn push_fresh_welcome_ambient(
    vertices: &mut Vec<Vertex>,
    size: PhysicalSize<u32>,
    tick: u64,
    y_offset: f32,
) {
    let draft_top = single_session_draft_top(size);
    let usable_height = (draft_top - PANEL_BODY_TOP_PADDING).max(180.0);
    let t = tick as f32 * 0.055;

    push_aurora_ribbon(
        vertices,
        size,
        PANEL_BODY_TOP_PADDING + usable_height * 0.18 + (t * 0.60).sin() * 18.0 + y_offset,
        usable_height * 0.30,
        t * 0.85,
        WELCOME_AURORA_BLUE,
        WELCOME_AURORA_VIOLET,
    );
    push_aurora_ribbon(
        vertices,
        size,
        PANEL_BODY_TOP_PADDING + usable_height * 0.39 + (t * 0.47).cos() * 24.0 + y_offset,
        usable_height * 0.34,
        t * -0.72 + 1.8,
        WELCOME_AURORA_MINT,
        WELCOME_AURORA_BLUE,
    );
    push_aurora_ribbon(
        vertices,
        size,
        PANEL_BODY_TOP_PADDING + usable_height * 0.58 + (t * 0.52).sin() * 16.0 + y_offset,
        usable_height * 0.24,
        t * 0.64 + 3.2,
        WELCOME_AURORA_WARM,
        WELCOME_AURORA_MINT,
    );
}

fn push_handwritten_welcome_hero_with_offset(
    vertices: &mut Vec<Vertex>,
    phrase: &str,
    size: PhysicalSize<u32>,
    ui_scale: f32,
    reveal_progress: f32,
    y_offset: f32,
) {
    if !welcome_hero_approx_bounds_visible(size, ui_scale, y_offset) {
        return;
    }

    let paths = handwritten_welcome_paths_for_phrase(phrase);
    let total_length = stroke_paths_length(&paths);
    if total_length <= 0.0 {
        return;
    }

    let (bounds_min, bounds_max) =
        handwritten_welcome_bounds_for_phrase_with_scale(size, phrase, ui_scale);
    let bounds_min = [bounds_min[0], bounds_min[1] + y_offset];
    let bounds_max = [bounds_max[0], bounds_max[1] + y_offset];
    let (source_min, source_max) = stroke_paths_bounds(&paths);
    let source_width = (source_max[0] - source_min[0]).max(1.0);
    let scale = (bounds_max[0] - bounds_min[0]) / source_width;
    let origin = [
        bounds_min[0] - source_min[0] * scale,
        bounds_min[1] - source_min[1] * scale,
    ];
    let thickness = (scale * 0.075).clamp(3.6, 8.5);
    let mut remaining = total_length * reveal_progress.clamp(0.0, 1.0);
    let mut lead = None;

    for path in &paths {
        for pair in path.windows(2) {
            let a = pair[0];
            let b = pair[1];
            let segment_length = distance(a, b);
            if segment_length <= 0.001 {
                continue;
            }
            if remaining <= 0.0 {
                break;
            }
            let draw_fraction = (remaining / segment_length).clamp(0.0, 1.0);
            let end = lerp_point(a, b, draw_fraction);
            let pa = transform_handwriting_point(a, origin, scale);
            let pb = transform_handwriting_point(end, origin, scale);
            push_stroke_segment(vertices, pa, pb, thickness, WELCOME_HANDWRITING_COLOR, size);
            lead = Some(pb);
            remaining -= segment_length;
            if draw_fraction < 1.0 {
                break;
            }
        }
    }

    if let Some(point) = lead
        && (0.01..0.995).contains(&reveal_progress)
    {
        push_stroke_dot(
            vertices,
            point,
            thickness * 1.65,
            WELCOME_HANDWRITING_HIGHLIGHT_COLOR,
            size,
        );
    }
}

fn welcome_timeline_chrome_visible(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    y_offset: f32,
) -> bool {
    app.is_welcome_timeline_visible()
        && (!app.has_welcome_timeline_transcript()
            || welcome_hero_approx_bounds_visible(size, app.text_scale(), y_offset))
}

fn welcome_hero_approx_bounds_visible(
    size: PhysicalSize<u32>,
    ui_scale: f32,
    y_offset: f32,
) -> bool {
    let body_top = PANEL_BODY_TOP_PADDING;
    let draft_top = single_session_draft_top(size);
    let top = body_top + (draft_top - body_top) * 0.18 + y_offset;
    let bottom = body_top + (draft_top - body_top) * 0.74 * ui_scale + y_offset;
    bottom >= -64.0 && top <= size.height as f32 + 64.0
}

fn welcome_timeline_visual_offset_pixels(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
) -> f32 {
    welcome_timeline_visual_offset_pixels_for_total_lines(
        app,
        size,
        smooth_scroll_lines,
        welcome_timeline_total_body_lines(app, size),
    )
}

fn welcome_timeline_visual_offset_pixels_for_total_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    total_lines: usize,
) -> f32 {
    if !app.is_welcome_timeline_visible() || !app.has_welcome_timeline_transcript() {
        return 0.0;
    }

    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let body_top = single_session_body_top_for_app(app, size);
    let body_bottom = single_session_body_bottom_for_app(app, size);
    let visible_lines = (((body_bottom - body_top).max(line_height)) / line_height)
        .floor()
        .max(1.0);
    let total_lines = total_lines as f32;
    if total_lines <= visible_lines {
        return 0.0;
    }

    let max_scroll = (total_lines - visible_lines).max(0.0);
    let scroll = (app.body_scroll_lines + smooth_scroll_lines).clamp(0.0, max_scroll);
    let top_line = (total_lines - scroll - visible_lines).max(0.0);
    -top_line * line_height
}

#[cfg(test)]
pub(crate) fn handwritten_welcome_bounds(size: PhysicalSize<u32>) -> ([f32; 2], [f32; 2]) {
    handwritten_welcome_bounds_for_phrase(size, handwritten_welcome_phrase(0))
}

#[cfg(test)]
fn handwritten_welcome_bounds_for_phrase(
    size: PhysicalSize<u32>,
    phrase: &str,
) -> ([f32; 2], [f32; 2]) {
    handwritten_welcome_bounds_for_phrase_with_scale(size, phrase, 1.0)
}

fn handwritten_welcome_bounds_for_phrase_with_scale(
    size: PhysicalSize<u32>,
    phrase: &str,
    ui_scale: f32,
) -> ([f32; 2], [f32; 2]) {
    let paths = handwritten_welcome_paths_for_phrase(phrase);
    let (source_min, source_max) = stroke_paths_bounds(&paths);
    let source_width = (source_max[0] - source_min[0]).max(1.0);
    let source_height = (source_max[1] - source_min[1]).max(1.0);
    let normal_draft_top = single_session_draft_top(size);
    let target_width = size.width as f32 * 0.52 * ui_scale;
    let scale = target_width / source_width;
    let left = (size.width as f32 - target_width) * 0.5;
    let top = PANEL_BODY_TOP_PADDING + (normal_draft_top - PANEL_BODY_TOP_PADDING) * 0.31;
    (
        [left, top],
        [left + target_width, top + source_height * scale],
    )
}

fn handwritten_welcome_paths_for_phrase(phrase: &str) -> Vec<Vec<[f32; 2]>> {
    match phrase.trim().to_ascii_lowercase().as_str() {
        "hi there" => handwritten_hi_there_paths(),
        "hey there" => handwritten_hey_there_paths(),
        _ => handwritten_hello_there_paths(),
    }
}

fn handwritten_hello_there_paths() -> Vec<Vec<[f32; 2]>> {
    let mut paths = Vec::new();
    let mut word = Vec::new();
    append_hello_path(&mut word, 0.0);
    paths.push(word);

    let mut there = Vec::new();
    append_there_path(&mut there, 5.55);
    paths.push(there);

    paths.push(vec![[5.80, 0.42], [6.50, 0.35]]);
    paths
}

fn handwritten_hi_there_paths() -> Vec<Vec<[f32; 2]>> {
    let mut paths = Vec::new();
    let mut hi = Vec::new();
    append_hi_path(&mut hi, 0.0);
    paths.push(hi);

    let mut there = Vec::new();
    append_there_path(&mut there, 2.10);
    paths.push(there);

    paths.push(vec![[4.35, 0.42], [5.05, 0.35]]);
    paths
}

fn handwritten_hey_there_paths() -> Vec<Vec<[f32; 2]>> {
    let mut paths = Vec::new();
    let mut hey = Vec::new();
    append_hey_path(&mut hey, 0.0);
    paths.push(hey);

    let mut there = Vec::new();
    append_there_path(&mut there, 3.40);
    paths.push(there);

    paths.push(vec![[5.65, 0.42], [6.35, 0.35]]);
    paths
}

fn append_hello_path(path: &mut Vec<[f32; 2]>, x: f32) {
    path.push([x + 0.05, 1.05]);
    append_cubic(
        path,
        [x + 0.05, 1.05],
        [x + 0.12, 0.64],
        [x + 0.10, 0.15],
        [x + 0.34, -0.08],
        10,
    );
    append_cubic(
        path,
        [x + 0.34, -0.08],
        [x + 0.66, 0.14],
        [x + 0.20, 0.88],
        [x + 0.26, 1.06],
        14,
    );
    append_cubic(
        path,
        [x + 0.26, 1.06],
        [x + 0.38, 0.58],
        [x + 0.82, 0.52],
        [x + 1.02, 1.02],
        12,
    );
    append_cubic(
        path,
        [x + 1.02, 1.02],
        [x + 1.20, 0.58],
        [x + 1.72, 0.45],
        [x + 1.58, 0.86],
        12,
    );
    append_cubic(
        path,
        [x + 1.58, 0.86],
        [x + 1.42, 1.18],
        [x + 2.02, 1.18],
        [x + 2.22, 0.92],
        12,
    );
    append_cubic(
        path,
        [x + 2.22, 0.92],
        [x + 2.62, 0.45],
        [x + 2.78, -0.10],
        [x + 2.96, -0.06],
        12,
    );
    append_cubic(
        path,
        [x + 2.96, -0.06],
        [x + 3.22, 0.02],
        [x + 2.76, 0.78],
        [x + 3.07, 1.02],
        12,
    );
    append_cubic(
        path,
        [x + 3.07, 1.02],
        [x + 3.48, 0.56],
        [x + 3.60, -0.08],
        [x + 3.82, -0.04],
        12,
    );
    append_cubic(
        path,
        [x + 3.82, -0.04],
        [x + 4.04, 0.04],
        [x + 3.66, 0.72],
        [x + 3.90, 1.00],
        12,
    );
    append_cubic(
        path,
        [x + 3.90, 1.00],
        [x + 4.22, 0.38],
        [x + 5.00, 0.44],
        [x + 4.88, 0.86],
        16,
    );
    append_cubic(
        path,
        [x + 4.88, 0.86],
        [x + 4.74, 1.28],
        [x + 4.02, 1.15],
        [x + 4.15, 0.72],
        16,
    );
    append_cubic(
        path,
        [x + 4.15, 0.72],
        [x + 4.38, 0.28],
        [x + 4.96, 0.92],
        [x + 5.20, 0.82],
        12,
    );
}

fn append_hi_path(path: &mut Vec<[f32; 2]>, x: f32) {
    path.push([x + 0.08, 1.10]);
    append_cubic(
        path,
        [x + 0.08, 1.10],
        [x + 0.14, 0.70],
        [x + 0.12, 0.20],
        [x + 0.36, -0.06],
        10,
    );
    append_cubic(
        path,
        [x + 0.36, -0.06],
        [x + 0.68, -0.34],
        [x + 0.70, 0.74],
        [x + 0.40, 0.70],
        12,
    );
    append_cubic(
        path,
        [x + 0.40, 0.70],
        [x + 0.18, 0.66],
        [x + 0.28, 0.36],
        [x + 0.55, 0.42],
        8,
    );
    append_cubic(
        path,
        [x + 0.55, 0.42],
        [x + 0.82, 0.52],
        [x + 0.74, 0.78],
        [x + 0.92, 0.78],
        6,
    );
    append_cubic(
        path,
        [x + 0.92, 0.78],
        [x + 1.08, 0.76],
        [x + 0.93, 0.30],
        [x + 1.12, 0.28],
        8,
    );
    path.push([x + 0.99, 0.08]);
    path.push([x + 1.00, 0.09]);
}

fn append_hey_path(path: &mut Vec<[f32; 2]>, x: f32) {
    path.push([x + 0.08, 1.10]);
    append_cubic(
        path,
        [x + 0.08, 1.10],
        [x + 0.14, 0.68],
        [x + 0.12, 0.18],
        [x + 0.36, -0.06],
        10,
    );
    append_cubic(
        path,
        [x + 0.36, -0.06],
        [x + 0.70, -0.30],
        [x + 0.70, 0.74],
        [x + 0.42, 0.70],
        12,
    );
    append_cubic(
        path,
        [x + 0.42, 0.70],
        [x + 0.20, 0.66],
        [x + 0.28, 0.36],
        [x + 0.56, 0.42],
        8,
    );
    append_cubic(
        path,
        [x + 0.56, 0.42],
        [x + 0.88, 0.56],
        [x + 0.98, 0.62],
        [x + 1.18, 0.58],
        8,
    );
    append_cubic(
        path,
        [x + 1.18, 0.58],
        [x + 0.88, 0.80],
        [x + 0.86, 0.24],
        [x + 1.28, 0.30],
        12,
    );
    append_cubic(
        path,
        [x + 1.28, 0.30],
        [x + 1.54, 0.34],
        [x + 1.64, 0.52],
        [x + 1.78, 0.68],
        8,
    );
    append_cubic(
        path,
        [x + 1.78, 0.68],
        [x + 1.62, 0.38],
        [x + 1.70, 0.10],
        [x + 1.98, 0.10],
        8,
    );
    append_cubic(
        path,
        [x + 1.98, 0.10],
        [x + 2.30, 0.10],
        [x + 2.24, 0.68],
        [x + 2.08, 0.56],
        8,
    );
    append_cubic(
        path,
        [x + 2.08, 0.56],
        [x + 1.96, 0.44],
        [x + 2.08, 0.18],
        [x + 2.34, 0.20],
        8,
    );
    append_cubic(
        path,
        [x + 2.34, 0.20],
        [x + 2.58, 0.26],
        [x + 2.54, 0.58],
        [x + 2.70, 0.60],
        8,
    );
}

fn append_there_path(path: &mut Vec<[f32; 2]>, x: f32) {
    path.push([x + 0.38, 0.08]);
    append_cubic(
        path,
        [x + 0.38, 0.08],
        [x + 0.24, 0.52],
        [x + 0.22, 0.92],
        [x + 0.40, 1.06],
        12,
    );
    append_cubic(
        path,
        [x + 0.40, 1.06],
        [x + 0.66, 1.22],
        [x + 0.98, 0.92],
        [x + 1.05, 0.82],
        10,
    );
    append_cubic(
        path,
        [x + 1.05, 0.82],
        [x + 1.12, 0.44],
        [x + 1.14, 0.04],
        [x + 1.36, -0.05],
        12,
    );
    append_cubic(
        path,
        [x + 1.36, -0.05],
        [x + 1.72, 0.16],
        [x + 1.26, 0.78],
        [x + 1.38, 1.04],
        14,
    );
    append_cubic(
        path,
        [x + 1.38, 1.04],
        [x + 1.58, 0.62],
        [x + 2.00, 0.50],
        [x + 2.18, 1.02],
        12,
    );
    append_cubic(
        path,
        [x + 2.18, 1.02],
        [x + 2.38, 0.56],
        [x + 2.90, 0.45],
        [x + 2.76, 0.86],
        12,
    );
    append_cubic(
        path,
        [x + 2.76, 0.86],
        [x + 2.60, 1.18],
        [x + 3.20, 1.18],
        [x + 3.40, 0.92],
        12,
    );
    append_cubic(
        path,
        [x + 3.40, 0.92],
        [x + 3.54, 0.54],
        [x + 3.86, 0.54],
        [x + 4.00, 0.80],
        10,
    );
    append_cubic(
        path,
        [x + 4.00, 0.80],
        [x + 4.10, 0.52],
        [x + 4.24, 0.48],
        [x + 4.40, 0.62],
        8,
    );
    append_cubic(
        path,
        [x + 4.40, 0.62],
        [x + 4.22, 0.80],
        [x + 4.14, 1.14],
        [x + 4.50, 1.04],
        10,
    );
    append_cubic(
        path,
        [x + 4.50, 1.04],
        [x + 4.82, 0.56],
        [x + 5.34, 0.45],
        [x + 5.20, 0.86],
        12,
    );
    append_cubic(
        path,
        [x + 5.20, 0.86],
        [x + 5.04, 1.18],
        [x + 5.66, 1.16],
        [x + 5.92, 0.88],
        12,
    );
}

fn append_cubic(
    path: &mut Vec<[f32; 2]>,
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    steps: usize,
) {
    let steps = steps.saturating_mul(3).max(1);
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let mt = 1.0 - t;
        path.push([
            mt.powi(3) * p0[0]
                + 3.0 * mt.powi(2) * t * p1[0]
                + 3.0 * mt * t.powi(2) * p2[0]
                + t.powi(3) * p3[0],
            mt.powi(3) * p0[1]
                + 3.0 * mt.powi(2) * t * p1[1]
                + 3.0 * mt * t.powi(2) * p2[1]
                + t.powi(3) * p3[1],
        ]);
    }
}

fn stroke_paths_length(paths: &[Vec<[f32; 2]>]) -> f32 {
    paths
        .iter()
        .flat_map(|path| path.windows(2).map(|pair| distance(pair[0], pair[1])))
        .sum()
}

fn stroke_paths_bounds(paths: &[Vec<[f32; 2]>]) -> ([f32; 2], [f32; 2]) {
    let mut min = [f32::INFINITY, f32::INFINITY];
    let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
    for point in paths.iter().flatten() {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    if !min[0].is_finite() || !max[0].is_finite() {
        ([0.0, 0.0], [1.0, 1.0])
    } else {
        (min, max)
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

fn lerp_point(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn transform_handwriting_point(point: [f32; 2], origin: [f32; 2], scale: f32) -> [f32; 2] {
    [origin[0] + point[0] * scale, origin[1] + point[1] * scale]
}

fn push_stroke_segment(
    vertices: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    thickness: f32,
    color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    let soft_color = alpha_scaled(color, 0.16);
    push_stroke_segment_quad(vertices, a, b, thickness + 3.0, soft_color, size);
    push_stroke_dot(vertices, b, thickness * 0.78, soft_color, size);

    let feather_color = alpha_scaled(color, 0.28);
    push_stroke_segment_quad(vertices, a, b, thickness + 1.4, feather_color, size);
    push_stroke_dot(vertices, b, thickness * 0.64, feather_color, size);

    push_stroke_segment_quad(vertices, a, b, thickness, color, size);
    push_stroke_dot(vertices, b, thickness * 0.52, color, size);
}

fn push_stroke_segment_quad(
    vertices: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    thickness: f32,
    color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    let length = distance(a, b);
    if length <= 0.01 {
        return;
    }
    let nx = -(b[1] - a[1]) / length * thickness * 0.5;
    let ny = (b[0] - a[0]) / length * thickness * 0.5;
    push_pixel_triangle(
        vertices,
        [a[0] + nx, a[1] + ny],
        [a[0] - nx, a[1] - ny],
        [b[0] - nx, b[1] - ny],
        color,
        size,
    );
    push_pixel_triangle(
        vertices,
        [a[0] + nx, a[1] + ny],
        [b[0] - nx, b[1] - ny],
        [b[0] + nx, b[1] + ny],
        color,
        size,
    );
}

fn push_stroke_dot(
    vertices: &mut Vec<Vertex>,
    center: [f32; 2],
    radius: f32,
    color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    let segments = 28;
    for segment in 0..segments {
        let start = segment as f32 / segments as f32 * std::f32::consts::TAU;
        let end = (segment + 1) as f32 / segments as f32 * std::f32::consts::TAU;
        push_pixel_triangle(
            vertices,
            center,
            [
                center[0] + radius * start.cos(),
                center[1] + radius * start.sin(),
            ],
            [
                center[0] + radius * end.cos(),
                center[1] + radius * end.sin(),
            ],
            color,
            size,
        );
    }
}

fn push_aurora_ribbon(
    vertices: &mut Vec<Vertex>,
    size: PhysicalSize<u32>,
    center_y: f32,
    height: f32,
    phase: f32,
    left_color: [f32; 4],
    right_color: [f32; 4],
) {
    let width = size.width as f32;
    let segments = 18;
    for segment in 0..segments {
        let a = segment as f32 / segments as f32;
        let b = (segment + 1) as f32 / segments as f32;
        let x0 = -width * 0.08 + a * width * 1.16;
        let x1 = -width * 0.08 + b * width * 1.16;
        let wave0 = (a * std::f32::consts::TAU * 1.35 + phase).sin() * height * 0.23
            + (a * std::f32::consts::TAU * 2.10 + phase * 0.7).cos() * height * 0.10;
        let wave1 = (b * std::f32::consts::TAU * 1.35 + phase).sin() * height * 0.23
            + (b * std::f32::consts::TAU * 2.10 + phase * 0.7).cos() * height * 0.10;
        let color0 = mix_color(left_color, right_color, a);
        let color1 = mix_color(left_color, right_color, b);
        let edge0 = transparent(color0);
        let edge1 = transparent(color1);
        let top0 = [x0, center_y + wave0 - height * 0.55];
        let mid0 = [x0, center_y + wave0];
        let bot0 = [x0, center_y + wave0 + height * 0.55];
        let top1 = [x1, center_y + wave1 - height * 0.55];
        let mid1 = [x1, center_y + wave1];
        let bot1 = [x1, center_y + wave1 + height * 0.55];
        push_gradient_quad(
            vertices, top0, mid0, mid1, top1, edge0, color0, color1, edge1, size,
        );
        push_gradient_quad(
            vertices, mid0, bot0, bot1, mid1, color0, edge0, edge1, color1, size,
        );
    }
}

fn push_gradient_quad(
    vertices: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    d: [f32; 2],
    a_color: [f32; 4],
    b_color: [f32; 4],
    c_color: [f32; 4],
    d_color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    push_gradient_triangle(vertices, a, b, c, a_color, b_color, c_color, size);
    push_gradient_triangle(vertices, a, c, d, a_color, c_color, d_color, size);
}

fn mix_color(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

fn push_gradient_triangle(
    vertices: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    a_color: [f32; 4],
    b_color: [f32; 4],
    c_color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    vertices.extend_from_slice(&[
        Vertex {
            position: pixel_to_ndc(a, size),
            color: a_color,
        },
        Vertex {
            position: pixel_to_ndc(b, size),
            color: b_color,
        },
        Vertex {
            position: pixel_to_ndc(c, size),
            color: c_color,
        },
    ]);
}

fn transparent(mut color: [f32; 4]) -> [f32; 4] {
    color[3] = 0.0;
    color
}

fn alpha_scaled(mut color: [f32; 4], scale: f32) -> [f32; 4] {
    color[3] = (color[3] * scale).clamp(0.0, 1.0);
    color
}

pub(crate) fn push_native_activity_spinner(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) {
    let typography = single_session_typography();
    let draft_top = single_session_draft_top_for_app(app, size);
    let center_y = if welcome_status_lane_visible(app) {
        draft_top + typography.meta_size * 0.58
    } else {
        draft_top - SINGLE_SESSION_STATUS_GAP + 7.0
    };
    let center = [
        size.width as f32 - PANEL_TITLE_LEFT_PADDING - 12.0,
        center_y,
    ];
    let radius = (typography.meta_size * 0.54).clamp(5.0, 9.0);
    let thickness = 2.4;
    let segments = 12;
    let phase = (tick as usize) % segments;
    for segment in 0..segments {
        let age = (segment + segments - phase) % segments;
        let alpha_scale = if age == 0 {
            1.0
        } else {
            0.18 + (segments - age) as f32 / segments as f32 * 0.52
        };
        let mut color = if age == 0 {
            NATIVE_SPINNER_HEAD_COLOR
        } else {
            NATIVE_SPINNER_TRACK_COLOR
        };
        color[3] = (color[3] * alpha_scale).clamp(0.08, 1.0);
        let start =
            -std::f32::consts::FRAC_PI_2 + segment as f32 / segments as f32 * std::f32::consts::TAU;
        let end = start + std::f32::consts::TAU / segments as f32 * 0.64;
        push_spinner_segment(vertices, center, radius, thickness, start, end, color, size);
    }
}

fn push_spinner_segment(
    vertices: &mut Vec<Vertex>,
    center: [f32; 2],
    radius: f32,
    thickness: f32,
    start: f32,
    end: f32,
    color: [f32; 4],
    size: PhysicalSize<u32>,
) {
    let inner_radius = (radius - thickness).max(1.0);
    let outer_start = [
        center[0] + radius * start.cos(),
        center[1] + radius * start.sin(),
    ];
    let outer_end = [
        center[0] + radius * end.cos(),
        center[1] + radius * end.sin(),
    ];
    let inner_start = [
        center[0] + inner_radius * start.cos(),
        center[1] + inner_radius * start.sin(),
    ];
    let inner_end = [
        center[0] + inner_radius * end.cos(),
        center[1] + inner_radius * end.sin(),
    ];
    push_pixel_triangle(vertices, outer_start, outer_end, inner_end, color, size);
    push_pixel_triangle(vertices, outer_start, inner_end, inner_start, color, size);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SingleSessionTranscriptCardRun {
    pub(crate) line: usize,
    pub(crate) line_count: usize,
    pub(crate) style: SingleSessionLineStyle,
}

fn push_single_session_transcript_cards(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) {
    let viewport = single_session_body_viewport_for_tick(app, size, tick, smooth_scroll_lines);
    push_single_session_transcript_cards_from_viewport(
        vertices,
        app,
        size,
        &viewport,
        viewport.total_lines,
    );
}

fn push_single_session_transcript_cards_from_viewport(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    viewport: &SingleSessionBodyViewport,
    total_lines: usize,
) {
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let width = (size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0 + 12.0).max(1.0);
    let body_top = single_session_body_top_for_app(app, size);
    let body_bottom = single_session_body_bottom_for_total_lines(app, size, total_lines);

    for run in single_session_transcript_card_runs(&viewport.lines) {
        let Some(color) = single_session_line_card_color(run.style) else {
            continue;
        };
        let rect = Rect {
            x: PANEL_TITLE_LEFT_PADDING - 6.0,
            y: body_top + viewport.top_offset_pixels + run.line as f32 * line_height + 3.0,
            width,
            height: (run.line_count as f32 * line_height - 6.0).max(1.0),
        };
        let Some(rect) = clip_rect_to_vertical_bounds(rect, body_top, body_bottom) else {
            continue;
        };
        push_rounded_rect(vertices, rect, 7.0, color, size);
    }
}

pub(crate) fn push_single_session_streaming_shimmer(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) {
    let Some(shimmer) =
        single_session_streaming_shimmer_with_scroll(app, size, tick, smooth_scroll_lines)
    else {
        return;
    };

    push_rect(
        vertices,
        shimmer.soft_rect,
        STREAMING_SHIMMER_SOFT_COLOR,
        size,
    );
    push_rect(
        vertices,
        shimmer.core_rect,
        STREAMING_SHIMMER_CORE_COLOR,
        size,
    );
}

fn push_single_session_streaming_shimmer_from_viewport(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    viewport: &SingleSessionBodyViewport,
) {
    let Some(shimmer) = single_session_streaming_shimmer_from_viewport(app, size, tick, viewport)
    else {
        return;
    };

    push_rect(
        vertices,
        shimmer.soft_rect,
        STREAMING_SHIMMER_SOFT_COLOR,
        size,
    );
    push_rect(
        vertices,
        shimmer.core_rect,
        STREAMING_SHIMMER_CORE_COLOR,
        size,
    );
}

#[derive(Clone, Copy)]
pub(crate) struct SingleSessionStreamingShimmer {
    pub(crate) soft_rect: Rect,
    pub(crate) core_rect: Rect,
}

#[cfg(test)]
pub(crate) fn single_session_streaming_shimmer(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) -> Option<SingleSessionStreamingShimmer> {
    single_session_streaming_shimmer_with_scroll(app, size, tick, 0.0)
}

fn single_session_streaming_shimmer_with_scroll(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) -> Option<SingleSessionStreamingShimmer> {
    if app.streaming_response.trim().is_empty() {
        return None;
    }

    let viewport = single_session_body_viewport_for_tick(app, size, tick, smooth_scroll_lines);
    single_session_streaming_shimmer_from_viewport(app, size, tick, &viewport)
}

fn single_session_streaming_shimmer_from_viewport(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    viewport: &SingleSessionBodyViewport,
) -> Option<SingleSessionStreamingShimmer> {
    if app.streaming_response.trim().is_empty() {
        return None;
    }

    let line_index = viewport.lines.iter().rposition(is_shimmer_anchor_line)?;

    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let body_top = single_session_body_top_for_app(app, size);
    let text_columns = viewport.lines[line_index].text.chars().count().max(8) as f32;
    let text_width = (text_columns * single_session_body_char_width_for_scale(app.text_scale()))
        .min((size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0).max(1.0));
    let lane_width = (text_width + 56.0).max(120.0);
    let shimmer_width = lane_width.min(180.0).max(72.0);
    let phase = (tick % 48) as f32 / 48.0;
    let travel = lane_width + shimmer_width;
    let head_x = PANEL_TITLE_LEFT_PADDING - shimmer_width + phase * travel;
    let y = body_top
        + viewport.top_offset_pixels
        + line_index as f32 * line_height
        + line_height * 0.12;
    let height = line_height * 0.76;

    let soft_rect = Rect {
        x: head_x,
        y,
        width: shimmer_width,
        height,
    };
    let core_rect = Rect {
        x: head_x + shimmer_width * 0.34,
        y,
        width: shimmer_width * 0.32,
        height,
    };
    Some(SingleSessionStreamingShimmer {
        soft_rect,
        core_rect,
    })
}

fn push_single_session_scrollbar(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) {
    let Some(metrics) = single_session_body_scroll_metrics(app, size, tick) else {
        return;
    };
    push_single_session_scrollbar_for_metrics(vertices, size, smooth_scroll_lines, metrics);
}

fn push_single_session_scrollbar_for_total_lines(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    total_lines: usize,
) {
    let Some(metrics) = single_session_body_scroll_metrics_for_total_lines(app, size, total_lines)
    else {
        return;
    };
    push_single_session_scrollbar_for_metrics(vertices, size, smooth_scroll_lines, metrics);
}

fn push_single_session_scrollbar_for_metrics(
    vertices: &mut Vec<Vertex>,
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    metrics: SingleSessionBodyScrollMetrics,
) {
    let track_top = PANEL_BODY_TOP_PADDING + 4.0;
    let track_bottom = single_session_body_bottom(size) - 4.0;
    let track_height = (track_bottom - track_top).max(1.0);
    let x = size.width as f32 - PANEL_TITLE_LEFT_PADDING - 4.0;
    let thumb_height = (metrics.visible_lines as f32 / metrics.total_lines as f32 * track_height)
        .clamp(28.0, track_height);
    let travel = (track_height - thumb_height).max(0.0);
    let smooth_scroll_lines =
        (metrics.scroll_lines + smooth_scroll_lines).clamp(0.0, metrics.max_scroll_lines as f32);
    let scroll_fraction = smooth_scroll_lines / metrics.max_scroll_lines.max(1) as f32;
    let thumb_y = track_top + (1.0 - scroll_fraction.clamp(0.0, 1.0)) * travel;

    push_rounded_rect(
        vertices,
        Rect {
            x,
            y: track_top,
            width: 3.0,
            height: track_height,
        },
        2.0,
        [0.040, 0.055, 0.090, 0.075],
        size,
    );
    push_rounded_rect(
        vertices,
        Rect {
            x: x - 0.5,
            y: thumb_y,
            width: 4.0,
            height: thumb_height,
        },
        2.0,
        [0.035, 0.065, 0.145, 0.34],
        size,
    );
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SingleSessionBodyScrollMetrics {
    pub(crate) total_lines: usize,
    pub(crate) visible_lines: usize,
    pub(crate) scroll_lines: f32,
    pub(crate) max_scroll_lines: usize,
}

pub(crate) fn single_session_body_scroll_metrics(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) -> Option<SingleSessionBodyScrollMetrics> {
    let _ = tick;
    let total_lines = welcome_timeline_total_body_lines(app, size);
    single_session_body_scroll_metrics_for_total_lines(app, size, total_lines)
}

pub(crate) fn single_session_body_scroll_metrics_for_total_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    total_lines: usize,
) -> Option<SingleSessionBodyScrollMetrics> {
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let body_top = single_session_body_top_for_app(app, size);
    let body_bottom = single_session_body_bottom_for_total_lines(app, size, total_lines);
    let available_height = (body_bottom - body_top).max(line_height);
    let visible_lines = ((available_height / line_height).floor() as usize).max(1);
    let max_scroll_lines = total_lines.saturating_sub(visible_lines);
    (max_scroll_lines > 0).then_some(SingleSessionBodyScrollMetrics {
        total_lines,
        visible_lines,
        scroll_lines: app.body_scroll_lines.min(max_scroll_lines as f32),
        max_scroll_lines,
    })
}

fn is_shimmer_anchor_line(line: &SingleSessionStyledLine) -> bool {
    !line.text.trim().is_empty() && is_assistant_rendered_style(line.style)
}

fn is_assistant_rendered_style(style: SingleSessionLineStyle) -> bool {
    matches!(
        style,
        SingleSessionLineStyle::Assistant
            | SingleSessionLineStyle::AssistantHeading
            | SingleSessionLineStyle::AssistantQuote
            | SingleSessionLineStyle::AssistantTable
            | SingleSessionLineStyle::AssistantLink
            | SingleSessionLineStyle::Code
    )
}

pub(crate) fn single_session_transcript_card_runs(
    lines: &[SingleSessionStyledLine],
) -> Vec<SingleSessionTranscriptCardRun> {
    let mut runs = Vec::new();
    let mut current: Option<SingleSessionTranscriptCardRun> = None;

    for (line, styled_line) in lines.iter().enumerate() {
        if single_session_line_card_color(styled_line.style).is_none() {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            continue;
        }

        match &mut current {
            Some(run) if run.style == styled_line.style && run.line + run.line_count == line => {
                run.line_count += 1;
            }
            Some(run) => {
                runs.push(*run);
                current = Some(SingleSessionTranscriptCardRun {
                    line,
                    line_count: 1,
                    style: styled_line.style,
                });
            }
            None => {
                current = Some(SingleSessionTranscriptCardRun {
                    line,
                    line_count: 1,
                    style: styled_line.style,
                });
            }
        }
    }

    if let Some(run) = current {
        runs.push(run);
    }
    runs
}

fn single_session_line_card_color(style: SingleSessionLineStyle) -> Option<[f32; 4]> {
    match style {
        SingleSessionLineStyle::Code => Some(CODE_BLOCK_BACKGROUND_COLOR),
        SingleSessionLineStyle::AssistantQuote => Some(QUOTE_CARD_BACKGROUND_COLOR),
        SingleSessionLineStyle::AssistantTable => Some(TABLE_CARD_BACKGROUND_COLOR),
        SingleSessionLineStyle::Tool => Some(TOOL_CARD_BACKGROUND_COLOR),
        SingleSessionLineStyle::Error => Some(ERROR_CARD_BACKGROUND_COLOR),
        SingleSessionLineStyle::OverlaySelection => Some(OVERLAY_SELECTION_BACKGROUND_COLOR),
        _ => None,
    }
}

fn push_single_session_selection(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) {
    if !app.has_body_selection() && !app.has_draft_selection() {
        return;
    }

    let typography = single_session_typography();
    let line_height = typography.body_size * typography.body_line_height;
    let char_width = single_session_body_char_width();
    let visible_lines = single_session_visible_body(app, size);
    let body_top = single_session_body_top_for_app(app, size);
    for segment in app.selection_segments(&visible_lines) {
        let selected_columns = segment
            .end_column
            .saturating_sub(segment.start_column)
            .max(1);
        push_rect(
            vertices,
            Rect {
                x: PANEL_TITLE_LEFT_PADDING - 2.0 + segment.start_column as f32 * char_width,
                y: body_top + segment.line as f32 * line_height,
                width: selected_columns as f32 * char_width + 4.0,
                height: line_height,
            },
            SELECTION_HIGHLIGHT_COLOR,
            size,
        );
    }

    if welcome_status_lane_visible(app) {
        return;
    }
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.code_size * typography.code_line_height;
    let char_width = typography.code_size * 0.58;
    let draft_top = single_session_draft_top_for_app(app, size);
    for segment in app.draft_selection_segments() {
        let selected_columns = segment
            .end_column
            .saturating_sub(segment.start_column)
            .max(1);
        push_rect(
            vertices,
            Rect {
                x: PANEL_TITLE_LEFT_PADDING - 2.0 + segment.start_column as f32 * char_width,
                y: draft_top + segment.line as f32 * line_height,
                width: selected_columns as f32 * char_width + 4.0,
                height: line_height,
            },
            SELECTION_HIGHLIGHT_COLOR,
            size,
        );
    }
}

pub(crate) fn push_single_session_caret(
    vertices: &mut Vec<Vertex>,
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    draft_buffer: Option<&Buffer>,
) {
    if welcome_status_lane_visible(app) {
        return;
    }

    let caret = draft_buffer
        .and_then(|buffer| glyphon_draft_caret_position(app, buffer, size))
        .unwrap_or_else(|| approximate_draft_caret_position(app, size));

    push_rect(
        vertices,
        Rect {
            x: caret.x,
            y: caret.y,
            width: SINGLE_SESSION_CARET_WIDTH,
            height: caret.height,
        },
        SINGLE_SESSION_CARET_COLOR,
        size,
    );
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CaretPosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
    height: f32,
}

pub(crate) fn glyphon_draft_caret_position(
    app: &SingleSessionApp,
    draft_buffer: &Buffer,
    size: PhysicalSize<u32>,
) -> Option<CaretPosition> {
    let typography = single_session_typography();
    let target = app.composer_cursor_line_byte_index();
    let target_line = target.0;
    let target_index = target.1;
    let mut fallback = None;

    for run in draft_buffer.layout_runs() {
        if run.line_i != target_line {
            continue;
        }
        let y = single_session_draft_top_for_app(app, size) + run.line_top;
        let height = typography.code_size * 1.12;
        if run.glyphs.is_empty() {
            return Some(CaretPosition {
                x: PANEL_TITLE_LEFT_PADDING,
                y,
                height,
            });
        }

        let first = run.glyphs.first()?;
        let last = run.glyphs.last()?;
        let mut run_position = CaretPosition {
            x: PANEL_TITLE_LEFT_PADDING + last.x + last.w,
            y,
            height,
        };
        if target_index <= first.start {
            run_position.x = PANEL_TITLE_LEFT_PADDING + first.x;
            return Some(run_position);
        }
        for glyph in run.glyphs {
            if target_index <= glyph.start {
                run_position.x = PANEL_TITLE_LEFT_PADDING + glyph.x;
                return Some(run_position);
            }
            if target_index <= glyph.end {
                run_position.x = PANEL_TITLE_LEFT_PADDING + glyph.x + glyph.w;
                return Some(run_position);
            }
        }
        if target_index >= first.start && target_index >= last.end {
            fallback = Some(run_position);
        }
    }

    fallback
}

fn approximate_draft_caret_position(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) -> CaretPosition {
    let typography = single_session_typography();
    let line_height = typography.code_size * typography.code_line_height;
    let draft_top = single_session_draft_top_for_app(app, size);
    let (cursor_line, cursor_column) = app.draft_cursor_line_col();
    let char_width = typography.code_size * 0.58;
    let prompt_column = if cursor_line == 0 {
        app.composer_prompt().chars().count()
    } else {
        0
    };
    let x = PANEL_TITLE_LEFT_PADDING
        + ((prompt_column + cursor_column) as f32 * char_width)
            .min((size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0).max(0.0));
    let y = draft_top + cursor_line as f32 * line_height;
    CaretPosition {
        x,
        y,
        height: typography.code_size * 1.12,
    }
}

pub(crate) fn single_session_draft_line_col_at_position(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    x: f32,
    y: f32,
) -> Option<(usize, usize)> {
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.code_size * typography.code_line_height;
    let draft_top = single_session_draft_top_for_app(app, size);
    let draft_bottom = size.height as f32 - PANEL_TITLE_TOP_PADDING;
    if y < draft_top || y > draft_bottom || x < PANEL_TITLE_LEFT_PADDING {
        return None;
    }

    let line = ((y - draft_top) / line_height).floor().max(0.0) as usize;
    let draft_lines: Vec<&str> = app.draft.split('\n').collect();
    let line = line.min(draft_lines.len().saturating_sub(1));
    let char_width = typography.code_size * 0.58;
    let raw_column = ((x - PANEL_TITLE_LEFT_PADDING) / char_width)
        .round()
        .max(0.0) as usize;
    let prompt_columns = if line == 0 {
        app.composer_prompt().chars().count()
    } else {
        0
    };
    let draft_column = raw_column.saturating_sub(prompt_columns);
    let max_column = draft_lines
        .get(line)
        .map(|text| text.chars().count())
        .unwrap_or_default();
    Some((line, draft_column.min(max_column)))
}

pub(crate) fn single_session_draft_top(size: PhysicalSize<u32>) -> f32 {
    (size.height as f32 - SINGLE_SESSION_DRAFT_TOP_OFFSET).max(112.0)
}

pub(crate) fn single_session_draft_top_for_app(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) -> f32 {
    if app.is_welcome_timeline_visible() {
        if app.has_welcome_timeline_transcript() {
            return welcome_timeline_draft_top(app, size);
        }
        return fresh_welcome_draft_top_for_scale(size, app.text_scale());
    }

    single_session_draft_top(size)
}

fn welcome_timeline_draft_top(app: &SingleSessionApp, size: PhysicalSize<u32>) -> f32 {
    welcome_timeline_draft_top_for_total_lines(
        app,
        size,
        welcome_timeline_total_body_lines(app, size),
    )
}

fn welcome_timeline_draft_top_for_total_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    total_lines: usize,
) -> f32 {
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let body_top = PANEL_BODY_TOP_PADDING;
    let timeline_lines = total_lines.max(1) as f32;
    let desired = body_top + timeline_lines * line_height + welcome_timeline_body_draft_gap();
    desired
        .min(single_session_draft_top(size))
        .max(fresh_welcome_draft_top(size))
}

fn welcome_timeline_body_draft_gap() -> f32 {
    let typography = single_session_typography();
    let body_line_height = typography.body_size * typography.body_line_height;
    let composer_line_height = typography.code_size * typography.code_line_height;
    body_line_height.max(composer_line_height * 0.86)
}

fn welcome_timeline_total_body_lines(app: &SingleSessionApp, size: PhysicalSize<u32>) -> usize {
    let transcript_lines =
        single_session_wrapped_body_lines(app.body_styled_lines(), size, app.text_scale()).len();
    if app.is_welcome_timeline_visible() && app.has_welcome_timeline_transcript() {
        welcome_timeline_virtual_body_lines(app, size) + transcript_lines
    } else {
        transcript_lines
    }
}

fn welcome_timeline_virtual_body_lines(app: &SingleSessionApp, size: PhysicalSize<u32>) -> usize {
    // Reserve scrollable visual space for the handwritten hero without adding
    // the hero phrase to transcript text or model-derived body lines.
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    ((fresh_welcome_visual_bottom(size) - PANEL_BODY_TOP_PADDING).max(0.0) / line_height)
        .ceil()
        .max(0.0) as usize
}

pub(crate) fn single_session_draft_top_for_fresh_state(
    size: PhysicalSize<u32>,
    fresh_welcome_visible: bool,
) -> f32 {
    if fresh_welcome_visible {
        fresh_welcome_draft_top(size)
    } else {
        single_session_draft_top(size)
    }
}

pub(crate) fn fresh_welcome_draft_top(size: PhysicalSize<u32>) -> f32 {
    fresh_welcome_draft_top_for_scale(size, 1.0)
}

fn fresh_welcome_draft_top_for_scale(size: PhysicalSize<u32>, ui_scale: f32) -> f32 {
    let hero_bottom = handwritten_welcome_bounds_for_phrase_with_scale(
        size,
        handwritten_welcome_phrase(0),
        ui_scale,
    )
    .1[1];
    let typography = single_session_typography_for_scale(ui_scale);
    let version_clearance = fresh_welcome_version_gap_for_scale(ui_scale)
        + fresh_welcome_version_font_size() * ui_scale * 1.4
        + (typography.body_size * 0.38).max(8.0);
    let clearance = (typography.code_size * 1.85)
        .max(version_clearance)
        .max(54.0);
    hero_bottom + clearance
}

fn fresh_welcome_visual_bottom(size: PhysicalSize<u32>) -> f32 {
    fresh_welcome_version_top(size) + fresh_welcome_version_font_size() * 1.4
}

#[cfg(test)]
pub(crate) fn single_session_text_buffers(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    font_system: &mut FontSystem,
) -> Vec<Buffer> {
    let key = single_session_text_key(app, size);
    single_session_text_buffers_from_key(&key, size, font_system)
}

#[cfg(test)]
pub(crate) fn single_session_text_key(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) -> SingleSessionTextKey {
    single_session_text_key_for_tick(app, size, 0)
}

#[cfg(test)]
pub(crate) fn single_session_text_key_for_tick(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) -> SingleSessionTextKey {
    single_session_text_key_for_tick_with_scroll(app, size, tick, 0.0)
}

pub(crate) fn single_session_text_key_for_tick_with_scroll(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) -> SingleSessionTextKey {
    let rendered_body_lines = single_session_rendered_body_lines_for_tick(app, size, tick);
    single_session_text_key_for_tick_with_rendered_body(
        app,
        size,
        tick,
        smooth_scroll_lines,
        &rendered_body_lines,
    )
}

pub(crate) fn single_session_text_key_for_tick_with_rendered_body(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
    rendered_body_lines: &[SingleSessionStyledLine],
) -> SingleSessionTextKey {
    let body = single_session_body_viewport_from_lines(
        app,
        size,
        smooth_scroll_lines,
        rendered_body_lines,
    )
    .lines;
    single_session_text_key_for_body_lines(app, size, tick, body)
}

fn single_session_text_key_for_body_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    body: Vec<SingleSessionStyledLine>,
) -> SingleSessionTextKey {
    let welcome_handoff_visible = false;
    let welcome_chrome_visible = app.is_welcome_timeline_visible();
    let welcome_input_visible = true;
    let (welcome_hero, welcome_hint) = if welcome_chrome_visible {
        (app.welcome_hero_text(), Vec::new())
    } else {
        (String::new(), Vec::new())
    };
    SingleSessionTextKey {
        size: (size.width, size.height),
        fresh_welcome_visible: welcome_chrome_visible,
        title: if welcome_chrome_visible {
            String::new()
        } else {
            app.header_title()
        },
        version: if welcome_chrome_visible {
            if welcome_input_visible {
                fresh_welcome_version_label()
            } else {
                String::new()
            }
        } else {
            desktop_header_version_label()
        },
        welcome_hero,
        welcome_hint,
        activity_active: app.has_activity_indicator(),
        welcome_handoff_visible,
        text_scale_bits: app.text_scale().to_bits(),
        body,
        draft: if welcome_input_visible {
            visualize_composer_whitespace(&app.composer_text())
        } else {
            String::new()
        },
        status: if welcome_chrome_visible && !app.has_welcome_timeline_transcript() {
            String::new()
        } else {
            app.composer_status_line_for_tick(tick)
        },
    }
}

pub(crate) fn single_session_text_buffers_from_key(
    key: &SingleSessionTextKey,
    size: PhysicalSize<u32>,
    font_system: &mut FontSystem,
) -> Vec<Buffer> {
    let text_scale = f32::from_bits(key.text_scale_bits);
    let typography = single_session_typography_for_scale(text_scale);
    let content_width = (size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0).max(1.0);

    let draft_top = if key.fresh_welcome_visible {
        fresh_welcome_draft_top_for_scale(size, text_scale)
    } else {
        single_session_draft_top_for_fresh_state(size, false)
    };
    let prompt_height = (size.height as f32 - draft_top - SINGLE_SESSION_STATUS_GAP - 18.0)
        .max(typography.code_size * typography.code_line_height * 2.0);
    let hero_font_size = welcome_hero_font_size(&key.welcome_hero, size) * text_scale;
    let version_font_size = if key.fresh_welcome_visible {
        fresh_welcome_version_font_size()
    } else {
        typography.meta_size
    };

    vec![
        single_session_text_buffer(
            font_system,
            &key.title,
            typography.title_size,
            typography.title_size * typography.meta_line_height,
            content_width,
            48.0,
        ),
        single_session_styled_text_buffer(
            font_system,
            &key.body,
            typography.body_size,
            typography.body_size * typography.body_line_height,
            content_width,
            (size.height as f32 - 150.0).max(1.0),
        ),
        single_session_text_buffer(
            font_system,
            &key.draft,
            typography.code_size,
            typography.code_size * typography.code_line_height,
            content_width,
            prompt_height,
        ),
        single_session_text_buffer(
            font_system,
            &key.status,
            typography.meta_size,
            typography.meta_size * typography.meta_line_height,
            content_width,
            28.0,
        ),
        single_session_text_buffer(
            font_system,
            &key.version,
            version_font_size,
            version_font_size * typography.meta_line_height,
            content_width,
            24.0,
        ),
        single_session_nowrap_text_buffer(
            font_system,
            key.welcome_hero.trim(),
            hero_font_size,
            hero_font_size * 1.08,
            size.width as f32 * 0.64,
            hero_font_size * 1.25,
        ),
    ]
}

pub(crate) fn single_session_body_text_buffer_from_lines(
    font_system: &mut FontSystem,
    lines: &[SingleSessionStyledLine],
    size: PhysicalSize<u32>,
    text_scale: f32,
) -> Buffer {
    let typography = single_session_typography_for_scale(text_scale);
    let content_width = (size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0).max(1.0);
    let mut buffer = single_session_styled_text_buffer(
        font_system,
        lines,
        typography.body_size,
        typography.body_size * typography.body_line_height,
        content_width,
        (size.height as f32 - 150.0).max(1.0),
    );
    buffer.shape_until(font_system, i32::MAX);
    buffer
}

fn welcome_hero_font_size(hero: &str, size: PhysicalSize<u32>) -> f32 {
    let width = size.width as f32;
    let height = size.height as f32;
    let chars = hero.trim().chars().count().max(1) as f32;
    let target_width = width * 0.50;
    (target_width / (chars * 0.56)).clamp(42.0, height * 0.18)
}

pub(crate) fn single_session_visible_body(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) -> Vec<String> {
    single_session_visible_styled_body(app, size)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

pub(crate) fn single_session_visible_styled_body(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
) -> Vec<SingleSessionStyledLine> {
    single_session_visible_styled_body_for_tick(app, size, 0)
}

pub(crate) fn single_session_visible_styled_body_for_tick(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) -> Vec<SingleSessionStyledLine> {
    single_session_body_viewport_for_tick(app, size, tick, 0.0).lines
}

#[derive(Clone, Debug)]
pub(crate) struct SingleSessionBodyViewport {
    pub(crate) lines: Vec<SingleSessionStyledLine>,
    pub(crate) top_offset_pixels: f32,
    pub(crate) start_line: usize,
    pub(crate) total_lines: usize,
}

pub(crate) fn single_session_body_viewport_for_tick(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) -> SingleSessionBodyViewport {
    let lines = single_session_rendered_body_lines_for_tick(app, size, tick);
    single_session_body_viewport_from_lines(app, size, smooth_scroll_lines, &lines)
}

pub(crate) fn single_session_body_viewport_from_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    lines: &[SingleSessionStyledLine],
) -> SingleSessionBodyViewport {
    let typography = single_session_typography_for_scale(app.text_scale());
    let line_height = typography.body_size * typography.body_line_height;
    let body_top = single_session_body_top_for_app(app, size);
    let total_lines = lines.len();
    let body_bottom = single_session_body_bottom_for_total_lines(app, size, total_lines);
    let available_height = (body_bottom - body_top).max(line_height);
    let visible_lines = ((available_height / line_height).floor() as usize).max(1);
    if lines.len() <= visible_lines {
        return SingleSessionBodyViewport {
            lines: lines.to_vec(),
            top_offset_pixels: 0.0,
            start_line: 0,
            total_lines,
        };
    }

    let max_scroll = lines.len().saturating_sub(visible_lines);
    let scroll = (app.body_scroll_lines + smooth_scroll_lines).clamp(0.0, max_scroll as f32);
    let bottom_line = lines.len() as f32 - scroll;
    let top_line = bottom_line - visible_lines as f32;
    let start = top_line.floor().max(0.0) as usize;
    let end = bottom_line.ceil().min(lines.len() as f32) as usize;
    let top_offset_pixels = (start as f32 - top_line) * line_height;
    SingleSessionBodyViewport {
        lines: lines[start..end.max(start)].to_vec(),
        top_offset_pixels,
        start_line: start,
        total_lines,
    }
}

pub(crate) fn single_session_rendered_body_lines_for_tick(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
) -> Vec<SingleSessionStyledLine> {
    let lines = single_session_wrapped_body_lines(
        app.body_styled_lines_for_tick(tick),
        size,
        app.text_scale(),
    );
    if !(app.is_welcome_timeline_visible() && app.has_welcome_timeline_transcript()) {
        return lines;
    }

    // The welcome hero is visual chrome. These blank prelude rows make it
    // scroll like the first timeline block while keeping transcript text pure.
    let virtual_lines = welcome_timeline_virtual_body_lines(app, size);
    let mut rendered = Vec::with_capacity(virtual_lines + lines.len());
    rendered.extend((0..virtual_lines).map(|_| blank_render_line()));
    rendered.extend(lines);
    rendered
}

fn blank_render_line() -> SingleSessionStyledLine {
    SingleSessionStyledLine {
        text: String::new(),
        style: SingleSessionLineStyle::Blank,
    }
}

fn single_session_wrapped_body_lines(
    lines: Vec<SingleSessionStyledLine>,
    size: PhysicalSize<u32>,
    text_scale: f32,
) -> Vec<SingleSessionStyledLine> {
    // Glyphon also wraps, but explicit visual rows keep scroll metrics,
    // selection hit-testing, and the rendered text viewport in agreement.
    let content_width = (size.width as f32 - PANEL_TITLE_LEFT_PADDING * 2.0).max(1.0);
    let max_columns = (content_width / single_session_body_char_width_for_scale(text_scale))
        .floor()
        .max(20.0) as usize;
    let mut wrapped = Vec::with_capacity(lines.len());

    for line in lines {
        if line.text.chars().count() <= max_columns || line.text.is_empty() {
            wrapped.push(line);
            continue;
        }
        for text in wrap_body_line_text(&line.text, max_columns) {
            wrapped.push(SingleSessionStyledLine {
                text,
                style: line.style,
            });
        }
    }

    wrapped
}

fn wrap_body_line_text(text: &str, max_columns: usize) -> Vec<String> {
    let max_columns = max_columns.max(1);
    let mut remaining = text.trim_end();
    let mut lines = Vec::new();

    while remaining.chars().count() > max_columns {
        let split = word_wrap_split_index(remaining, max_columns);
        let (line, rest) = remaining.split_at(split);
        lines.push(line.trim_end().to_string());
        remaining = rest.trim_start();
    }

    lines.push(remaining.to_string());
    lines
}

fn word_wrap_split_index(text: &str, max_columns: usize) -> usize {
    let hard_split = byte_index_at_char_limit(text, max_columns);
    text[..hard_split]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .filter(|index| *index > 0)
        .unwrap_or(hard_split)
}

fn byte_index_at_char_limit(text: &str, max_columns: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .nth(max_columns)
        .unwrap_or(text.len())
}

pub(crate) fn single_session_body_line_at_y(size: PhysicalSize<u32>, y: f32) -> Option<usize> {
    let typography = single_session_typography();
    let line_height = typography.body_size * typography.body_line_height;
    if y < PANEL_BODY_TOP_PADDING || y >= single_session_body_bottom(size) {
        return None;
    }
    Some(((y - PANEL_BODY_TOP_PADDING) / line_height).floor() as usize)
}

pub(crate) fn single_session_body_point_at_position(
    size: PhysicalSize<u32>,
    x: f32,
    y: f32,
    lines: &[String],
) -> Option<SelectionPoint> {
    let line = single_session_body_line_at_y(size, y)?;
    let text = lines.get(line)?;
    Some(SelectionPoint {
        line,
        column: single_session_body_column_at_x(x, text),
    })
}

pub(crate) fn single_session_body_column_at_x(x: f32, line: &str) -> usize {
    let char_count = line.chars().count();
    if x <= PANEL_TITLE_LEFT_PADDING {
        return 0;
    }
    let raw = ((x - PANEL_TITLE_LEFT_PADDING) / single_session_body_char_width()).round();
    raw.max(0.0).min(char_count as f32) as usize
}

pub(crate) fn single_session_body_char_width() -> f32 {
    single_session_body_char_width_for_scale(1.0)
}

fn single_session_body_char_width_for_scale(text_scale: f32) -> f32 {
    let typography = single_session_typography_for_scale(text_scale);
    typography.body_size * 0.58
}

fn single_session_body_top_for_app(_app: &SingleSessionApp, _size: PhysicalSize<u32>) -> f32 {
    PANEL_BODY_TOP_PADDING
}

fn single_session_body_bottom_for_app(app: &SingleSessionApp, size: PhysicalSize<u32>) -> f32 {
    if app.is_welcome_timeline_visible() && app.has_welcome_timeline_transcript() {
        return (single_session_draft_top_for_app(app, size) - welcome_timeline_body_draft_gap())
            .max(single_session_body_top_for_app(app, size));
    }

    single_session_body_bottom(size)
}

fn single_session_body_bottom_for_total_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    total_lines: usize,
) -> f32 {
    if app.is_welcome_timeline_visible() && app.has_welcome_timeline_transcript() {
        return (welcome_timeline_draft_top_for_total_lines(app, size, total_lines)
            - welcome_timeline_body_draft_gap())
        .max(single_session_body_top_for_app(app, size));
    }

    single_session_body_bottom(size)
}

pub(crate) fn single_session_body_bottom(size: PhysicalSize<u32>) -> f32 {
    single_session_draft_top(size) - SINGLE_SESSION_STATUS_GAP - 12.0
}

fn clip_rect_to_vertical_bounds(rect: Rect, top: f32, bottom: f32) -> Option<Rect> {
    let clipped_y = rect.y.max(top);
    let clipped_bottom = (rect.y + rect.height).min(bottom);
    (clipped_bottom > clipped_y).then_some(Rect {
        y: clipped_y,
        height: clipped_bottom - clipped_y,
        ..rect
    })
}

fn single_session_text_buffer(
    font_system: &mut FontSystem,
    text: &str,
    font_size: f32,
    line_height: f32,
    width: f32,
    height: f32,
) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, width, height);
    buffer.set_wrap(font_system, Wrap::Word);
    buffer.set_text(
        font_system,
        text,
        Attrs::new().family(Family::Name(SINGLE_SESSION_FONT_FAMILY)),
        desktop_text_shaping(text),
    );
    buffer.shape_until_scroll(font_system);
    buffer
}

fn single_session_nowrap_text_buffer(
    font_system: &mut FontSystem,
    text: &str,
    font_size: f32,
    line_height: f32,
    width: f32,
    height: f32,
) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, width, height);
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_text(
        font_system,
        text,
        Attrs::new().family(Family::Name(SINGLE_SESSION_FONT_FAMILY)),
        desktop_text_shaping(text),
    );
    buffer.shape_until_scroll(font_system);
    buffer
}

fn single_session_styled_text_buffer(
    font_system: &mut FontSystem,
    lines: &[SingleSessionStyledLine],
    font_size: f32,
    line_height: f32,
    width: f32,
    height: f32,
) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, width, height);
    let segments = single_session_styled_text_segments(lines);
    let shaping = if segments.iter().all(|(text, _)| text.is_ascii()) {
        Shaping::Basic
    } else {
        Shaping::Advanced
    };
    buffer.set_rich_text(
        font_system,
        segments.iter().map(|(text, attrs)| (text.as_str(), *attrs)),
        shaping,
    );
    buffer.shape_until_scroll(font_system);
    buffer
}

fn desktop_text_shaping(text: &str) -> Shaping {
    if text.is_ascii() {
        Shaping::Basic
    } else {
        Shaping::Advanced
    }
}

pub(crate) fn single_session_styled_text_segments(
    lines: &[SingleSessionStyledLine],
) -> Vec<(String, Attrs<'static>)> {
    let mut segments = Vec::new();
    let total_user_turns = lines
        .iter()
        .filter(|line| line.style == SingleSessionLineStyle::User)
        .count();
    for (index, line) in lines.iter().enumerate() {
        if !line.text.is_empty() {
            if line.style == SingleSessionLineStyle::User {
                push_user_prompt_segments(&mut segments, &line.text, total_user_turns);
            } else {
                segments.push((line.text.clone(), single_session_style_attrs(line.style)));
            }
        }
        if index + 1 < lines.len() {
            segments.push((
                "\n".to_string(),
                single_session_style_attrs(SingleSessionLineStyle::Blank),
            ));
        }
    }
    if segments.is_empty() {
        segments.push((
            String::new(),
            single_session_style_attrs(SingleSessionLineStyle::Blank),
        ));
    }
    segments
}

fn push_user_prompt_segments(
    segments: &mut Vec<(String, Attrs<'static>)>,
    line: &str,
    total_user_turns: usize,
) {
    let Some((number, text)) = line.split_once("  ") else {
        segments.push((
            line.to_string(),
            single_session_style_attrs(SingleSessionLineStyle::User),
        ));
        return;
    };
    let Ok(turn) = number.parse::<usize>() else {
        segments.push((
            line.to_string(),
            single_session_style_attrs(SingleSessionLineStyle::User),
        ));
        return;
    };

    segments.push((
        number.to_string(),
        single_session_color_attrs(user_prompt_number_color_for_distance(
            total_user_turns.saturating_add(1).saturating_sub(turn),
        )),
    ));
    segments.push((
        "› ".to_string(),
        single_session_color_attrs(text_color(USER_PROMPT_ACCENT_COLOR)),
    ));
    segments.push((
        text.to_string(),
        single_session_style_attrs(SingleSessionLineStyle::User),
    ));
}

fn single_session_style_attrs(style: SingleSessionLineStyle) -> Attrs<'static> {
    let family = if is_ai_response_font_style(style) {
        SINGLE_SESSION_ASSISTANT_FONT_FAMILY
    } else {
        SINGLE_SESSION_FONT_FAMILY
    };
    Attrs::new()
        .family(Family::Name(family))
        .color(single_session_line_color(style))
}

fn is_ai_response_font_style(style: SingleSessionLineStyle) -> bool {
    matches!(
        style,
        SingleSessionLineStyle::Assistant
            | SingleSessionLineStyle::AssistantHeading
            | SingleSessionLineStyle::AssistantQuote
            | SingleSessionLineStyle::AssistantLink
    )
}

fn single_session_color_attrs(color: TextColor) -> Attrs<'static> {
    Attrs::new()
        .family(Family::Name(SINGLE_SESSION_FONT_FAMILY))
        .color(color)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn user_prompt_number_color(turn: usize) -> TextColor {
    user_prompt_number_color_for_distance(turn.saturating_sub(1))
}

fn user_prompt_number_color_for_distance(distance: usize) -> TextColor {
    // Match the TUI prompt-number effect: recent prompts start in a softened
    // rainbow and older prompts exponentially decay toward gray.
    const RAINBOW: [[f32; 3]; 7] = [
        [1.000, 0.314, 0.314],
        [1.000, 0.627, 0.314],
        [1.000, 0.902, 0.314],
        [0.314, 0.863, 0.392],
        [0.314, 0.784, 0.863],
        [0.392, 0.549, 1.000],
        [0.706, 0.392, 1.000],
    ];
    const GRAY: [f32; 3] = [0.314, 0.314, 0.314];

    let decay = (-0.4 * distance as f32).exp();
    let rainbow = RAINBOW[distance.min(RAINBOW.len() - 1)];
    text_color([
        rainbow[0] * decay + GRAY[0] * (1.0 - decay),
        rainbow[1] * decay + GRAY[1] * (1.0 - decay),
        rainbow[2] * decay + GRAY[2] * (1.0 - decay),
        1.0,
    ])
}

pub(crate) fn single_session_line_color(style: SingleSessionLineStyle) -> TextColor {
    text_color(single_session_line_rgba(style))
}

fn single_session_line_rgba(style: SingleSessionLineStyle) -> [f32; 4] {
    match style {
        SingleSessionLineStyle::Assistant => ASSISTANT_TEXT_COLOR,
        SingleSessionLineStyle::AssistantHeading => ASSISTANT_HEADING_TEXT_COLOR,
        SingleSessionLineStyle::AssistantQuote => ASSISTANT_QUOTE_TEXT_COLOR,
        SingleSessionLineStyle::AssistantTable => ASSISTANT_TABLE_TEXT_COLOR,
        SingleSessionLineStyle::AssistantLink => ASSISTANT_LINK_TEXT_COLOR,
        SingleSessionLineStyle::Code => CODE_TEXT_COLOR,
        SingleSessionLineStyle::User => USER_TEXT_COLOR,
        SingleSessionLineStyle::UserContinuation => USER_CONTINUATION_TEXT_COLOR,
        SingleSessionLineStyle::Tool => TOOL_TEXT_COLOR,
        SingleSessionLineStyle::Meta | SingleSessionLineStyle::Blank => META_TEXT_COLOR,
        SingleSessionLineStyle::Status => STATUS_TEXT_ACCENT_COLOR,
        SingleSessionLineStyle::Error => ERROR_TEXT_COLOR,
        SingleSessionLineStyle::OverlayTitle => PANEL_TITLE_COLOR,
        SingleSessionLineStyle::Overlay => OVERLAY_TEXT_COLOR,
        SingleSessionLineStyle::OverlaySelection => OVERLAY_SELECTION_TEXT_COLOR,
    }
}

pub(crate) fn single_session_text_areas(
    buffers: &[Buffer],
    size: PhysicalSize<u32>,
) -> Vec<TextArea<'_>> {
    single_session_text_areas_for_fresh_state(buffers, size, false)
}

#[cfg(test)]
pub(crate) fn single_session_text_areas_for_app<'a>(
    app: &SingleSessionApp,
    buffers: &'a [Buffer],
    size: PhysicalSize<u32>,
) -> Vec<TextArea<'a>> {
    single_session_text_areas_for_app_with_scroll(app, buffers, size, 0, 0.0)
}

pub(crate) fn single_session_text_areas_for_app_with_scroll<'a>(
    app: &SingleSessionApp,
    buffers: &'a [Buffer],
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
) -> Vec<TextArea<'a>> {
    let body_top_offset_pixels =
        single_session_body_viewport_for_tick(app, size, tick, smooth_scroll_lines)
            .top_offset_pixels;
    single_session_text_areas_for_state(
        buffers,
        size,
        app.is_welcome_timeline_visible(),
        false,
        body_top_offset_pixels,
        single_session_body_top_for_app(app, size),
        single_session_body_bottom_for_app(app, size) as i32,
        single_session_draft_top_for_app(app, size),
        welcome_timeline_visual_offset_pixels(app, size, smooth_scroll_lines),
        welcome_status_lane_visible(app),
        app.text_scale(),
    )
}

pub(crate) fn single_session_text_areas_for_app_with_cached_body<'a>(
    app: &SingleSessionApp,
    buffers: &'a [Buffer],
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    rendered_body_lines: &[SingleSessionStyledLine],
) -> Vec<TextArea<'a>> {
    let viewport = single_session_body_viewport_from_lines(
        app,
        size,
        smooth_scroll_lines,
        rendered_body_lines,
    );
    single_session_text_areas_for_app_with_cached_body_viewport(
        app,
        buffers,
        size,
        smooth_scroll_lines,
        viewport,
    )
}

pub(crate) fn single_session_text_areas_for_app_with_cached_body_viewport<'a>(
    app: &SingleSessionApp,
    buffers: &'a [Buffer],
    size: PhysicalSize<u32>,
    smooth_scroll_lines: f32,
    viewport: SingleSessionBodyViewport,
) -> Vec<TextArea<'a>> {
    single_session_text_areas_for_state(
        buffers,
        size,
        app.is_welcome_timeline_visible(),
        false,
        viewport.top_offset_pixels,
        single_session_body_top_for_app(app, size),
        single_session_body_bottom_for_total_lines(app, size, viewport.total_lines) as i32,
        single_session_draft_top_for_app(app, size),
        welcome_timeline_visual_offset_pixels_for_total_lines(
            app,
            size,
            smooth_scroll_lines,
            viewport.total_lines,
        ),
        welcome_status_lane_visible(app),
        app.text_scale(),
    )
}

pub(crate) fn single_session_text_areas_for_fresh_state(
    buffers: &[Buffer],
    size: PhysicalSize<u32>,
    fresh_welcome_visible: bool,
) -> Vec<TextArea<'_>> {
    single_session_text_areas_for_state(
        buffers,
        size,
        fresh_welcome_visible,
        false,
        0.0,
        PANEL_BODY_TOP_PADDING,
        single_session_body_bottom(size) as i32,
        single_session_draft_top_for_fresh_state(size, fresh_welcome_visible),
        0.0,
        false,
        1.0,
    )
}

fn welcome_status_lane_visible(app: &SingleSessionApp) -> bool {
    app.is_welcome_timeline_visible()
        && app.has_welcome_timeline_transcript()
        && app.draft.is_empty()
        && app.has_activity_indicator()
}

pub(crate) fn single_session_text_areas_for_state(
    buffers: &[Buffer],
    size: PhysicalSize<u32>,
    welcome_chrome_visible: bool,
    welcome_handoff_visible: bool,
    body_top_offset_pixels: f32,
    body_top: f32,
    body_bottom: i32,
    draft_top: f32,
    welcome_chrome_offset_pixels: f32,
    status_lane_visible: bool,
    ui_scale: f32,
) -> Vec<TextArea<'_>> {
    if buffers.len() < 5 {
        return Vec::new();
    }

    let left = PANEL_TITLE_LEFT_PADDING;
    let right = size.width.saturating_sub(PANEL_TITLE_LEFT_PADDING as u32) as i32;
    let bottom = size.height.saturating_sub(PANEL_TITLE_TOP_PADDING as u32) as i32;
    let body_top = if welcome_handoff_visible {
        draft_top
    } else {
        body_top
    };
    let body_bottom = if welcome_handoff_visible {
        bottom
    } else {
        body_bottom
    };
    let version_label = fresh_welcome_version_label();
    let version_font_size = fresh_welcome_version_font_size() * ui_scale;
    let version_left = if welcome_chrome_visible {
        fresh_welcome_version_left(&version_label, size, version_font_size)
    } else {
        (size.width as f32 * 0.42).max(left + 220.0)
    };
    let version_top = if welcome_chrome_visible {
        fresh_welcome_version_top_for_scale(size, ui_scale) + welcome_chrome_offset_pixels
    } else {
        PANEL_TITLE_TOP_PADDING + 3.0
    };
    let version_bounds_top = if welcome_chrome_visible {
        version_top as i32
    } else {
        0
    };
    let version_bounds_bottom = if welcome_chrome_visible {
        (version_top + version_font_size * 1.4) as i32
    } else {
        64
    };

    let mut areas = Vec::new();

    // Keep the composer lane first in glyphon preparation order. The visual
    // positions are unchanged, but fresh keystrokes get shaped before the
    // heavier transcript/chrome text on frames where both changed.
    if status_lane_visible {
        areas.push(TextArea {
            buffer: &buffers[3],
            left,
            top: draft_top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: draft_top as i32,
                right,
                bottom,
            },
            default_color: text_color(STATUS_TEXT_ACCENT_COLOR),
        });
    } else if !welcome_handoff_visible {
        areas.push(TextArea {
            buffer: &buffers[2],
            left,
            top: draft_top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: draft_top as i32,
                right,
                bottom,
            },
            default_color: text_color(PANEL_SECTION_COLOR),
        });
    }

    if !welcome_chrome_visible && !status_lane_visible {
        areas.push(TextArea {
            buffer: &buffers[3],
            left,
            top: draft_top - SINGLE_SESSION_STATUS_GAP,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: (draft_top - SINGLE_SESSION_STATUS_GAP) as i32,
                right,
                bottom: draft_top as i32,
            },
            default_color: text_color(PANEL_SECTION_COLOR),
        });
    }

    areas.push(TextArea {
        buffer: &buffers[0],
        left,
        top: PANEL_TITLE_TOP_PADDING,
        scale: 1.0,
        bounds: TextBounds {
            left: 0,
            top: 0,
            right,
            bottom: 64,
        },
        default_color: text_color(PANEL_TITLE_COLOR),
    });
    areas.push(TextArea {
        buffer: &buffers[4],
        left: version_left,
        top: version_top,
        scale: 1.0,
        bounds: TextBounds {
            left: 0,
            top: version_bounds_top,
            right,
            bottom: version_bounds_bottom,
        },
        default_color: text_color(META_TEXT_COLOR),
    });
    areas.push(TextArea {
        buffer: &buffers[1],
        left,
        top: body_top + body_top_offset_pixels,
        scale: 1.0,
        bounds: TextBounds {
            left: 0,
            top: body_top as i32,
            right,
            bottom: body_bottom,
        },
        default_color: text_color(ASSISTANT_TEXT_COLOR),
    });

    areas
}

fn visualize_composer_whitespace(text: &str) -> String {
    text.to_string()
}

pub(crate) fn desktop_header_version_label() -> String {
    let version = option_env!("JCODE_DESKTOP_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    let binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unknown binary".to_string());
    format!("{binary} · {version}")
}

pub(crate) fn fresh_welcome_version_label() -> String {
    let version = option_env!("JCODE_PRODUCT_VERSION")
        .or(option_env!("JCODE_DESKTOP_VERSION"))
        .unwrap_or(env!("CARGO_PKG_VERSION"));
    format!("jcode {version}")
}

fn fresh_welcome_version_font_size() -> f32 {
    (single_session_typography().meta_size * 0.58).clamp(11.0, 14.0)
}

fn fresh_welcome_version_top(size: PhysicalSize<u32>) -> f32 {
    fresh_welcome_version_top_for_scale(size, 1.0)
}

fn fresh_welcome_version_top_for_scale(size: PhysicalSize<u32>, ui_scale: f32) -> f32 {
    handwritten_welcome_bounds_for_phrase_with_scale(size, handwritten_welcome_phrase(0), ui_scale)
        .1[1]
        + fresh_welcome_version_gap_for_scale(ui_scale)
}

fn fresh_welcome_version_gap_for_scale(ui_scale: f32) -> f32 {
    (fresh_welcome_version_font_size() * ui_scale * 2.25).max(30.0 * ui_scale)
}

fn fresh_welcome_version_left(label: &str, size: PhysicalSize<u32>, font_size: f32) -> f32 {
    let estimated_width = label.chars().count() as f32 * font_size * 0.58;
    ((size.width as f32 - estimated_width) * 0.5).max(PANEL_TITLE_LEFT_PADDING)
}

pub(crate) fn text_color(color: [f32; 4]) -> TextColor {
    TextColor::rgba(
        (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}
