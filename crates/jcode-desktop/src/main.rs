mod animation;
mod desktop_prefs;
mod power_inhibit;
mod render_helpers;
mod session_data;
mod session_launch;
mod single_session;
mod single_session_render;
mod workspace;

use animation::{AnimatedViewport, FocusPulse, VisibleColumnLayout, WorkspaceRenderLayout};
use anyhow::{Context, Result};
use base64::Engine;
use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Wrap,
};
use render_helpers::*;
use single_session::{
    SINGLE_SESSION_ASSISTANT_FONT_FAMILY, SINGLE_SESSION_FONT_FAMILY, SelectionPoint,
    SingleSessionApp, SingleSessionLineStyle, SingleSessionMessage, SingleSessionStyledLine,
    handwritten_welcome_phrase, single_session_surface, single_session_typography,
    single_session_typography_for_scale,
};
use single_session_render::*;
use wgpu::{CompositeAlphaMode, PresentMode, SurfaceError, TextureUsages};
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowBuilder};
use workspace::{InputMode, KeyInput, KeyOutcome, PanelSizePreset, Workspace};

use std::collections::hash_map::DefaultHasher;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEFAULT_WINDOW_WIDTH: f64 = 1280.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;
const OUTER_PADDING: f32 = 8.0;
const GAP: f32 = 6.0;
const STATUS_BAR_HEIGHT: f32 = 30.0;
const FOCUSED_BORDER_WIDTH: f32 = 2.0;
const UNFOCUSED_BORDER_WIDTH: f32 = 1.5;
const PANEL_RADIUS: f32 = 8.0;
const STATUS_RADIUS: f32 = 7.0;
const ROUNDED_CORNER_SEGMENTS: usize = 6;
const PANEL_FIT_TOLERANCE: f32 = 0.15;
const STATUS_PREVIEW_LANE_RADIUS: i32 = 2;
const STATUS_PREVIEW_MAX_WIDTH: f32 = 420.0;
const STATUS_PREVIEW_HEIGHT: f32 = 14.0;
const STATUS_PREVIEW_PANEL_WIDTH: f32 = 9.0;
const STATUS_PREVIEW_PANEL_GAP: f32 = 2.0;
const STATUS_PREVIEW_GROUP_GAP: f32 = 10.0;
const STATUS_PREVIEW_SIDE_RESERVE: f32 = 74.0;
const WORKSPACE_NUMBER_LEFT_PADDING: f32 = 14.0;
const WORKSPACE_NUMBER_DIGIT_WIDTH: f32 = 8.0;
const WORKSPACE_NUMBER_DIGIT_HEIGHT: f32 = 14.0;
const WORKSPACE_NUMBER_DIGIT_GAP: f32 = 4.0;
const WORKSPACE_NUMBER_STROKE: f32 = 2.0;
const BITMAP_TEXT_PIXEL: f32 = 2.0;
const STATUS_TEXT_RIGHT_PADDING: f32 = 14.0;
const PANEL_TITLE_LEFT_PADDING: f32 = 12.0;
const PANEL_TITLE_TOP_PADDING: f32 = 12.0;
const PANEL_BODY_TOP_PADDING: f32 = 38.0;
const PANEL_BODY_LINE_GAP: f32 = 8.0;
const SINGLE_SESSION_DRAFT_TOP_OFFSET: f32 = 158.0;
const SINGLE_SESSION_STATUS_GAP: f32 = 30.0;
const SINGLE_SESSION_CARET_WIDTH: f32 = 2.0;
const SINGLE_SESSION_CARET_COLOR: [f32; 4] = [0.130, 0.150, 0.190, 0.92];
const SESSION_SPAWN_REFRESH_DELAY: Duration = Duration::from_millis(350);
const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_millis(33);
const HEADLESS_CHAT_SMOKE_TIMEOUT: Duration = Duration::from_secs(90);
const DESKTOP_SPINNER_FRAME_MS: u128 = 180;
const MOUSE_WHEEL_LINES_PER_DETENT: f32 = 3.0;
const MAX_MOUSE_SCROLL_LINES_PER_EVENT: f32 = 24.0;
const SCROLL_GESTURE_IDLE_RESET: Duration = Duration::from_millis(180);
const SCROLL_FRACTIONAL_EPSILON: f32 = 0.000_1;
const SCROLL_MOMENTUM_GAIN: f32 = 8.5;
const SCROLL_MOMENTUM_DECAY_PER_SECOND: f32 = 7.0;
const SCROLL_MOMENTUM_MAX_VELOCITY: f32 = 72.0;
const SCROLL_MOMENTUM_STOP_VELOCITY: f32 = 0.08;
const SCROLL_FRAME_MAX_DT_SECONDS: f32 = 0.050;
const SINGLE_SESSION_BODY_TEXT_WINDOW_BEFORE_LINES: usize = 48;
const SINGLE_SESSION_BODY_TEXT_WINDOW_AFTER_LINES: usize = 96;
const DESKTOP_120FPS_FRAME_BUDGET: Duration = Duration::from_micros(8_333);
const DESKTOP_FRAME_PROFILE_REPORT_INTERVAL: Duration = Duration::from_secs(1);

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.955,
    g: 0.965,
    b: 0.985,
    a: 1.0,
};

const BACKGROUND_TOP_LEFT: [f32; 4] = [0.858, 0.910, 1.000, 1.0];
const BACKGROUND_TOP_RIGHT: [f32; 4] = [0.945, 0.884, 1.000, 1.0];
const BACKGROUND_BOTTOM_RIGHT: [f32; 4] = [0.846, 0.972, 0.910, 1.0];
const BACKGROUND_BOTTOM_LEFT: [f32; 4] = [0.930, 0.950, 0.988, 1.0];
const FOCUS_RING_COLOR: [f32; 4] = [0.165, 0.185, 0.225, 0.94];
const NAV_STATUS_COLOR: [f32; 4] = [0.184, 0.204, 0.251, 1.0];
const INSERT_STATUS_COLOR: [f32; 4] = [0.310, 0.435, 0.376, 1.0];
const STATUS_PREVIEW_ACTIVE_GROUP_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.16];
const STATUS_PREVIEW_EMPTY_FOCUSED_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.50];
const STATUS_PREVIEW_VIEWPORT_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.78];
const WORKSPACE_NUMBER_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.90];
const STATUS_TEXT_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.88];
const PANEL_TITLE_COLOR: [f32; 4] = [0.010, 0.014, 0.025, 1.0];
const PANEL_BODY_COLOR: [f32; 4] = [0.008, 0.012, 0.020, 1.0];
const ASSISTANT_TEXT_COLOR: [f32; 4] = [0.000, 0.060, 0.072, 1.0];
const ASSISTANT_HEADING_TEXT_COLOR: [f32; 4] = [0.012, 0.080, 0.250, 1.0];
const ASSISTANT_QUOTE_TEXT_COLOR: [f32; 4] = [0.145, 0.055, 0.275, 1.0];
const ASSISTANT_TABLE_TEXT_COLOR: [f32; 4] = [0.000, 0.120, 0.145, 1.0];
const ASSISTANT_LINK_TEXT_COLOR: [f32; 4] = [0.000, 0.095, 0.315, 1.0];
const USER_TEXT_COLOR: [f32; 4] = [0.012, 0.030, 0.180, 1.0];
const USER_CONTINUATION_TEXT_COLOR: [f32; 4] = [0.018, 0.035, 0.155, 1.0];
const TOOL_TEXT_COLOR: [f32; 4] = [0.225, 0.105, 0.000, 1.0];
const META_TEXT_COLOR: [f32; 4] = [0.055, 0.070, 0.105, 1.0];
const CODE_TEXT_COLOR: [f32; 4] = [0.045, 0.055, 0.080, 1.0];
const STATUS_TEXT_ACCENT_COLOR: [f32; 4] = [0.030, 0.125, 0.080, 1.0];
const ERROR_TEXT_COLOR: [f32; 4] = [0.360, 0.000, 0.000, 1.0];
const OVERLAY_TEXT_COLOR: [f32; 4] = [0.030, 0.045, 0.075, 1.0];
const OVERLAY_SELECTION_TEXT_COLOR: [f32; 4] = [0.010, 0.035, 0.105, 1.0];
const USER_PROMPT_ACCENT_COLOR: [f32; 4] = [0.000, 0.105, 0.250, 1.0];
const PANEL_SECTION_COLOR: [f32; 4] = [0.045, 0.055, 0.080, 0.95];
const SELECTION_HIGHLIGHT_COLOR: [f32; 4] = [0.220, 0.420, 0.700, 0.22];
const STREAMING_SHIMMER_SOFT_COLOR: [f32; 4] = [0.220, 0.520, 0.780, 0.055];
const STREAMING_SHIMMER_CORE_COLOR: [f32; 4] = [0.220, 0.520, 0.780, 0.115];
const WELCOME_AURORA_BLUE: [f32; 4] = [0.250, 0.520, 1.000, 0.145];
const WELCOME_AURORA_VIOLET: [f32; 4] = [0.720, 0.360, 0.980, 0.125];
const WELCOME_AURORA_MINT: [f32; 4] = [0.220, 0.840, 0.660, 0.115];
const WELCOME_AURORA_WARM: [f32; 4] = [1.000, 0.620, 0.360, 0.075];
const WELCOME_HANDWRITING_COLOR: [f32; 4] = [0.012, 0.080, 0.250, 0.94];
const WELCOME_HANDWRITING_HIGHLIGHT_COLOR: [f32; 4] = [0.200, 0.520, 1.000, 0.42];
const NATIVE_SPINNER_TRACK_COLOR: [f32; 4] = [0.105, 0.135, 0.190, 0.16];
const NATIVE_SPINNER_HEAD_COLOR: [f32; 4] = [0.045, 0.185, 0.470, 0.96];
const CODE_BLOCK_BACKGROUND_COLOR: [f32; 4] = [0.075, 0.095, 0.135, 0.075];
const QUOTE_CARD_BACKGROUND_COLOR: [f32; 4] = [0.520, 0.330, 0.760, 0.070];
const TABLE_CARD_BACKGROUND_COLOR: [f32; 4] = [0.080, 0.460, 0.520, 0.060];
const ERROR_CARD_BACKGROUND_COLOR: [f32; 4] = [0.850, 0.170, 0.170, 0.105];
const OVERLAY_SELECTION_BACKGROUND_COLOR: [f32; 4] = [0.280, 0.470, 0.780, 0.115];
const STATUS_PREVIEW_ACCENTS: [[f32; 3]; 8] = [
    [0.560, 0.690, 0.980],
    [0.780, 0.610, 0.910],
    [0.520, 0.760, 0.620],
    [0.900, 0.650, 0.450],
    [0.600, 0.780, 0.840],
    [0.880, 0.580, 0.690],
    [0.720, 0.740, 0.820],
    [0.810, 0.760, 0.520],
];

const SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let startup_benchmark = startup_benchmark_requested(&args);
    let startup_trace = DesktopStartupTrace::new(startup_benchmark || startup_log_requested(&args));
    startup_trace.mark("args parsed");
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", desktop_help_text());
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("{}", desktop_header_version_label());
        return Ok(());
    }
    if let Some(message) = headless_chat_smoke_message(&args) {
        return run_headless_chat_smoke(message);
    }
    if let Some(frames) = scroll_render_benchmark_frames(&args) {
        return run_scroll_render_benchmark(frames);
    }
    let fullscreen = args.iter().any(|arg| arg == "--fullscreen");
    let desktop_mode = desktop_mode_from_args(args.iter().map(String::as_str));
    let event_loop = EventLoopBuilder::<DesktopUserEvent>::with_user_event()
        .build()
        .context("failed to create event loop")?;
    let event_loop_proxy = event_loop.create_proxy();
    startup_trace.mark("event loop created");
    let mut window_builder = WindowBuilder::new()
        .with_title("Jcode Desktop")
        .with_inner_size(LogicalSize::new(
            DEFAULT_WINDOW_WIDTH,
            DEFAULT_WINDOW_HEIGHT,
        ));

    if fullscreen {
        window_builder = window_builder.with_fullscreen(Some(Fullscreen::Borderless(None)));
    }

    let window: &'static Window = Box::leak(Box::new(
        window_builder
            .build(&event_loop)
            .context("failed to create desktop window")?,
    ));
    startup_trace.mark("window created");

    let mut app = if desktop_mode == DesktopMode::WorkspacePrototype {
        let session_cards = load_session_cards_for_desktop();
        let mut workspace = Workspace::from_session_cards(session_cards);
        if let Some(preferences) = load_desktop_preferences() {
            workspace.apply_preferences(preferences);
        }
        DesktopApp::Workspace(workspace)
    } else {
        fresh_single_session_app()
    };
    startup_trace.mark("app state initialized");
    window.set_title(&app.status_title());
    let mut canvas = Canvas::new(window, startup_trace).await?;
    startup_trace.mark("canvas ready");
    let mut modifiers = ModifiersState::empty();
    let mut cursor_position = winit::dpi::PhysicalPosition::new(0.0, 0.0);
    let mut selecting_body = false;
    let mut selecting_draft = false;
    let mut scroll_accumulator = ScrollLineAccumulator::default();
    let mut scroll_metrics_cache = SingleSessionScrollMetricsCache::default();
    let mut hot_reloader = DesktopHotReloader::new();
    let mut power_inhibitor = power_inhibit::PowerInhibitor::new();
    let (session_event_tx, session_event_rx) = mpsc::channel();
    spawn_session_event_forwarder(session_event_rx, event_loop_proxy.clone());
    let mut recovery_scan_pending = app.is_single_session();
    let mut first_frame_presented = false;

    event_loop.run(move |event, target| {
        let has_background_work = app.has_background_work();
        power_inhibitor.set_active(has_background_work);
        if has_background_work || app.has_frame_animation() {
            target.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + BACKGROUND_POLL_INTERVAL,
            ));
        } else {
            target.set_control_flow(ControlFlow::Wait);
        }

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    canvas.resize(size);
                    scroll_metrics_cache.clear();
                    window.request_redraw();
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    canvas.resize(window.inner_size());
                    scroll_metrics_cache.clear();
                    window.request_redraw();
                }
                WindowEvent::ModifiersChanged(new_modifiers) => {
                    modifiers = new_modifiers.state();
                }
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let size = window.inner_size();
                    let now = Instant::now();
                    let previous_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    let mut should_redraw = false;
                    if !app.is_single_session() {
                        scroll_accumulator.reset();
                        scroll_metrics_cache.clear();
                    } else if let Some(lines) = scroll_accumulator.scroll_lines(delta, now) {
                        should_redraw |=
                            app.scroll_single_session_body(lines, size, &mut scroll_metrics_cache);
                    }
                    if matches!(phase, TouchPhase::Cancelled) {
                        scroll_accumulator.reset();
                    }
                    let next_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    should_redraw |= (next_smooth_scroll - previous_smooth_scroll).abs()
                        >= SCROLL_FRACTIONAL_EPSILON;
                    if should_redraw {
                        window.request_redraw();
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_position = position;
                    if selecting_draft
                        && app.update_single_session_draft_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        )
                    {
                        window.request_redraw();
                    } else if selecting_body
                        && app.update_single_session_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        )
                    {
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => match state {
                    ElementState::Pressed => {
                        if app.begin_single_session_draft_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        ) {
                            selecting_body = false;
                            selecting_draft = true;
                            window.set_title(&app.status_title());
                            window.request_redraw();
                            return;
                        }

                        selecting_draft = false;
                        selecting_body = app.begin_single_session_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        );
                        if selecting_body {
                            window.request_redraw();
                        }
                    }
                    ElementState::Released => {
                        if selecting_draft {
                            app.update_single_session_draft_selection_at(
                                cursor_position.x as f32,
                                cursor_position.y as f32,
                                window.inner_size(),
                            );
                            selecting_draft = false;
                            let selected = app.selected_single_session_draft_text();
                            if let Some(text) = selected {
                                copy_text_to_clipboard(&text, "copied input selection", &mut app);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        } else if selecting_body {
                            app.update_single_session_selection_at(
                                cursor_position.x as f32,
                                cursor_position.y as f32,
                                window.inner_size(),
                            );
                            selecting_body = false;
                            let selected = app.selected_single_session_text(window.inner_size());
                            if let Some(text) = selected {
                                copy_text_to_clipboard(&text, "copied selection", &mut app);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                    }
                },
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    let keyboard_started = Instant::now();
                    let size = window.inner_size();
                    let had_smooth_scroll = app
                        .single_session_smooth_scroll_lines(
                            scroll_accumulator.pending_lines(),
                            size,
                            &mut scroll_metrics_cache,
                        )
                        .abs()
                        >= SCROLL_FRACTIONAL_EPSILON;
                    scroll_accumulator.reset();
                    if had_smooth_scroll {
                        window.request_redraw();
                    }
                    let key_input = to_key_input(&event.logical_key, modifiers);
                    let key_debug = format!("{key_input:?}");
                    if key_input == KeyInput::RefreshSessions && app.is_workspace() {
                        if let DesktopApp::Workspace(workspace) = &mut app {
                            workspace.replace_session_cards(load_session_cards_for_desktop());
                            save_desktop_preferences(workspace);
                        }
                        window.set_title(&app.status_title());
                        window.request_redraw();
                        return;
                    }

                    match app.handle_key(key_input) {
                        KeyOutcome::Exit => target.exit(),
                        KeyOutcome::Redraw => {
                            if let DesktopApp::Workspace(workspace) = &app {
                                save_desktop_preferences(workspace);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::OpenSession { session_id, title } => {
                            if let DesktopApp::Workspace(workspace) = &app {
                                save_desktop_preferences(workspace);
                            }
                            if let Err(error) =
                                session_launch::launch_validated_resume_session(&session_id, &title)
                            {
                                eprintln!(
                                    "jcode-desktop: failed to open session {session_id}: {error:#}"
                                );
                            }
                        }
                        KeyOutcome::SpawnSession => {
                            if let DesktopApp::SingleSession(app) = &mut app {
                                app.reset_fresh_session();
                                window.set_title(&app.status_title());
                                window.request_redraw();
                                return;
                            }

                            if let Err(error) = session_launch::launch_new_session() {
                                eprintln!("jcode-desktop: failed to spawn session: {error:#}");
                            } else {
                                std::thread::sleep(SESSION_SPAWN_REFRESH_DELAY);
                                app.refresh_sessions();
                                if let DesktopApp::Workspace(workspace) = &app {
                                    save_desktop_preferences(workspace);
                                }
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            }
                        }
                        KeyOutcome::SendDraft {
                            session_id,
                            title,
                            message,
                            images,
                        } => {
                            if app.is_single_session() {
                                match session_launch::spawn_message_to_session(
                                    session_id.clone(),
                                    message,
                                    images,
                                    session_event_tx.clone(),
                                ) {
                                    Ok(handle) => app.set_single_session_handle(handle),
                                    Err(error) => apply_single_session_error(&mut app, error),
                                }
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            } else if !images.is_empty() {
                                match session_launch::spawn_message_to_session(
                                    session_id.clone(),
                                    message,
                                    images,
                                    session_event_tx.clone(),
                                ) {
                                    Ok(_handle) => {
                                        std::thread::sleep(SESSION_SPAWN_REFRESH_DELAY);
                                        app.refresh_sessions();
                                        if let DesktopApp::Workspace(workspace) = &app {
                                            save_desktop_preferences(workspace);
                                        }
                                        window.set_title(&app.status_title());
                                        window.request_redraw();
                                    }
                                    Err(error) => eprintln!(
                                        "jcode-desktop: failed to send image draft to {session_id}: {error:#}"
                                    ),
                                }
                            } else if let Err(error) = session_launch::send_message_to_session(
                                &session_id,
                                &title,
                                &message,
                            ) {
                                eprintln!(
                                    "jcode-desktop: failed to send draft to {session_id}: {error:#}"
                                );
                            } else {
                                std::thread::sleep(SESSION_SPAWN_REFRESH_DELAY);
                                app.refresh_sessions();
                                if let DesktopApp::Workspace(workspace) = &app {
                                    save_desktop_preferences(workspace);
                                }
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            }
                        }
                        KeyOutcome::StartFreshSession { message, images } => {
                            match session_launch::spawn_fresh_server_session(
                                message,
                                images,
                                session_event_tx.clone(),
                            ) {
                                Ok(handle) => app.set_single_session_handle(handle),
                                Err(error) => apply_single_session_error(&mut app, error),
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CancelGeneration => {
                            app.cancel_single_session_generation();
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CopyLatestResponse(text) => {
                            copy_text_to_clipboard(&text, "copied latest response", &mut app);
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CutDraftToClipboard(text) => {
                            copy_text_to_clipboard(&text, "cut input line", &mut app);
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CycleModel(direction) => {
                            if let Err(error) = session_launch::spawn_cycle_model(
                                direction,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(
                                    session_launch::DesktopSessionEvent::Status(
                                        "switching model".to_string(),
                                    ),
                                );
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::LoadModelCatalog => {
                            if let Err(error) = session_launch::spawn_load_model_catalog(
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::LoadSessionSwitcher => {
                            app.apply_single_session_switcher_cards(load_session_cards_for_desktop());
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::RestoreCrashedSessions => {
                            let crashed = load_crashed_session_cards_for_desktop();
                            if crashed.is_empty() {
                                apply_single_session_error(
                                    &mut app,
                                    anyhow::anyhow!("no crashed sessions found"),
                                );
                            } else {
                                for card in &crashed {
                                    if let Err(error) = session_launch::launch_validated_resume_session(
                                        &card.session_id,
                                        &card.title,
                                    ) {
                                        eprintln!(
                                            "jcode-desktop: failed to restore crashed session {}: {error:#}",
                                            card.session_id
                                        );
                                    }
                                }
                                if let DesktopApp::SingleSession(single_session) = &mut app {
                                    single_session.set_recovery_session_count(0);
                                }
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetModel(model) => {
                            if let Err(error) = session_launch::spawn_set_model(
                                model,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(
                                    session_launch::DesktopSessionEvent::Status(
                                        "switching model".to_string(),
                                    ),
                                );
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SendStdinResponse { request_id, input } => {
                            if let Err(error) = app.send_single_session_stdin_response(request_id, input)
                            {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::AttachClipboardImage => {
                            match clipboard_image_png_base64() {
                                Ok((media_type, base64_data)) => {
                                    app.attach_clipboard_image(media_type, base64_data);
                                }
                                Err(error) => apply_single_session_error(&mut app, error),
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::PasteText => {
                            if let Err(error) = paste_clipboard_into_app(&mut app) {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::None => {}
                    }
                    log_desktop_slow_interaction(
                        "keyboard_input",
                        keyboard_started.elapsed(),
                        serde_json::json!({ "key": key_debug }),
                    );
                }
                WindowEvent::RedrawRequested => {
                    let smooth_scroll_lines = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        window.inner_size(),
                        &mut scroll_metrics_cache,
                    );
                    match canvas.render(
                        &app,
                        window.current_monitor().map(|monitor| monitor.size()),
                        smooth_scroll_lines,
                    ) {
                    Ok(animation_active) => {
                        if !first_frame_presented {
                            first_frame_presented = true;
                            startup_trace.mark("first frame presented");
                            if startup_benchmark {
                                target.exit();
                                return;
                            }
                            if recovery_scan_pending {
                                recovery_scan_pending = false;
                                spawn_recovery_session_count_scan(
                                    event_loop_proxy.clone(),
                                    startup_trace,
                                );
                            }
                        }
                        if animation_active {
                            window.request_redraw();
                        }
                    }
                    Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                        canvas.resize(window.inner_size());
                        window.request_redraw();
                    }
                    Err(SurfaceError::OutOfMemory) => target.exit(),
                    Err(SurfaceError::Timeout) => {
                        window.request_redraw();
                    }
                    }
                }
                _ => {}
            },
            Event::UserEvent(DesktopUserEvent::RecoveryCount(recovery_count)) => {
                if let DesktopApp::SingleSession(single_session) = &mut app {
                    single_session.set_recovery_session_count(recovery_count);
                    window.set_title(&app.status_title());
                    window.request_redraw();
                }
            }
            Event::UserEvent(DesktopUserEvent::SessionEvents(events)) => {
                if apply_desktop_session_event_batch(&mut app, events) {
                    if let Some(session_id) = app.single_session_live_id() {
                        attach_single_session_by_id(&mut app, &session_id);
                    }
                    if let Some((message, images)) = app.take_next_queued_single_session_draft() {
                        let result = if let Some(session_id) = app.single_session_live_id() {
                            session_launch::spawn_message_to_session(
                                session_id,
                                message,
                                images,
                                session_event_tx.clone(),
                            )
                        } else {
                            session_launch::spawn_fresh_server_session(
                                message,
                                images,
                                session_event_tx.clone(),
                            )
                        };
                        match result {
                            Ok(handle) => app.set_single_session_handle(handle),
                            Err(error) => apply_single_session_error(&mut app, error),
                        }
                    }
                    window.set_title(&app.status_title());
                    window.request_redraw();
                }
            }
            Event::AboutToWait => {
                if app.is_single_session() {
                    let size = window.inner_size();
                    let previous_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    let frame = scroll_accumulator.frame(Instant::now());
                    if let Some(lines) = frame.scroll_lines
                        && !app.scroll_single_session_body(lines, size, &mut scroll_metrics_cache)
                    {
                        scroll_accumulator.stop();
                    }
                    let next_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    if frame.active
                        || (next_smooth_scroll - previous_smooth_scroll).abs()
                            >= SCROLL_FRACTIONAL_EPSILON
                    {
                        window.request_redraw();
                    }
                } else if scroll_accumulator.is_active() {
                    scroll_accumulator.reset();
                    scroll_metrics_cache.clear();
                }
                if let Some(relaunch) = hot_reloader.poll() {
                    if let Err(error) = relaunch.spawn() {
                        eprintln!("jcode-desktop: failed to hot reload desktop: {error:#}");
                    } else {
                        target.exit();
                        return;
                    }
                }

                if canvas.needs_initial_frame {
                    canvas.needs_initial_frame = false;
                    window.request_redraw();
                } else if app.has_frame_animation() {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}

fn load_session_cards_for_desktop() -> Vec<workspace::SessionCard> {
    match session_data::load_recent_session_cards() {
        Ok(cards) => cards,
        Err(error) => {
            eprintln!("jcode-desktop: failed to load session metadata: {error:#}");
            Vec::new()
        }
    }
}

fn load_crashed_session_cards_for_desktop() -> Vec<workspace::SessionCard> {
    match session_data::load_crashed_session_cards() {
        Ok(cards) => cards,
        Err(error) => {
            eprintln!("jcode-desktop: failed to load crashed session metadata: {error:#}");
            Vec::new()
        }
    }
}

fn spawn_recovery_session_count_scan(
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
    startup_trace: DesktopStartupTrace,
) {
    if let Err(error) = std::thread::Builder::new()
        .name("jcode-desktop-recovery-scan".to_string())
        .spawn(move || {
            startup_trace.mark("recovery scan started");
            let recovery_count = load_crashed_session_cards_for_desktop().len();
            startup_trace.mark(&format!(
                "recovery scan completed ({recovery_count} crashed)"
            ));
            let _ = event_loop_proxy.send_event(DesktopUserEvent::RecoveryCount(recovery_count));
        })
    {
        eprintln!("jcode-desktop: failed to start recovery scan: {error:#}");
    }
}

fn headless_chat_smoke_message(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--headless-chat-smoke=")
            .map(ToOwned::to_owned)
            .or_else(|| {
                (arg == "--headless-chat-smoke")
                    .then(|| args.get(index + 1).cloned())
                    .flatten()
            })
    })
}

const DESKTOP_HELP_LINES: &[&str] = &[
    "Jcode Desktop",
    "",
    "Usage:",
    "  jcode-desktop [OPTIONS]",
    "",
    "Options:",
    "  --fullscreen                 Start borderless fullscreen",
    "  --workspace                  Open the workspace prototype instead of the single-session chat",
    "  --startup-log                Print launch timing milestones to stderr",
    "  --startup-benchmark          Print launch timings and exit after the first frame",
    "  --scroll-render-benchmark[N]  Print CPU scroll/render benchmark JSON and exit",
    "  --headless-chat-smoke <MSG>  Run a hidden backend smoke test and print JSON events",
    "  --headless-chat-smoke=<MSG>  Same as above",
    "  -V, --version                Print version information",
    "  -h, --help                   Print this help",
    "",
];

fn desktop_help_text() -> String {
    DESKTOP_HELP_LINES.join("\n")
}

fn startup_log_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--startup-log")
        || std::env::var_os("JCODE_DESKTOP_STARTUP_LOG").is_some_and(env_flag_enabled)
}

fn startup_benchmark_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--startup-benchmark")
}

fn scroll_render_benchmark_frames(args: &[String]) -> Option<usize> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--scroll-render-benchmark=")
            .and_then(|value| value.parse::<usize>().ok())
            .or_else(|| {
                (arg == "--scroll-render-benchmark").then(|| {
                    args.get(index + 1)
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(600)
                })
            })
    })
}

fn env_flag_enabled(value: OsString) -> bool {
    let value = value.to_string_lossy();
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no"
    )
}

#[derive(Clone, Copy)]
struct DesktopStartupTrace {
    started_at: Instant,
    enabled: bool,
}

impl DesktopStartupTrace {
    fn new(enabled: bool) -> Self {
        Self {
            started_at: Instant::now(),
            enabled,
        }
    }

    fn mark(&self, milestone: &str) {
        if self.enabled {
            eprintln!(
                "jcode-desktop startup +{:>7.2} ms  {milestone}",
                self.started_at.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

#[derive(Debug)]
enum DesktopUserEvent {
    SessionEvents(Vec<session_launch::DesktopSessionEvent>),
    RecoveryCount(usize),
}

fn spawn_session_event_forwarder(
    session_event_rx: mpsc::Receiver<session_launch::DesktopSessionEvent>,
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
) {
    if let Err(error) = std::thread::Builder::new()
        .name("jcode-desktop-session-event-forwarder".to_string())
        .spawn(move || {
            while let Ok(first_event) = session_event_rx.recv() {
                let mut events = vec![first_event];
                while let Ok(event) = session_event_rx.try_recv() {
                    events.push(event);
                }
                let events = coalesce_desktop_session_events(events);
                if events.is_empty() {
                    continue;
                }
                if event_loop_proxy
                    .send_event(DesktopUserEvent::SessionEvents(events))
                    .is_err()
                {
                    break;
                }
            }
        })
    {
        eprintln!("jcode-desktop: failed to start session event forwarder: {error:#}");
    }
}

fn coalesce_desktop_session_events(
    events: Vec<session_launch::DesktopSessionEvent>,
) -> Vec<session_launch::DesktopSessionEvent> {
    let mut coalesced = Vec::with_capacity(events.len());
    for event in events {
        match event {
            session_launch::DesktopSessionEvent::TextDelta(delta) if !delta.is_empty() => {
                if let Some(session_launch::DesktopSessionEvent::TextDelta(existing)) =
                    coalesced.last_mut()
                {
                    existing.push_str(&delta);
                } else {
                    coalesced.push(session_launch::DesktopSessionEvent::TextDelta(delta));
                }
            }
            session_launch::DesktopSessionEvent::ToolInput { delta } if !delta.is_empty() => {
                if let Some(session_launch::DesktopSessionEvent::ToolInput { delta: existing }) =
                    coalesced.last_mut()
                {
                    existing.push_str(&delta);
                } else {
                    coalesced.push(session_launch::DesktopSessionEvent::ToolInput { delta });
                }
            }
            session_launch::DesktopSessionEvent::Status(status) => {
                if let Some(session_launch::DesktopSessionEvent::Status(existing)) =
                    coalesced.last_mut()
                {
                    *existing = status;
                } else {
                    coalesced.push(session_launch::DesktopSessionEvent::Status(status));
                }
            }
            event => coalesced.push(event),
        }
    }
    coalesced
}

fn run_headless_chat_smoke(message: String) -> Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("headless chat smoke message cannot be empty");
    }

    let (event_tx, event_rx) = mpsc::channel();
    let _handle = session_launch::spawn_fresh_server_session(message, Vec::new(), event_tx)
        .context("failed to start desktop headless chat smoke")?;
    let started = Instant::now();
    let mut session_id = None;
    let mut response = String::new();
    let mut last_status = None;

    while started.elapsed() < HEADLESS_CHAT_SMOKE_TIMEOUT {
        let remaining = HEADLESS_CHAT_SMOKE_TIMEOUT.saturating_sub(started.elapsed());
        let poll = remaining.min(Duration::from_millis(250));
        let event = match event_rx.recv_timeout(poll) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!(
                    "desktop chat smoke worker disconnected before completion; last_status={}",
                    last_status.as_deref().unwrap_or("unknown")
                );
            }
        };

        match event {
            session_launch::DesktopSessionEvent::Status(status) => {
                last_status = Some(status.clone());
                println!(
                    "{}",
                    serde_json::json!({"event": "status", "status": status})
                );
            }
            session_launch::DesktopSessionEvent::SessionStarted { session_id: id } => {
                session_id = Some(id.clone());
                println!(
                    "{}",
                    serde_json::json!({"event": "session", "session_id": id})
                );
            }
            session_launch::DesktopSessionEvent::TextDelta(text) => {
                response.push_str(&text);
                println!(
                    "{}",
                    serde_json::json!({"event": "text_delta", "chars": text.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::TextReplace(text) => {
                response = text;
                println!(
                    "{}",
                    serde_json::json!({"event": "text_replace", "chars": response.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::ToolStarted { name } => {
                last_status = Some(format!("preparing tool {name}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_started", "name": name})
                );
            }
            session_launch::DesktopSessionEvent::ToolExecuting { name } => {
                last_status = Some(format!("using tool {name}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_executing", "name": name})
                );
            }
            session_launch::DesktopSessionEvent::ToolInput { delta } => {
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_input", "chars": delta.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::ToolFinished {
                name,
                summary,
                is_error,
            } => {
                last_status = Some(if is_error {
                    format!("tool {name} failed")
                } else {
                    format!("tool {name} done")
                });
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "tool_finished",
                        "name": name,
                        "summary": summary,
                        "is_error": is_error,
                    })
                );
            }
            session_launch::DesktopSessionEvent::Reloading { new_socket } => {
                last_status = Some("server reloading, reconnecting".to_string());
                println!(
                    "{}",
                    serde_json::json!({"event": "reloading", "new_socket": new_socket})
                );
            }
            session_launch::DesktopSessionEvent::ModelChanged {
                model,
                provider_name,
                error,
            } => {
                if let Some(error) = error {
                    last_status = Some(format!("model switch failed: {error}"));
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "model_changed",
                            "model": model,
                            "provider_name": provider_name,
                            "error": error,
                        })
                    );
                    continue;
                }
                let label = provider_name
                    .as_deref()
                    .map(|provider| format!("{provider} · {model}"))
                    .unwrap_or_else(|| model.clone());
                last_status = Some(format!("model: {label}"));
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "model_changed",
                        "model": model,
                        "provider_name": provider_name,
                    })
                );
            }
            session_launch::DesktopSessionEvent::ModelCatalog {
                current_model,
                provider_name,
                models,
            } => {
                last_status = Some(format!("models loaded ({})", models.len()));
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "model_catalog",
                        "current_model": current_model,
                        "provider_name": provider_name,
                        "models": models.len(),
                    })
                );
            }
            session_launch::DesktopSessionEvent::ModelCatalogError { error } => {
                last_status = Some(format!("model picker error: {error}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "model_catalog_error", "error": error})
                );
            }
            session_launch::DesktopSessionEvent::StdinRequest {
                request_id,
                prompt,
                is_password,
                tool_call_id,
            } => {
                last_status = Some("interactive input requested".to_string());
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "stdin_request",
                        "request_id": request_id,
                        "prompt": prompt,
                        "is_password": is_password,
                        "tool_call_id": tool_call_id,
                    })
                );
            }
            session_launch::DesktopSessionEvent::Done => {
                let response = response.trim().to_string();
                if response.is_empty() {
                    anyhow::bail!(
                        "desktop chat smoke completed without assistant text; session_id={}; last_status={}",
                        session_id.as_deref().unwrap_or("unknown"),
                        last_status.as_deref().unwrap_or("unknown")
                    );
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "ok",
                        "session_id": session_id,
                        "response_chars": response.chars().count(),
                        "response_preview": response.chars().take(240).collect::<String>(),
                    })
                );
                return Ok(());
            }
            session_launch::DesktopSessionEvent::Error(error) => {
                anyhow::bail!(
                    "desktop chat smoke failed; session_id={}; error={}",
                    session_id.as_deref().unwrap_or("unknown"),
                    error
                );
            }
        }
    }

    anyhow::bail!(
        "desktop chat smoke timed out after {:?}; session_id={}; response_chars={}; last_status={}",
        HEADLESS_CHAT_SMOKE_TIMEOUT,
        session_id.as_deref().unwrap_or("unknown"),
        response.chars().count(),
        last_status.as_deref().unwrap_or("unknown")
    )
}

fn run_scroll_render_benchmark(frames: usize) -> Result<()> {
    let frames = frames.max(1);
    let size = PhysicalSize::new(1200, 760);
    let mut app = desktop_scroll_benchmark_app();
    if let Some(metrics) = single_session_body_scroll_metrics(&app, size, 0) {
        app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }

    let mut setup_font_system = benchmark_font_system();
    let setup_started = Instant::now();
    let setup_key = single_session_text_key_for_tick_with_scroll(&app, size, 0, 0.0);
    let setup_buffers =
        single_session_text_buffers_from_key(&setup_key, size, &mut setup_font_system);
    let setup_areas =
        single_session_text_areas_for_app_with_scroll(&app, &setup_buffers, size, 0, 0.0);
    let setup_vertices =
        build_single_session_vertices_with_scroll_and_reveal(&app, size, 0.0, 0, 0.0, 1.0);
    let setup_elapsed = setup_started.elapsed();
    let setup_checksum =
        setup_key.body.len() ^ setup_buffers.len() ^ setup_areas.len() ^ setup_vertices.len();

    let mut legacy_font_system = benchmark_font_system();
    let (legacy_smooth_text_ms, legacy_smooth_text_checksum) = benchmark_phase(frames, |frame| {
        let tick = frame as u64;
        let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);
        let key =
            single_session_text_key_for_tick_with_scroll(&app, size, tick, smooth_scroll_lines);
        let buffers = single_session_text_buffers_from_key(&key, size, &mut legacy_font_system);
        let areas = single_session_text_areas_for_app_with_scroll(
            &app,
            &buffers,
            size,
            tick,
            smooth_scroll_lines,
        );
        let vertices = build_single_session_vertices_with_scroll_and_reveal(
            &app,
            size,
            0.0,
            tick,
            smooth_scroll_lines,
            1.0,
        );
        key.body.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut optimized_font_system = benchmark_font_system();
    let optimized_key = single_session_text_key_for_tick_with_scroll(&app, size, 0, 0.0);
    let optimized_buffers =
        single_session_text_buffers_from_key(&optimized_key, size, &mut optimized_font_system);
    let optimized_areas =
        single_session_text_areas_for_app_with_scroll(&app, &optimized_buffers, size, 0, 0.0);
    let optimized_body_lines = single_session_rendered_body_lines_for_tick(&app, size, 0);
    let (optimized_smooth_geometry_ms, optimized_smooth_geometry_checksum) =
        benchmark_phase(frames, |frame| {
            let tick = frame as u64;
            let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);
            let vertices = build_single_session_vertices_with_cached_body(
                &app,
                size,
                0.0,
                tick,
                smooth_scroll_lines,
                1.0,
                &optimized_body_lines,
            );
            optimized_key.body.len()
                ^ optimized_buffers.len()
                ^ optimized_areas.len()
                ^ vertices.len()
        });

    let mut whole_line_app = app.clone();
    let mut whole_line_font_system = benchmark_font_system();
    let whole_line_body_lines =
        single_session_rendered_body_lines_for_tick(&whole_line_app, size, 0);
    let (whole_line_text_ms, whole_line_text_checksum) = benchmark_phase(frames, |frame| {
        whole_line_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
        let tick = frame as u64;
        let key = single_session_text_key_for_tick_with_rendered_body(
            &whole_line_app,
            size,
            tick,
            0.0,
            &whole_line_body_lines,
        );
        let buffers = single_session_text_buffers_from_key(&key, size, &mut whole_line_font_system);
        let areas = single_session_text_areas_for_app_with_cached_body(
            &whole_line_app,
            &buffers,
            size,
            0.0,
            &whole_line_body_lines,
        );
        let vertices = build_single_session_vertices_with_cached_body(
            &whole_line_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &whole_line_body_lines,
        );
        key.body.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut visible_whole_line_app = app.clone();
    let mut visible_whole_line_font_system = benchmark_font_system();
    let visible_whole_line_body_lines =
        single_session_rendered_body_lines_for_tick(&visible_whole_line_app, size, 0);
    let visible_whole_line_key = single_session_text_key_for_tick_with_rendered_body(
        &visible_whole_line_app,
        size,
        0,
        0.0,
        &visible_whole_line_body_lines,
    );
    let mut visible_whole_line_buffers = single_session_text_buffers_from_key(
        &visible_whole_line_key,
        size,
        &mut visible_whole_line_font_system,
    );
    let mut visible_whole_line_start = single_session_body_viewport_from_lines(
        &visible_whole_line_app,
        size,
        0.0,
        &visible_whole_line_body_lines,
    )
    .start_line;
    let initial_visible_viewport = single_session_body_viewport_from_lines(
        &visible_whole_line_app,
        size,
        0.0,
        &visible_whole_line_body_lines,
    );
    let (mut visible_window_start, mut visible_window_end) =
        single_session_body_text_window_bounds(&initial_visible_viewport);
    if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut visible_whole_line_font_system,
            &visible_whole_line_body_lines[visible_window_start..visible_window_end],
            size,
            visible_whole_line_app.text_scale(),
        );
        body_buffer.set_scroll(
            initial_visible_viewport
                .start_line
                .saturating_sub(visible_window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    let mut visible_viewport_ms = 0.0;
    let mut visible_window_ms = 0.0;
    let mut visible_scroll_ms = 0.0;
    let mut visible_glyph_ms = 0.0;
    let mut visible_areas_ms = 0.0;
    let mut visible_vertices_ms = 0.0;
    let (visible_whole_line_text_ms, visible_whole_line_text_checksum) =
        benchmark_phase(frames, |frame| {
            visible_whole_line_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
            let tick = frame as u64;
            let phase_started = Instant::now();
            let viewport = single_session_body_viewport_from_lines(
                &visible_whole_line_app,
                size,
                0.0,
                &visible_whole_line_body_lines,
            );
            visible_viewport_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            if !single_session_body_text_window_contains(
                visible_window_start,
                visible_window_end,
                &viewport,
            ) {
                (visible_window_start, visible_window_end) =
                    single_session_body_text_window_bounds(&viewport);
                if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
                    *body_buffer = single_session_body_text_buffer_from_lines(
                        &mut visible_whole_line_font_system,
                        &visible_whole_line_body_lines[visible_window_start..visible_window_end],
                        size,
                        visible_whole_line_app.text_scale(),
                    );
                }
                visible_whole_line_start = usize::MAX;
            }
            visible_window_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            if viewport.start_line != visible_whole_line_start {
                if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
                    body_buffer.set_scroll(
                        viewport
                            .start_line
                            .saturating_sub(visible_window_start)
                            .min(i32::MAX as usize) as i32,
                    );
                }
                visible_whole_line_start = viewport.start_line;
            }
            visible_scroll_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let glyph_checksum = visible_whole_line_buffers
                .get(1)
                .map(|body_buffer| {
                    body_buffer
                        .layout_runs()
                        .map(|run| run.glyphs.len())
                        .sum::<usize>()
                })
                .unwrap_or_default();
            visible_glyph_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let areas = single_session_text_areas_for_app_with_cached_body_viewport(
                &visible_whole_line_app,
                &visible_whole_line_buffers,
                size,
                0.0,
                viewport,
            );
            visible_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let vertices = build_single_session_vertices_with_cached_body(
                &visible_whole_line_app,
                size,
                0.0,
                tick,
                0.0,
                1.0,
                &visible_whole_line_body_lines,
            );
            visible_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            visible_whole_line_key.body.len()
                ^ visible_whole_line_buffers.len()
                ^ areas.len()
                ^ vertices.len()
                ^ glyph_checksum
        });

    let mut typing_app = app.clone();
    typing_app.scroll_body_to_bottom();
    typing_app.draft.clear();
    typing_app.draft_cursor = 0;
    let typing_body_lines = single_session_rendered_body_lines_for_tick(&typing_app, size, 0);
    let mut typing_font_system = benchmark_font_system();
    let typing_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &typing_app,
        size,
        0,
        0.0,
        &typing_body_lines,
    );
    let mut typing_buffers =
        single_session_text_buffers_from_key(&typing_initial_key, size, &mut typing_font_system);
    let typing_initial_viewport =
        single_session_body_viewport_from_lines(&typing_app, size, 0.0, &typing_body_lines);
    let (typing_window_start, typing_window_end) =
        single_session_body_text_window_bounds(&typing_initial_viewport);
    if let Some(body_buffer) = typing_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut typing_font_system,
            &typing_body_lines[typing_window_start..typing_window_end],
            size,
            typing_app.text_scale(),
        );
    }
    let mut typing_previous_key = Some(typing_initial_key);
    let mut typing_text_cache_ms = 0.0;
    let mut typing_areas_ms = 0.0;
    let mut typing_vertices_ms = 0.0;
    let (typing_redraw_ms, typing_redraw_checksum) = benchmark_phase(frames, |frame| {
        let ch = benchmark_typing_char(frame);
        typing_app.draft.push(ch);
        typing_app.draft_cursor = typing_app.draft.len();
        let tick = frame as u64;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &typing_app,
            size,
            tick,
            0.0,
            &typing_body_lines,
        );
        let draft_len = key.draft.len();
        let previous_key = typing_previous_key.take();
        let old_buffers = std::mem::take(&mut typing_buffers);
        typing_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            true,
            size,
            &mut typing_font_system,
        );
        typing_previous_key = Some(key);
        typing_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body(
            &typing_app,
            &typing_buffers,
            size,
            0.0,
            &typing_body_lines,
        );
        typing_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &typing_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &typing_body_lines,
        );
        typing_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        draft_len ^ typing_buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut fresh_typing_app = SingleSessionApp::new(None);
    fresh_typing_app.draft.clear();
    fresh_typing_app.draft_cursor = 0;
    let mut fresh_typing_font_system = benchmark_font_system();
    let mut fresh_typing_text_cache_ms = 0.0;
    let mut fresh_typing_areas_ms = 0.0;
    let mut fresh_typing_vertices_ms = 0.0;
    let (fresh_typing_ms, fresh_typing_checksum) = benchmark_phase(frames, |frame| {
        let ch = benchmark_typing_char(frame);
        fresh_typing_app.draft.push(ch);
        fresh_typing_app.draft_cursor = fresh_typing_app.draft.len();
        let tick = frame as u64;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_scroll(&fresh_typing_app, size, tick, 0.0);
        let buffers =
            single_session_text_buffers_from_key(&key, size, &mut fresh_typing_font_system);
        fresh_typing_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_scroll(
            &fresh_typing_app,
            &buffers,
            size,
            tick,
            0.0,
        );
        fresh_typing_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_scroll_and_reveal(
            &fresh_typing_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
        );
        fresh_typing_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        key.draft.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut streaming_app = app.clone();
    streaming_app.scroll_body_to_bottom();
    streaming_app.streaming_response.clear();
    let mut streaming_font_system = benchmark_font_system();
    let mut streaming_body_lines =
        single_session_rendered_body_lines_for_tick(&streaming_app, size, 0);
    let mut streaming_base_key = None;
    let mut streaming_base_len = 0usize;
    let streaming_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &streaming_app,
        size,
        0,
        0.0,
        &streaming_body_lines,
    );
    let mut streaming_buffers = single_session_text_buffers_from_key(
        &streaming_initial_key,
        size,
        &mut streaming_font_system,
    );
    let streaming_initial_viewport =
        single_session_body_viewport_from_lines(&streaming_app, size, 0.0, &streaming_body_lines);
    let (mut streaming_window_start, mut streaming_window_end) =
        single_session_body_text_window_bounds(&streaming_initial_viewport);
    if let Some(body_buffer) = streaming_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut streaming_font_system,
            &streaming_body_lines[streaming_window_start..streaming_window_end],
            size,
            streaming_app.text_scale(),
        );
        body_buffer.set_scroll(
            streaming_initial_viewport
                .start_line
                .saturating_sub(streaming_window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    let mut streaming_previous_key = Some(streaming_initial_key);
    let mut streaming_tail_text_key = None;
    let mut streaming_tail_text_start_line = None;
    let mut streaming_tail_text_buffer = None;
    let mut streaming_body_ms = 0.0;
    let mut streaming_text_cache_ms = 0.0;
    let mut streaming_areas_ms = 0.0;
    let mut streaming_vertices_ms = 0.0;
    let (streaming_delta_ms, streaming_delta_checksum) = benchmark_phase(frames, |frame| {
        streaming_app
            .streaming_response
            .push(benchmark_typing_char(frame));
        if frame % 17 == 0 {
            streaming_app.streaming_response.push('\n');
        }
        let tick = frame as u64;

        let phase_started = Instant::now();
        if !streaming_app.streaming_response.is_empty() {
            let base_key = streaming_app.rendered_body_static_cache_key((size.width, size.height));
            if streaming_base_key != Some(base_key) {
                streaming_body_lines = single_session_rendered_static_body_lines_for_streaming(
                    &streaming_app,
                    size,
                    tick,
                )
                .unwrap_or_else(|| {
                    single_session_rendered_body_lines_for_tick(&streaming_app, size, tick)
                });
                streaming_base_len = streaming_body_lines.len();
                streaming_base_key = Some(base_key);
            } else {
                streaming_body_lines.truncate(streaming_base_len);
            }
            append_single_session_streaming_response_rendered_body_lines(
                &streaming_app,
                size,
                &mut streaming_body_lines,
            );
        } else {
            streaming_body_lines =
                single_session_rendered_body_lines_for_tick(&streaming_app, size, tick);
            streaming_base_key = None;
            streaming_base_len = 0;
        }
        streaming_body_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &streaming_app,
            size,
            tick,
            0.0,
            &streaming_body_lines,
        );
        let viewport = single_session_body_viewport_from_lines(
            &streaming_app,
            size,
            0.0,
            &streaming_body_lines,
        );
        let visible_static_start = viewport.start_line.min(streaming_base_len);
        let visible_static_end = viewport
            .start_line
            .saturating_add(viewport.lines.len())
            .min(streaming_base_len);
        let body_window_contains = visible_static_start >= visible_static_end
            || (streaming_window_start <= visible_static_start
                && visible_static_end <= streaming_window_end);
        let previous_key = streaming_previous_key.take();
        let mut old_buffers = std::mem::take(&mut streaming_buffers);
        if old_buffers.len() > 1 && !body_window_contains {
            streaming_window_start =
                visible_static_start.saturating_sub(SINGLE_SESSION_BODY_TEXT_WINDOW_BEFORE_LINES);
            streaming_window_end = visible_static_end
                .saturating_add(SINGLE_SESSION_BODY_TEXT_WINDOW_AFTER_LINES)
                .min(streaming_base_len);
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut streaming_font_system,
                &streaming_body_lines[streaming_window_start..streaming_window_end],
                size,
                streaming_app.text_scale(),
            );
        }
        let can_reuse_body_buffer = old_buffers.len() > 1;
        streaming_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut streaming_font_system,
        );
        if let Some(body_buffer) = streaming_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(streaming_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        let streaming_start_line =
            streaming_base_len.saturating_add(usize::from(!streaming_app.messages.is_empty()));
        let visible_start = viewport.start_line;
        let visible_end = viewport.start_line.saturating_add(viewport.lines.len());
        let streaming_visible_start = streaming_start_line.max(visible_start);
        let streaming_visible_end = streaming_body_lines.len().min(visible_end);
        if streaming_visible_start < streaming_visible_end {
            let mut hasher = DefaultHasher::new();
            (size.width, size.height).hash(&mut hasher);
            streaming_app.text_scale().to_bits().hash(&mut hasher);
            streaming_visible_start.hash(&mut hasher);
            streaming_visible_end.hash(&mut hasher);
            streaming_body_lines[streaming_visible_start..streaming_visible_end].hash(&mut hasher);
            let tail_key = hasher.finish();
            if streaming_tail_text_key != Some(tail_key) {
                streaming_tail_text_buffer = Some(single_session_body_text_buffer_from_lines(
                    &mut streaming_font_system,
                    &streaming_body_lines[streaming_visible_start..streaming_visible_end],
                    size,
                    streaming_app.text_scale(),
                ));
                streaming_tail_text_key = Some(tail_key);
                streaming_tail_text_start_line = Some(streaming_visible_start);
            }
        } else {
            streaming_tail_text_key = None;
            streaming_tail_text_start_line = None;
            streaming_tail_text_buffer = None;
        }
        streaming_previous_key = Some(key);
        streaming_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let mut areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &streaming_app,
            &streaming_buffers,
            size,
            0.0,
            viewport.clone(),
        );
        if let (Some(buffer), Some(start_line)) = (
            streaming_tail_text_buffer.as_ref(),
            streaming_tail_text_start_line,
        ) {
            areas.push(single_session_streaming_text_area_for_cached_body_viewport(
                &streaming_app,
                buffer,
                size,
                viewport,
                start_line,
            ));
        }
        streaming_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &streaming_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &streaming_body_lines,
        );
        streaming_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        streaming_body_lines.len()
            ^ streaming_buffers.len()
            ^ streaming_tail_text_buffer.is_some() as usize
            ^ areas.len()
            ^ vertices.len()
    });

    let mut event_batch_app = DesktopApp::SingleSession(SingleSessionApp::new(None));
    let (event_batch_ms, event_batch_checksum) = benchmark_phase(frames, |frame| {
        let events = (0..128)
            .map(|offset| {
                session_launch::DesktopSessionEvent::TextDelta(
                    benchmark_typing_char(frame + offset).to_string(),
                )
            })
            .collect::<Vec<_>>();
        let original_events = events.len();
        let coalesced = coalesce_desktop_session_events(events);
        let coalesced_events = coalesced.len();
        apply_desktop_session_event_batch(&mut event_batch_app, coalesced);
        original_events ^ coalesced_events
    });

    let mut hero_app = desktop_scroll_benchmark_app_with_turns(24);
    let hero_body_lines = single_session_rendered_body_lines_for_tick(&hero_app, size, 0);
    let hero_boundary_scroll =
        benchmark_hero_boundary_scroll_lines(&hero_app, size, &hero_body_lines);
    hero_app.body_scroll_lines = hero_boundary_scroll;
    let mut hero_font_system = benchmark_font_system();
    let hero_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &hero_app,
        size,
        0,
        0.0,
        &hero_body_lines,
    );
    let mut hero_buffers =
        single_session_text_buffers_from_key(&hero_initial_key, size, &mut hero_font_system);
    let hero_initial_viewport =
        single_session_body_viewport_from_lines(&hero_app, size, 0.0, &hero_body_lines);
    let (mut hero_window_start, mut hero_window_end) =
        single_session_body_text_window_bounds(&hero_initial_viewport);
    if let Some(body_buffer) = hero_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut hero_font_system,
            &hero_body_lines[hero_window_start..hero_window_end],
            size,
            hero_app.text_scale(),
        );
    }
    let mut hero_previous_key = Some(hero_initial_key);
    let mut hero_viewport_key_ms = 0.0;
    let mut hero_window_rebuild_ms = 0.0;
    let mut hero_buffer_reuse_ms = 0.0;
    let mut hero_body_buffer_rebuilds = 0usize;
    let mut hero_text_cache_ms = 0.0;
    let mut hero_areas_ms = 0.0;
    let mut hero_vertices_ms = 0.0;
    let (hero_boundary_scroll_ms, hero_boundary_checksum) = benchmark_phase(frames, |frame| {
        let tick = frame as u64;
        let scroll_offset = (frame % 24) as f32 - 12.0;
        hero_app.body_scroll_lines = (hero_boundary_scroll + scroll_offset).max(0.0);
        let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);

        let phase_started = Instant::now();
        let subphase_started = Instant::now();
        let viewport = single_session_body_viewport_from_lines(
            &hero_app,
            size,
            smooth_scroll_lines,
            &hero_body_lines,
        );
        let key = single_session_text_key_for_tick_with_rendered_body(
            &hero_app,
            size,
            tick,
            smooth_scroll_lines,
            &hero_body_lines,
        );
        hero_viewport_key_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;

        let subphase_started = Instant::now();
        let previous_key = hero_previous_key.take();
        let mut old_buffers = std::mem::take(&mut hero_buffers);
        if old_buffers.len() > 1
            && !single_session_body_text_window_contains(
                hero_window_start,
                hero_window_end,
                &viewport,
            )
        {
            hero_body_buffer_rebuilds += 1;
            (hero_window_start, hero_window_end) =
                single_session_body_text_window_bounds(&viewport);
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut hero_font_system,
                &hero_body_lines[hero_window_start..hero_window_end],
                size,
                hero_app.text_scale(),
            );
        }
        hero_window_rebuild_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;

        let subphase_started = Instant::now();
        let can_reuse_body_buffer = old_buffers.len() > 1;
        hero_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut hero_font_system,
        );
        if let Some(body_buffer) = hero_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(hero_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        let hero_visible = key.fresh_welcome_visible;
        hero_previous_key = Some(key);
        hero_buffer_reuse_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;
        hero_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &hero_app,
            &hero_buffers,
            size,
            smooth_scroll_lines,
            viewport,
        );
        hero_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &hero_app,
            size,
            0.0,
            tick,
            smooth_scroll_lines,
            1.0,
            &hero_body_lines,
        );
        hero_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        hero_buffers.len() ^ areas.len() ^ vertices.len() ^ usize::from(hero_visible)
    });

    let mut action_input_app = DesktopApp::SingleSession(SingleSessionApp::new(None));
    let (action_input_ms, action_input_checksum) = benchmark_phase(frames, |frame| {
        let events = (0..128)
            .map(|offset| session_launch::DesktopSessionEvent::ToolInput {
                delta: benchmark_typing_char(frame + offset).to_string(),
            })
            .collect::<Vec<_>>();
        let coalesced = coalesce_desktop_session_events(events);
        let visible_changed = apply_desktop_session_event_batch(&mut action_input_app, coalesced);
        usize::from(visible_changed)
    });

    let mut action_app = desktop_scroll_benchmark_app_with_turns(64);
    action_app.scroll_body_to_bottom();
    action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
        name: "bash".to_string(),
    });
    let mut action_font_system = benchmark_font_system();
    let mut action_body_key = action_app.rendered_body_cache_key((size.width, size.height));
    let mut action_body_lines = single_session_rendered_body_lines_for_tick(&action_app, size, 0);
    let action_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &action_app,
        size,
        0,
        0.0,
        &action_body_lines,
    );
    let mut action_buffers =
        single_session_text_buffers_from_key(&action_initial_key, size, &mut action_font_system);
    let action_initial_viewport =
        single_session_body_viewport_from_lines(&action_app, size, 0.0, &action_body_lines);
    let (mut action_window_start, mut action_window_end) =
        single_session_body_text_window_bounds(&action_initial_viewport);
    if let Some(body_buffer) = action_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut action_font_system,
            &action_body_lines[action_window_start..action_window_end],
            size,
            action_app.text_scale(),
        );
    }
    let mut action_previous_key = Some(action_initial_key);
    let mut action_apply_ms = 0.0;
    let mut action_body_ms = 0.0;
    let mut action_text_cache_ms = 0.0;
    let mut action_areas_ms = 0.0;
    let mut action_vertices_ms = 0.0;
    let (action_visible_ms, action_visible_checksum) = benchmark_phase(frames, |frame| {
        let phase_started = Instant::now();
        action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolInput {
            delta: format!(" chunk-{frame}"),
        });
        action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
            name: "bash".to_string(),
        });
        action_apply_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
        let tick = frame as u64;

        let phase_started = Instant::now();
        let next_body_key = action_app.rendered_body_cache_key((size.width, size.height));
        let action_body_changed = action_body_key != next_body_key;
        if action_body_changed {
            action_body_lines =
                single_session_rendered_body_lines_for_tick(&action_app, size, tick);
            action_body_key = next_body_key;
        }
        action_body_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &action_app,
            size,
            tick,
            0.0,
            &action_body_lines,
        );
        let viewport =
            single_session_body_viewport_from_lines(&action_app, size, 0.0, &action_body_lines);
        let previous_key = action_previous_key.take();
        let mut old_buffers = std::mem::take(&mut action_buffers);
        if old_buffers.len() > 1
            && (action_body_changed
                || !single_session_body_text_window_contains(
                    action_window_start,
                    action_window_end,
                    &viewport,
                ))
        {
            (action_window_start, action_window_end) =
                single_session_body_text_window_bounds(&viewport);
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut action_font_system,
                &action_body_lines[action_window_start..action_window_end],
                size,
                action_app.text_scale(),
            );
        }
        let can_reuse_body_buffer = old_buffers.len() > 1;
        action_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut action_font_system,
        );
        if let Some(body_buffer) = action_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(action_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        action_previous_key = Some(key);
        action_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &action_app,
            &action_buffers,
            size,
            0.0,
            viewport,
        );
        action_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &action_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &action_body_lines,
        );
        action_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        action_body_lines.len() ^ action_buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut large_app = desktop_large_transcript_benchmark_app();
    let large_body_started = Instant::now();
    let large_body_lines = single_session_rendered_body_lines_for_tick(&large_app, size, 0);
    if let Some(metrics) =
        single_session_body_scroll_metrics_for_total_lines(&large_app, size, large_body_lines.len())
    {
        large_app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }
    let large_body_elapsed = large_body_started.elapsed();
    let (large_scroll_ms, large_scroll_checksum) = benchmark_phase(frames, |frame| {
        large_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
        let viewport =
            single_session_body_viewport_from_lines(&large_app, size, 0.0, &large_body_lines);
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &large_app,
            &visible_whole_line_buffers,
            size,
            0.0,
            viewport,
        );
        let vertices = build_single_session_vertices_with_cached_body(
            &large_app,
            size,
            0.0,
            frame as u64,
            0.0,
            1.0,
            &large_body_lines,
        );
        large_body_lines.len() ^ areas.len() ^ vertices.len()
    });

    let target_budget_ms = duration_ms(DESKTOP_120FPS_FRAME_BUDGET);
    let critical_phase_means_ms = [
        visible_whole_line_text_ms / frames as f64,
        typing_redraw_ms / frames as f64,
        fresh_typing_ms / frames as f64,
        streaming_delta_ms / frames as f64,
        event_batch_ms / frames as f64,
        hero_boundary_scroll_ms / frames as f64,
        action_input_ms / frames as f64,
        action_visible_ms / frames as f64,
        large_scroll_ms / frames as f64,
    ];
    let metrics = single_session_body_scroll_metrics(&app, size, 0);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frames": frames,
            "target_frame_budget_ms": target_budget_ms,
            "passes_120fps_scroll_cpu_budget": (visible_whole_line_text_ms / frames as f64)
                <= target_budget_ms,
            "passes_120fps_interaction_cpu_budget": critical_phase_means_ms
                .iter()
                .all(|mean_ms| *mean_ms <= target_budget_ms),
            "event_delivery": {
                "previous_background_poll_interval_ms": duration_ms(BACKGROUND_POLL_INTERVAL),
                "backend_events_wake_event_loop": true,
                "coalesces_consecutive_text_delta_events": true,
            },
            "size": { "width": size.width, "height": size.height },
            "messages": app.messages.len(),
            "scroll_metrics": metrics.map(|metrics| serde_json::json!({
                "total_lines": metrics.total_lines,
                "visible_lines": metrics.visible_lines,
                "max_scroll_lines": metrics.max_scroll_lines,
                "start_scroll_lines": app.body_scroll_lines,
            })),
            "setup": benchmark_phase_json(
                "setup_text_and_geometry",
                setup_elapsed.as_secs_f64() * 1000.0,
                1,
                setup_checksum,
            ),
            "phases": [
                benchmark_phase_json(
                    "legacy_smooth_scroll_text_relayout",
                    legacy_smooth_text_ms,
                    frames,
                    legacy_smooth_text_checksum,
                ),
                benchmark_phase_json(
                    "optimized_smooth_scroll_geometry_only",
                    optimized_smooth_geometry_ms,
                    frames,
                    optimized_smooth_geometry_checksum,
                ),
                benchmark_phase_json(
                    "legacy_whole_line_scroll_text_relayout",
                    whole_line_text_ms,
                    frames,
                    whole_line_text_checksum,
                ),
                benchmark_phase_json(
                    "optimized_whole_line_scroll_visible_body_only",
                    visible_whole_line_text_ms,
                    frames,
                    visible_whole_line_text_checksum,
                ),
                benchmark_phase_json(
                    "typing_redraw_reuse_body_cache",
                    typing_redraw_ms,
                    frames,
                    typing_redraw_checksum,
                ),
                benchmark_phase_json(
                    "fresh_welcome_typing_redraw",
                    fresh_typing_ms,
                    frames,
                    fresh_typing_checksum,
                ),
                benchmark_phase_json(
                    "streaming_delta_redraw",
                    streaming_delta_ms,
                    frames,
                    streaming_delta_checksum,
                ),
                benchmark_phase_json(
                    "background_event_batch_coalesce_apply",
                    event_batch_ms,
                    frames,
                    event_batch_checksum,
                ),
                benchmark_phase_json(
                    "hero_boundary_scroll_redraw",
                    hero_boundary_scroll_ms,
                    frames,
                    hero_boundary_checksum,
                ),
                benchmark_phase_json(
                    "action_tool_input_batch_no_redraw",
                    action_input_ms,
                    frames,
                    action_input_checksum,
                ),
                benchmark_phase_json(
                    "action_tool_visible_redraw",
                    action_visible_ms,
                    frames,
                    action_visible_checksum,
                ),
                benchmark_phase_json(
                    "large_transcript_scroll_visible_body_only",
                    large_scroll_ms,
                    frames,
                    large_scroll_checksum,
                ),
            ],
            "visible_whole_line_subphases": [
                benchmark_phase_json("viewport", visible_viewport_ms, frames, 0),
                benchmark_phase_json("window", visible_window_ms, frames, 0),
                benchmark_phase_json("set_scroll", visible_scroll_ms, frames, 0),
                benchmark_phase_json("glyph_runs", visible_glyph_ms, frames, 0),
                benchmark_phase_json("areas", visible_areas_ms, frames, 0),
                benchmark_phase_json("vertices", visible_vertices_ms, frames, 0),
            ],
            "typing_redraw_subphases": [
                benchmark_phase_json("text_cache", typing_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", typing_areas_ms, frames, 0),
                benchmark_phase_json("vertices", typing_vertices_ms, frames, 0),
            ],
            "fresh_welcome_typing_subphases": [
                benchmark_phase_json("text_cache", fresh_typing_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", fresh_typing_areas_ms, frames, 0),
                benchmark_phase_json("vertices", fresh_typing_vertices_ms, frames, 0),
            ],
            "streaming_delta_subphases": [
                benchmark_phase_json("body_wrap", streaming_body_ms, frames, 0),
                benchmark_phase_json("text_cache", streaming_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", streaming_areas_ms, frames, 0),
                benchmark_phase_json("vertices", streaming_vertices_ms, frames, 0),
            ],
            "hero_boundary": {
                "start_scroll_lines": hero_boundary_scroll,
                "body_buffer_rebuilds": hero_body_buffer_rebuilds,
                "subphases": [
                    benchmark_phase_json("text_cache", hero_text_cache_ms, frames, 0),
                    benchmark_phase_json("viewport_and_key", hero_viewport_key_ms, frames, 0),
                    benchmark_phase_json("body_window_rebuild", hero_window_rebuild_ms, frames, hero_body_buffer_rebuilds),
                    benchmark_phase_json("reuse_text_buffers", hero_buffer_reuse_ms, frames, 0),
                    benchmark_phase_json("areas", hero_areas_ms, frames, 0),
                    benchmark_phase_json("vertices", hero_vertices_ms, frames, 0),
                ],
            },
            "action_tool_visible_subphases": [
                benchmark_phase_json("event_apply_body_mutation", action_apply_ms, frames, 0),
                benchmark_phase_json("body_wrap", action_body_ms, frames, 0),
                benchmark_phase_json("text_cache", action_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", action_areas_ms, frames, 0),
                benchmark_phase_json("vertices", action_vertices_ms, frames, 0),
            ],
            "large_transcript_setup": benchmark_phase_json(
                "large_transcript_initial_body_wrap",
                large_body_elapsed.as_secs_f64() * 1000.0,
                1,
                large_body_lines.len(),
            ),
        }))?
    );
    Ok(())
}

fn benchmark_phase(mut frames: usize, mut run_frame: impl FnMut(usize) -> usize) -> (f64, usize) {
    frames = frames.max(1);
    let started = Instant::now();
    let mut checksum = 0usize;
    for frame in 0..frames {
        checksum ^= std::hint::black_box(run_frame(frame));
    }
    (started.elapsed().as_secs_f64() * 1000.0, checksum)
}

fn benchmark_phase_json(
    name: &str,
    total_ms: f64,
    frames: usize,
    checksum: usize,
) -> serde_json::Value {
    let frames = frames.max(1);
    serde_json::json!({
        "name": name,
        "total_ms": total_ms,
        "mean_ms_per_frame": total_ms / frames as f64,
        "mean_us_per_frame": total_ms * 1000.0 / frames as f64,
        "checksum": checksum,
    })
}

fn benchmark_smooth_scroll_lines(frame: usize) -> f32 {
    ((frame % 16) as f32 / 16.0) - 0.5
}

fn benchmark_typing_char(frame: usize) -> char {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz     .,;";
    CHARS[frame % CHARS.len()] as char
}

fn benchmark_hero_boundary_scroll_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    body_lines: &[SingleSessionStyledLine],
) -> f32 {
    let Some(metrics) =
        single_session_body_scroll_metrics_for_total_lines(app, size, body_lines.len())
    else {
        return 0.0;
    };
    let mut probe = app.clone();
    for scroll in 0..=metrics.max_scroll_lines {
        probe.body_scroll_lines = scroll as f32;
        let key =
            single_session_text_key_for_tick_with_rendered_body(&probe, size, 0, 0.0, body_lines);
        if key.fresh_welcome_visible {
            return scroll.saturating_sub(6) as f32;
        }
    }
    metrics.max_scroll_lines.saturating_sub(12) as f32
}

fn benchmark_font_system() -> FontSystem {
    let mut font_system = FontSystem::new();
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/Kalam-Regular.ttf").to_vec());
    font_system
}

fn desktop_scroll_benchmark_app() -> SingleSessionApp {
    desktop_scroll_benchmark_app_with_turns(320)
}

fn desktop_large_transcript_benchmark_app() -> SingleSessionApp {
    desktop_scroll_benchmark_app_with_turns(2_000)
}

fn desktop_scroll_benchmark_app_with_turns(turns: usize) -> SingleSessionApp {
    let mut app = SingleSessionApp::new(None);
    app.draft = "summarize the latest changes and suggest the next optimization".to_string();
    app.draft_cursor = app.draft.len();
    for turn in 0..turns {
        app.messages.push(SingleSessionMessage::user(format!(
            "Prompt {turn}: inspect this desktop render path and explain where scroll jank may come from."
        )));
        app.messages.push(SingleSessionMessage::assistant(format!(
            "Assistant response {turn}: The render path includes markdown wrapping, transcript card geometry, scrollbar geometry, text buffer preparation, and GPU primitive upload. This paragraph is intentionally long enough to wrap across several desktop body lines so the benchmark exercises visible-line virtualization and wrapping behavior.\n\n- Check cached text keys.\n- Check smooth scroll fractional offsets.\n- Check whether geometry can update without reshaping text.\n\n```rust\nfn sample_{turn}() {{ println!(\"benchmark\"); }}\n```"
        )));
    }
    app.status = Some("benchmark fixture".to_string());
    app
}

fn load_desktop_preferences() -> Option<workspace::DesktopPreferences> {
    match desktop_prefs::load_preferences() {
        Ok(preferences) => preferences,
        Err(error) => {
            eprintln!("jcode-desktop: failed to load desktop preferences: {error:#}");
            None
        }
    }
}

fn save_desktop_preferences(workspace: &Workspace) {
    if let Err(error) = desktop_prefs::save_preferences(&workspace.preferences()) {
        eprintln!("jcode-desktop: failed to save desktop preferences: {error:#}");
    }
}

fn load_primary_session_card() -> Option<workspace::SessionCard> {
    load_session_cards_for_desktop().into_iter().next()
}

fn fresh_single_session_app() -> DesktopApp {
    DesktopApp::SingleSession(SingleSessionApp::new(None))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopMode {
    SingleSession,
    WorkspacePrototype,
}

fn desktop_mode_from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> DesktopMode {
    if args.into_iter().any(|arg| arg == "--workspace") {
        DesktopMode::WorkspacePrototype
    } else {
        DesktopMode::SingleSession
    }
}

fn attach_single_session_by_id(app: &mut DesktopApp, session_id: &str) {
    let Some(card) = load_session_cards_for_desktop()
        .into_iter()
        .find(|card| card.session_id == session_id)
    else {
        return;
    };

    if let DesktopApp::SingleSession(single_session) = app {
        single_session.replace_session(Some(card));
    }
}

struct DesktopHotReloader {
    relaunch: Option<DesktopRelaunch>,
    initial_modified: Option<std::time::SystemTime>,
    last_checked: Instant,
}

impl DesktopHotReloader {
    const CHECK_INTERVAL: Duration = Duration::from_millis(750);

    fn new() -> Self {
        let relaunch = DesktopRelaunch::from_current_process();
        let initial_modified = relaunch
            .as_ref()
            .and_then(|relaunch| binary_modified_time(&relaunch.binary));
        Self {
            relaunch,
            initial_modified,
            last_checked: Instant::now(),
        }
    }

    fn poll(&mut self) -> Option<DesktopRelaunch> {
        if self.last_checked.elapsed() < Self::CHECK_INTERVAL {
            return None;
        }
        self.last_checked = Instant::now();

        let relaunch = self.relaunch.as_ref()?;
        let initial_modified = self.initial_modified?;
        let current_modified = binary_modified_time(&relaunch.binary)?;
        if current_modified > initial_modified {
            self.initial_modified = Some(current_modified);
            return Some(relaunch.clone());
        }
        None
    }
}

#[derive(Clone, Debug)]
struct DesktopRelaunch {
    binary: PathBuf,
    args: Vec<OsString>,
}

impl DesktopRelaunch {
    fn from_current_process() -> Option<Self> {
        let mut args = std::env::args_os();
        let argv0 = args.next()?;
        let binary = match resolve_invoked_binary(&argv0) {
            Some(binary) => binary,
            None => match std::env::current_exe() {
                Ok(binary) => binary,
                Err(_) => return None,
            },
        };
        Some(Self {
            binary,
            args: args.collect(),
        })
    }

    fn spawn(&self) -> Result<()> {
        Command::new(&self.binary)
            .args(&self.args)
            .spawn()
            .with_context(|| format!("failed to spawn {}", self.binary.display()))?;
        Ok(())
    }
}

fn binary_modified_time(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    match metadata.modified() {
        Ok(modified) => Some(modified),
        Err(_) => None,
    }
}

fn resolve_invoked_binary(argv0: &OsString) -> Option<PathBuf> {
    let path = PathBuf::from(argv0);
    if path.components().count() > 1 {
        return Some(path);
    }

    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(&path))
        .find(|candidate| candidate.is_file())
}

enum DesktopApp {
    SingleSession(SingleSessionApp),
    Workspace(Workspace),
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopAppDebugSnapshot {
    mode: &'static str,
    title: String,
    live_session_id: Option<String>,
    status: Option<String>,
    is_processing: bool,
    body_text: String,
}

impl DesktopApp {
    fn is_single_session(&self) -> bool {
        matches!(self, Self::SingleSession(_))
    }

    fn is_workspace(&self) -> bool {
        matches!(self, Self::Workspace(_))
    }

    fn has_background_work(&self) -> bool {
        matches!(self, Self::SingleSession(app) if app.has_background_work())
    }

    fn has_frame_animation(&self) -> bool {
        matches!(self, Self::SingleSession(app) if app.has_frame_animation())
    }

    fn status_title(&self) -> String {
        match self {
            Self::SingleSession(app) => app.status_title(),
            Self::Workspace(workspace) => workspace.status_title(),
        }
    }

    fn handle_key(&mut self, key: KeyInput) -> KeyOutcome {
        match self {
            Self::SingleSession(app) => app.handle_key(key),
            Self::Workspace(workspace) => workspace.handle_key(key),
        }
    }

    fn refresh_sessions(&mut self) {
        match self {
            Self::SingleSession(app) => app.replace_session(load_primary_session_card()),
            Self::Workspace(workspace) => {
                workspace.replace_session_cards(load_session_cards_for_desktop())
            }
        }
    }

    fn apply_session_event(&mut self, event: session_launch::DesktopSessionEvent) {
        if let Self::SingleSession(app) = self {
            app.apply_session_event(event);
        }
    }

    fn set_single_session_handle(&mut self, handle: session_launch::DesktopSessionHandle) {
        if let Self::SingleSession(app) = self {
            app.set_session_handle(handle);
        }
    }

    fn apply_single_session_switcher_cards(&mut self, cards: Vec<workspace::SessionCard>) {
        if let Self::SingleSession(app) = self {
            app.apply_session_switcher_cards(cards);
        }
    }

    fn cancel_single_session_generation(&mut self) {
        if let Self::SingleSession(app) = self {
            app.cancel_generation();
        }
    }

    fn attach_clipboard_image(&mut self, media_type: String, base64_data: String) {
        match self {
            Self::SingleSession(app) => app.attach_image(media_type, base64_data),
            Self::Workspace(workspace) => {
                workspace.attach_image(media_type, base64_data);
            }
        }
    }

    fn accepts_clipboard_image_paste(&self) -> bool {
        match self {
            Self::SingleSession(app) => app.accepts_clipboard_image_paste(),
            Self::Workspace(workspace) => workspace.mode == InputMode::Insert,
        }
    }

    fn paste_text(&mut self, text: &str) {
        match self {
            Self::SingleSession(app) => app.paste_text(text),
            Self::Workspace(workspace) => {
                workspace.paste_text(text);
            }
        }
    }

    fn send_single_session_stdin_response(
        &mut self,
        request_id: String,
        input: String,
    ) -> anyhow::Result<()> {
        match self {
            Self::SingleSession(app) => app.send_stdin_response(request_id, input),
            Self::Workspace(_) => {
                anyhow::bail!("stdin responses are only supported in single-session mode")
            }
        }
    }

    fn take_next_queued_single_session_draft(&mut self) -> Option<(String, Vec<(String, String)>)> {
        match self {
            Self::SingleSession(app) => app.take_next_queued_draft(),
            Self::Workspace(_) => None,
        }
    }

    fn begin_single_session_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            if let Some(point) = single_session_body_point_at_position(size, x, y, &lines) {
                app.begin_selection(point);
                return true;
            }
        }
        false
    }

    fn update_single_session_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            if let Some(point) = single_session_body_point_at_position(size, x, y, &lines) {
                app.update_selection(point);
                return true;
            }
        }
        false
    }

    fn begin_single_session_draft_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self
            && let Some((line, column)) = single_session_draft_line_col_at_position(app, size, x, y)
        {
            app.begin_draft_selection(SelectionPoint { line, column });
            return true;
        }
        false
    }

    fn update_single_session_draft_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self
            && let Some((line, column)) = single_session_draft_line_col_at_position(app, size, x, y)
        {
            app.update_draft_selection(SelectionPoint { line, column });
            return true;
        }
        false
    }

    fn selected_single_session_draft_text(&mut self) -> Option<String> {
        if let Self::SingleSession(app) = self {
            return app.selected_draft_text();
        }
        None
    }

    fn selected_single_session_text(&mut self, size: PhysicalSize<u32>) -> Option<String> {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            let selected = app.selected_text_from_lines(&lines);
            app.clear_selection();
            return selected;
        }
        None
    }

    fn scroll_single_session_body(
        &mut self,
        lines: impl Into<f64>,
        size: PhysicalSize<u32>,
        metrics_cache: &mut SingleSessionScrollMetricsCache,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let previous_scroll_lines = app.body_scroll_lines;
            app.scroll_body_lines(lines);
            if let Some(metrics) = metrics_cache.metrics(app, size) {
                app.body_scroll_lines = app.body_scroll_lines.min(metrics.max_scroll_lines as f32);
            } else {
                app.body_scroll_lines = 0.0;
            }
            return (app.body_scroll_lines - previous_scroll_lines).abs()
                >= SCROLL_FRACTIONAL_EPSILON;
        }
        false
    }

    fn single_session_smooth_scroll_lines(
        &self,
        pending_lines: f32,
        size: PhysicalSize<u32>,
        metrics_cache: &mut SingleSessionScrollMetricsCache,
    ) -> f32 {
        let Self::SingleSession(app) = self else {
            return 0.0;
        };
        let Some(metrics) = metrics_cache.metrics(app, size) else {
            return 0.0;
        };
        let base_scroll = app.body_scroll_lines.min(metrics.max_scroll_lines as f32);
        (base_scroll + pending_lines).clamp(0.0, metrics.max_scroll_lines as f32) - base_scroll
    }

    fn single_session_live_id(&self) -> Option<String> {
        match self {
            Self::SingleSession(app) => app.live_session_id.clone(),
            Self::Workspace(_) => None,
        }
    }

    #[cfg(test)]
    fn debug_snapshot(&self) -> DesktopAppDebugSnapshot {
        match self {
            Self::SingleSession(app) => DesktopAppDebugSnapshot {
                mode: "single_session",
                title: app.title(),
                live_session_id: app.live_session_id.clone(),
                status: app.status.clone(),
                is_processing: app.is_processing,
                body_text: app.body_lines().join("\n"),
            },
            Self::Workspace(workspace) => DesktopAppDebugSnapshot {
                mode: "workspace",
                title: workspace.status_title(),
                live_session_id: None,
                status: None,
                is_processing: false,
                body_text: workspace.status_title(),
            },
        }
    }
}

fn to_key_input(key: &Key, modifiers: ModifiersState) -> KeyInput {
    match key {
        Key::Named(NamedKey::Escape) => KeyInput::Escape,
        Key::Named(NamedKey::Space) => KeyInput::Character(" ".to_string()),
        Key::Named(NamedKey::Enter) if modifiers.control_key() => KeyInput::QueueDraft,
        Key::Named(NamedKey::Enter) if modifiers.shift_key() => KeyInput::Enter,
        Key::Named(NamedKey::Enter) => KeyInput::SubmitDraft,
        Key::Named(NamedKey::Backspace) if modifiers.control_key() || modifiers.alt_key() => {
            KeyInput::DeletePreviousWord
        }
        Key::Named(NamedKey::Backspace) => KeyInput::Backspace,
        Key::Named(NamedKey::Delete) => KeyInput::DeleteNextChar,
        Key::Named(NamedKey::PageUp) => KeyInput::ScrollBodyPages(1),
        Key::Named(NamedKey::PageDown) => KeyInput::ScrollBodyPages(-1),
        Key::Named(NamedKey::ArrowUp) if modifiers.control_key() => KeyInput::RetrieveQueuedDraft,
        Key::Named(NamedKey::ArrowUp) if modifiers.alt_key() => KeyInput::JumpPrompt(-1),
        Key::Named(NamedKey::ArrowDown) if modifiers.alt_key() => KeyInput::JumpPrompt(1),
        Key::Named(NamedKey::ArrowUp) => KeyInput::ModelPickerMove(-1),
        Key::Named(NamedKey::ArrowDown) => KeyInput::ModelPickerMove(1),
        Key::Named(NamedKey::ArrowLeft) if modifiers.control_key() || modifiers.alt_key() => {
            KeyInput::MoveCursorWordLeft
        }
        Key::Named(NamedKey::ArrowRight) if modifiers.control_key() || modifiers.alt_key() => {
            KeyInput::MoveCursorWordRight
        }
        Key::Named(NamedKey::ArrowLeft) => KeyInput::MoveCursorLeft,
        Key::Named(NamedKey::ArrowRight) => KeyInput::MoveCursorRight,
        Key::Named(NamedKey::Home) => KeyInput::MoveToLineStart,
        Key::Named(NamedKey::End) => KeyInput::MoveToLineEnd,
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("a") => {
            KeyInput::MoveToLineStart
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("e") => {
            KeyInput::MoveToLineEnd
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("b") => {
            KeyInput::MoveCursorWordLeft
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("f") => {
            KeyInput::MoveCursorWordRight
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("u") => {
            KeyInput::DeleteToLineStart
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("k") => {
            KeyInput::DeleteToLineEnd
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("w") => {
            KeyInput::DeletePreviousWord
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("x") => {
            KeyInput::CutInputLine
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("z") => {
            KeyInput::UndoInput
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("c") =>
        {
            KeyInput::CopyLatestResponse
        }
        Key::Character(text)
            if modifiers.control_key()
                && (text.eq_ignore_ascii_case("c") || text.eq_ignore_ascii_case("d")) =>
        {
            KeyInput::CancelGeneration
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("b") => {
            KeyInput::MoveCursorWordLeft
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("f") => {
            KeyInput::MoveCursorWordRight
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("d") => {
            KeyInput::DeleteNextWord
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("v") => {
            KeyInput::AttachClipboardImage
        }
        Key::Character(text) if modifiers.control_key() && text == ";" => KeyInput::SpawnPanel,
        Key::Character(text) if modifiers.control_key() && (text == "?" || text == "/") => {
            KeyInput::HotkeyHelp
        }
        Key::Character(text)
            if modifiers.control_key()
                && (text.eq_ignore_ascii_case("p") || text.eq_ignore_ascii_case("o")) =>
        {
            KeyInput::OpenSessionSwitcher
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("r") => {
            KeyInput::RefreshSessions
        }
        Key::Character(text) if modifiers.control_key() && (text == "-" || text == "_") => {
            KeyInput::AdjustTextScale(-1)
        }
        Key::Character(text) if modifiers.control_key() && (text == "=" || text == "+") => {
            KeyInput::AdjustTextScale(1)
        }
        Key::Character(text) if modifiers.control_key() && text == "0" => KeyInput::ResetTextScale,
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("v") => {
            KeyInput::PasteText
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("i") =>
        {
            KeyInput::ClearAttachedImages
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("i") => {
            KeyInput::AttachClipboardImage
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("m") =>
        {
            KeyInput::OpenModelPicker
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("m") => {
            KeyInput::CycleModel(1)
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("n") => {
            KeyInput::CycleModel(-1)
        }
        Key::Character(text) if modifiers.control_key() && text == "1" => {
            KeyInput::SetPanelSize(PanelSizePreset::Quarter)
        }
        Key::Character(text) if modifiers.control_key() && text == "2" => {
            KeyInput::SetPanelSize(PanelSizePreset::Half)
        }
        Key::Character(text) if modifiers.control_key() && text == "3" => {
            KeyInput::SetPanelSize(PanelSizePreset::ThreeQuarter)
        }
        Key::Character(text) if modifiers.control_key() && text == "4" => {
            KeyInput::SetPanelSize(PanelSizePreset::Full)
        }
        Key::Character(_)
            if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() =>
        {
            KeyInput::Other
        }
        Key::Character(text) => KeyInput::Character(text.to_string()),
        _ => KeyInput::Other,
    }
}

fn apply_desktop_session_event_batch(
    app: &mut DesktopApp,
    events: Vec<session_launch::DesktopSessionEvent>,
) -> bool {
    if events.is_empty() {
        return false;
    }
    let started = Instant::now();
    let event_count = events.len();
    let mut text_delta_bytes = 0usize;
    let mut visible_changed = false;
    for event in events {
        if let session_launch::DesktopSessionEvent::TextDelta(text) = &event {
            text_delta_bytes += text.len();
        }
        visible_changed |= desktop_session_event_affects_visible_state(&event);
        app.apply_session_event(event);
    }
    log_desktop_slow_interaction(
        "session_event_apply",
        started.elapsed(),
        serde_json::json!({
            "events": event_count,
            "text_delta_bytes": text_delta_bytes,
        }),
    );
    visible_changed
}

fn desktop_session_event_affects_visible_state(
    event: &session_launch::DesktopSessionEvent,
) -> bool {
    !matches!(event, session_launch::DesktopSessionEvent::ToolInput { .. })
}

#[cfg(test)]
fn apply_pending_session_events(
    app: &mut DesktopApp,
    session_event_rx: &mpsc::Receiver<session_launch::DesktopSessionEvent>,
) -> bool {
    let mut events = Vec::new();
    while let Ok(event) = session_event_rx.try_recv() {
        events.push(event);
    }
    apply_desktop_session_event_batch(app, events)
}

fn apply_single_session_error(app: &mut DesktopApp, error: anyhow::Error) {
    app.apply_session_event(session_launch::DesktopSessionEvent::Error(format!(
        "{error:#}"
    )));
}

fn copy_text_to_clipboard(text: &str, success_notice: &'static str, app: &mut DesktopApp) {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.to_string())) {
        Ok(()) => app.apply_session_event(session_launch::DesktopSessionEvent::Status(
            success_notice.to_string(),
        )),
        Err(error) => app.apply_session_event(session_launch::DesktopSessionEvent::Error(format!(
            "failed to update clipboard after {success_notice}: {error}"
        ))),
    }
}

fn paste_clipboard_into_app(app: &mut DesktopApp) -> Result<()> {
    match clipboard_text() {
        Ok(text) => {
            if paste_clipboard_text(app, &text) || !app.accepts_clipboard_image_paste() {
                return Ok(());
            }
            paste_clipboard_image_into_app(app)
                .with_context(|| "clipboard text was empty and no pasteable image was available")
        }
        Err(text_error) if app.accepts_clipboard_image_paste() => {
            paste_clipboard_image_into_app(app)
                .with_context(|| format!("clipboard did not contain pasteable text: {text_error}"))
        }
        Err(error) => Err(error),
    }
}

fn paste_clipboard_text(app: &mut DesktopApp, text: &str) -> bool {
    let text = normalize_clipboard_text(text);
    if text.is_empty() {
        return false;
    }
    app.paste_text(&text);
    true
}

fn paste_clipboard_image_into_app(app: &mut DesktopApp) -> Result<()> {
    let (media_type, base64_data) = clipboard_image_png_base64()?;
    app.attach_clipboard_image(media_type, base64_data);
    Ok(())
}

fn normalize_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn clipboard_image_png_base64() -> Result<(String, String)> {
    let mut clipboard = arboard::Clipboard::new().context("failed to access clipboard")?;
    let image = clipboard
        .get_image()
        .context("clipboard does not contain an image")?;
    let width = u32::try_from(image.width).context("clipboard image is too wide")?;
    let height = u32::try_from(image.height).context("clipboard image is too tall")?;
    let rgba = image.bytes.into_owned();
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .context("clipboard image data had unexpected dimensions")?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("failed to encode clipboard image as png")?;
    Ok((
        "image/png".to_string(),
        base64::engine::general_purpose::STANDARD.encode(cursor.into_inner()),
    ))
}

fn clipboard_text() -> Result<String> {
    arboard::Clipboard::new()
        .context("failed to access clipboard")?
        .get_text()
        .context("clipboard does not contain text")
}

#[derive(Clone, Debug, Default)]
struct ScrollLineAccumulator {
    velocity_lines_per_second: f32,
    last_event_at: Option<Instant>,
    last_frame_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollAnimationFrame {
    scroll_lines: Option<f32>,
    active: bool,
}

impl ScrollLineAccumulator {
    fn scroll_lines(&mut self, delta: MouseScrollDelta, now: Instant) -> Option<f32> {
        if self
            .last_event_at
            .is_some_and(|last| now.saturating_duration_since(last) > SCROLL_GESTURE_IDLE_RESET)
        {
            self.stop();
        }
        self.last_event_at = Some(now);
        self.last_frame_at = Some(now);
        self.input_delta(mouse_scroll_delta_lines(delta))
    }

    fn frame(&mut self, now: Instant) -> ScrollAnimationFrame {
        let Some(last_frame_at) = self.last_frame_at else {
            self.last_frame_at = Some(now);
            return ScrollAnimationFrame {
                scroll_lines: None,
                active: self.is_active(),
            };
        };

        let dt = now
            .saturating_duration_since(last_frame_at)
            .as_secs_f32()
            .min(SCROLL_FRAME_MAX_DT_SECONDS);
        self.last_frame_at = Some(now);

        if dt <= 0.0 || !self.is_active() {
            return ScrollAnimationFrame {
                scroll_lines: None,
                active: self.is_active(),
            };
        }

        let scroll_lines = if self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
        {
            let lines = self.velocity_lines_per_second * dt;
            let decay = (-SCROLL_MOMENTUM_DECAY_PER_SECOND * dt).exp();
            self.velocity_lines_per_second *= decay;
            if self.velocity_lines_per_second.abs() < SCROLL_MOMENTUM_STOP_VELOCITY {
                self.velocity_lines_per_second = 0.0;
            }
            (lines.abs() >= SCROLL_FRACTIONAL_EPSILON).then_some(lines)
        } else {
            self.velocity_lines_per_second = 0.0;
            None
        };

        ScrollAnimationFrame {
            scroll_lines,
            active: self.is_active(),
        }
    }

    fn reset(&mut self) {
        self.stop();
        self.last_event_at = None;
        self.last_frame_at = None;
    }

    fn stop(&mut self) {
        self.velocity_lines_per_second = 0.0;
    }

    fn pending_lines(&self) -> f32 {
        0.0
    }

    fn is_active(&self) -> bool {
        self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
    }

    fn input_delta(&mut self, lines: f32) -> Option<f32> {
        if !lines.is_finite() || lines.abs() < SCROLL_FRACTIONAL_EPSILON {
            return None;
        }

        let lines = lines.clamp(
            -MAX_MOUSE_SCROLL_LINES_PER_EVENT,
            MAX_MOUSE_SCROLL_LINES_PER_EVENT,
        );
        if self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
            && self.velocity_lines_per_second.signum() != lines.signum()
        {
            self.stop();
        }

        self.velocity_lines_per_second = (self.velocity_lines_per_second
            + lines * SCROLL_MOMENTUM_GAIN)
            .clamp(-SCROLL_MOMENTUM_MAX_VELOCITY, SCROLL_MOMENTUM_MAX_VELOCITY);
        Some(lines)
    }
}

#[cfg(test)]
fn mouse_scroll_lines(delta: MouseScrollDelta) -> Option<f32> {
    ScrollLineAccumulator::default().scroll_lines(delta, Instant::now())
}

fn mouse_scroll_delta_lines(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y * MOUSE_WHEEL_LINES_PER_DETENT,
        MouseScrollDelta::PixelDelta(position) => position.y as f32 / body_scroll_line_pixels(),
    }
}

fn body_scroll_line_pixels() -> f32 {
    let typography = single_session_typography();
    typography.body_size * typography.body_line_height
}

fn desktop_spinner_tick(_now: Instant) -> u64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    (millis / DESKTOP_SPINNER_FRAME_MS) as u64
}

fn single_session_text_buffer_cache_key(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    tick: u64,
    rendered_body_key: u64,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    rendered_body_key.hash(&mut hasher);
    (size.width, size.height).hash(&mut hasher);
    app.is_welcome_timeline_visible().hash(&mut hasher);
    app.has_activity_indicator().hash(&mut hasher);
    app.text_scale().to_bits().hash(&mut hasher);
    app.header_title().hash(&mut hasher);
    app.welcome_hero_text().hash(&mut hasher);
    app.composer_text().hash(&mut hasher);
    app.composer_status_line_for_tick(tick).hash(&mut hasher);
    hasher.finish()
}

fn single_session_body_text_window_bounds(viewport: &SingleSessionBodyViewport) -> (usize, usize) {
    let start = viewport
        .start_line
        .saturating_sub(SINGLE_SESSION_BODY_TEXT_WINDOW_BEFORE_LINES);
    let end = viewport
        .start_line
        .saturating_add(viewport.lines.len())
        .saturating_add(SINGLE_SESSION_BODY_TEXT_WINDOW_AFTER_LINES)
        .min(viewport.total_lines);
    (start, end.max(start))
}

fn single_session_body_text_window_contains(
    window_start: usize,
    window_end: usize,
    viewport: &SingleSessionBodyViewport,
) -> bool {
    let visible_end = viewport.start_line.saturating_add(viewport.lines.len());
    window_start <= viewport.start_line && visible_end <= window_end
}

#[derive(Default)]
struct SingleSessionScrollMetricsCache {
    key: Option<u64>,
    total_lines: usize,
    streaming_base_key: Option<u64>,
    streaming_base_total_lines: usize,
}

impl SingleSessionScrollMetricsCache {
    fn metrics(
        &mut self,
        app: &SingleSessionApp,
        size: PhysicalSize<u32>,
    ) -> Option<SingleSessionBodyScrollMetrics> {
        let key = app.rendered_body_cache_key((size.width, size.height));
        if self.key != Some(key) {
            if !app.streaming_response.is_empty() {
                let base_key = app.rendered_body_static_cache_key((size.width, size.height));
                if self.streaming_base_key != Some(base_key) {
                    if let Some(base_lines) =
                        single_session_rendered_static_body_lines_for_streaming(app, size, 0)
                    {
                        self.streaming_base_total_lines = base_lines.len();
                        self.streaming_base_key = Some(base_key);
                    } else {
                        self.streaming_base_key = None;
                        self.streaming_base_total_lines = 0;
                    }
                }
                if self.streaming_base_key == Some(base_key) {
                    self.total_lines = self.streaming_base_total_lines
                        + single_session_streaming_response_rendered_body_line_count(app, size);
                } else {
                    self.total_lines =
                        single_session_rendered_body_lines_for_tick(app, size, 0).len();
                }
            } else {
                self.total_lines = single_session_rendered_body_lines_for_tick(app, size, 0).len();
                self.streaming_base_key = None;
                self.streaming_base_total_lines = 0;
            }
            self.key = Some(key);
        }
        single_session_body_scroll_metrics_for_total_lines(app, size, self.total_lines)
    }

    fn clear(&mut self) {
        self.key = None;
        self.total_lines = 0;
        self.streaming_base_key = None;
        self.streaming_base_total_lines = 0;
    }
}

#[derive(Clone)]
struct DesktopFrameStageProfile {
    name: &'static str,
    duration: Duration,
}

struct DesktopFrameProfile {
    started_at: Instant,
    last_checkpoint: Instant,
    stages: Vec<DesktopFrameStageProfile>,
}

impl DesktopFrameProfile {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_checkpoint: now,
            stages: Vec::with_capacity(14),
        }
    }

    fn checkpoint(&mut self, name: &'static str) {
        let now = Instant::now();
        self.stages.push(DesktopFrameStageProfile {
            name,
            duration: now.saturating_duration_since(self.last_checkpoint),
        });
        self.last_checkpoint = now;
    }

    fn total_duration(&self) -> Duration {
        self.last_checkpoint
            .saturating_duration_since(self.started_at)
    }
}

#[derive(Clone, Copy)]
struct DesktopFrameContext {
    mode: &'static str,
    smooth_scroll_lines: f32,
    text_buffer_count: usize,
    text_area_count: usize,
    primitive_vertices: usize,
    text_prepared: bool,
}

#[derive(Clone)]
struct DesktopFrameSlowSample {
    total: Duration,
    stages: Vec<DesktopFrameStageProfile>,
    context: DesktopFrameContext,
}

struct DesktopFrameProfiler {
    enabled: bool,
    log_all: bool,
    budget: Duration,
    report_interval: Duration,
    frames: usize,
    slow_frames: usize,
    worst: Option<DesktopFrameSlowSample>,
    last_report: Option<Instant>,
}

impl DesktopFrameProfiler {
    fn new() -> Self {
        let mode = std::env::var("JCODE_DESKTOP_FRAME_PROFILE").ok();
        let enabled = !matches!(mode.as_deref(), Some("0" | "false" | "off"));
        let log_all = matches!(mode.as_deref(), Some("all" | "trace"));
        let budget = std::env::var("JCODE_DESKTOP_FRAME_BUDGET_MS")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|ms| Duration::from_secs_f64(ms / 1000.0))
            .unwrap_or(DESKTOP_120FPS_FRAME_BUDGET);
        Self {
            enabled,
            log_all,
            budget,
            report_interval: DESKTOP_FRAME_PROFILE_REPORT_INTERVAL,
            frames: 0,
            slow_frames: 0,
            worst: None,
            last_report: None,
        }
    }

    fn observe(&mut self, profile: DesktopFrameProfile, context: DesktopFrameContext) {
        if !self.enabled {
            return;
        }

        self.frames += 1;
        let total = profile.total_duration();
        let slow = total >= self.budget;
        if slow || self.log_all {
            if slow {
                self.slow_frames += 1;
            }
            let replace_worst = self
                .worst
                .as_ref()
                .is_none_or(|sample| total > sample.total);
            if replace_worst {
                self.worst = Some(DesktopFrameSlowSample {
                    total,
                    stages: profile.stages,
                    context,
                });
            }
        }

        let now = Instant::now();
        let report_due = self.last_report.is_none_or(|last_report| {
            now.saturating_duration_since(last_report) >= self.report_interval
        });
        if report_due && (self.slow_frames > 0 || self.log_all) {
            self.report(now);
        }
    }

    fn report(&mut self, now: Instant) {
        if let Some(worst) = self.worst.as_ref() {
            eprintln!(
                "jcode-desktop-frame-profile {}",
                serde_json::json!({
                    "budget_ms": duration_ms(self.budget),
                    "window_frames": self.frames,
                    "slow_frames": self.slow_frames,
                    "worst_frame_ms": duration_ms(worst.total),
                    "mode": worst.context.mode,
                    "smooth_scroll_lines": worst.context.smooth_scroll_lines,
                    "text_buffer_count": worst.context.text_buffer_count,
                    "text_area_count": worst.context.text_area_count,
                    "primitive_vertices": worst.context.primitive_vertices,
                    "text_prepared": worst.context.text_prepared,
                    "stages": worst.stages.iter().map(|stage| serde_json::json!({
                        "name": stage.name,
                        "ms": duration_ms(stage.duration),
                    })).collect::<Vec<_>>(),
                })
            );
        }
        self.frames = 0;
        self.slow_frames = 0;
        self.worst = None;
        self.last_report = Some(now);
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn log_desktop_slow_interaction(
    kind: &'static str,
    duration: Duration,
    details: serde_json::Value,
) {
    if duration < DESKTOP_120FPS_FRAME_BUDGET {
        return;
    }
    let enabled = std::env::var("JCODE_DESKTOP_FRAME_PROFILE")
        .ok()
        .is_none_or(|value| !matches!(value.as_str(), "0" | "false" | "off"));
    if !enabled {
        return;
    }
    eprintln!(
        "jcode-desktop-interaction-profile {}",
        serde_json::json!({
            "kind": kind,
            "budget_ms": duration_ms(DESKTOP_120FPS_FRAME_BUDGET),
            "duration_ms": duration_ms(duration),
            "details": details,
        })
    );
}

struct Canvas<'window> {
    surface: wgpu::Surface<'window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    font_system: Option<FontSystem>,
    swash_cache: SwashCache,
    text_atlas: Option<TextAtlas>,
    text_renderer: Option<TextRenderer>,
    text_needs_prepare: bool,
    size: PhysicalSize<u32>,
    viewport_animation: AnimatedViewport,
    focus_pulse: FocusPulse,
    primitive_vertex_buffer: Option<wgpu::Buffer>,
    primitive_vertex_capacity: usize,
    needs_initial_frame: bool,
    defer_initial_text_frame: bool,
    single_session_text_cache_key: Option<u64>,
    single_session_text_key: Option<SingleSessionTextKey>,
    single_session_text_buffers: Vec<Buffer>,
    single_session_body_key: Option<u64>,
    single_session_body_lines: Vec<SingleSessionStyledLine>,
    single_session_streaming_base_key: Option<u64>,
    single_session_streaming_base_len: usize,
    single_session_streaming_text_key: Option<u64>,
    single_session_streaming_text_start_line: Option<usize>,
    single_session_streaming_text_buffer: Option<Buffer>,
    single_session_body_text_scroll_start: Option<usize>,
    single_session_body_text_window_start: Option<usize>,
    single_session_body_text_window_end: Option<usize>,
    welcome_hero_reveal_key: Option<String>,
    welcome_hero_reveal_started_at: Option<Instant>,
    frame_profiler: DesktopFrameProfiler,
}

impl<'window> Canvas<'window> {
    async fn new(window: &'window Window, startup_trace: DesktopStartupTrace) -> Result<Self> {
        let size = non_zero_size(window.inner_size());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        startup_trace.mark("wgpu instance created");
        let surface = instance
            .create_surface(window)
            .context("failed to create wgpu surface")?;
        startup_trace.mark("wgpu surface created");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("failed to find a compatible GPU adapter")?;
        startup_trace.mark("wgpu adapter ready");
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("jcode-desktop-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .context("failed to create wgpu device")?;
        startup_trace.mark("wgpu device ready");
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let present_mode = if capabilities.present_modes.contains(&PresentMode::Fifo) {
            PresentMode::Fifo
        } else {
            capabilities.present_modes[0]
        };
        let alpha_mode = if capabilities
            .alpha_modes
            .contains(&CompositeAlphaMode::Opaque)
        {
            CompositeAlphaMode::Opaque
        } else {
            capabilities.alpha_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        startup_trace.mark("surface configured");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jcode-desktop-primitive-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jcode-desktop-primitive-pipeline-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("jcode-desktop-primitive-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        startup_trace.mark("primitive pipeline ready");
        Ok(Self {
            surface,
            device,
            queue,
            config,
            render_pipeline,
            font_system: None,
            swash_cache: SwashCache::new(),
            text_atlas: None,
            text_renderer: None,
            text_needs_prepare: true,
            size,
            viewport_animation: AnimatedViewport::default(),
            focus_pulse: FocusPulse::default(),
            primitive_vertex_buffer: None,
            primitive_vertex_capacity: 0,
            needs_initial_frame: true,
            defer_initial_text_frame: true,
            single_session_text_cache_key: None,
            single_session_text_key: None,
            single_session_text_buffers: Vec::new(),
            single_session_body_key: None,
            single_session_body_lines: Vec::new(),
            single_session_streaming_base_key: None,
            single_session_streaming_base_len: 0,
            single_session_streaming_text_key: None,
            single_session_streaming_text_start_line: None,
            single_session_streaming_text_buffer: None,
            single_session_body_text_scroll_start: None,
            single_session_body_text_window_start: None,
            single_session_body_text_window_end: None,
            welcome_hero_reveal_key: None,
            welcome_hero_reveal_started_at: None,
            frame_profiler: DesktopFrameProfiler::new(),
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let size = non_zero_size(size);
        if self.size == size {
            return;
        }

        self.size = size;
        self.single_session_text_cache_key = None;
        self.single_session_text_key = None;
        self.single_session_body_key = None;
        self.single_session_streaming_base_key = None;
        self.single_session_streaming_base_len = 0;
        self.single_session_streaming_text_key = None;
        self.single_session_streaming_text_start_line = None;
        self.single_session_streaming_text_buffer = None;
        self.single_session_body_text_scroll_start = None;
        self.single_session_body_text_window_start = None;
        self.single_session_body_text_window_end = None;
        self.text_needs_prepare = true;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    fn refresh_cached_single_session_text_buffers(
        &mut self,
        app: &SingleSessionApp,
        now: Instant,
        smooth_scroll_lines: f32,
        rendered_body_key: u64,
        rendered_body_changed: bool,
    ) {
        let tick = desktop_spinner_tick(now);
        let viewport = single_session_body_viewport_from_lines(
            app,
            self.size,
            smooth_scroll_lines,
            &self.single_session_body_lines,
        );
        let text_cache_key =
            single_session_text_buffer_cache_key(app, self.size, tick, rendered_body_key);
        let key = single_session_text_key_for_tick_with_rendered_body(
            app,
            self.size,
            tick,
            smooth_scroll_lines,
            &self.single_session_body_lines,
        );
        let text_key_changed = self.single_session_text_key.as_ref() != Some(&key);
        if self.single_session_text_cache_key != Some(text_cache_key) || text_key_changed {
            let desired_body_window = self.single_session_body_buffer_window_bounds(app, &viewport);
            let body_window_contains = if let (Some(window_start), Some(window_end)) = (
                self.single_session_body_text_window_start,
                self.single_session_body_text_window_end,
            ) {
                self.single_session_body_buffer_window_contains(
                    app,
                    window_start,
                    window_end,
                    &viewport,
                )
            } else {
                false
            };
            let Some(font_system) = self.font_system.as_mut() else {
                self.single_session_text_cache_key = None;
                self.single_session_text_key = None;
                self.single_session_text_buffers.clear();
                self.single_session_body_text_scroll_start = None;
                self.single_session_body_text_window_start = None;
                self.single_session_body_text_window_end = None;
                return;
            };
            let previous_key = self.single_session_text_key.take();
            let mut old_buffers = std::mem::take(&mut self.single_session_text_buffers);
            let body_content_changed_in_buffer =
                rendered_body_changed && app.streaming_response.is_empty();
            let mut can_reuse_body_buffer =
                old_buffers.len() > 1 && body_window_contains && !body_content_changed_in_buffer;
            if old_buffers.len() > 1 && (!body_window_contains || body_content_changed_in_buffer) {
                let (window_start, window_end) = desired_body_window;
                old_buffers[1] = single_session_body_text_buffer_from_lines(
                    font_system,
                    &self.single_session_body_lines[window_start..window_end],
                    self.size,
                    app.text_scale(),
                );
                self.single_session_body_text_window_start = Some(window_start);
                self.single_session_body_text_window_end = Some(window_end);
                self.single_session_body_text_scroll_start = None;
                can_reuse_body_buffer = true;
            }
            self.single_session_text_buffers =
                single_session_text_buffers_from_key_reusing_unchanged(
                    &key,
                    previous_key.as_ref(),
                    old_buffers,
                    can_reuse_body_buffer,
                    self.size,
                    font_system,
                );
            self.single_session_text_key = Some(key);
            self.single_session_text_cache_key = Some(text_cache_key);
            if !can_reuse_body_buffer {
                self.single_session_body_text_scroll_start = None;
                self.single_session_body_text_window_start = None;
                self.single_session_body_text_window_end = None;
            }
            self.text_needs_prepare = true;
        }
        self.sync_single_session_body_text_window(app, &viewport);
    }

    fn sync_single_session_body_text_window(
        &mut self,
        app: &SingleSessionApp,
        viewport: &SingleSessionBodyViewport,
    ) {
        let desired_body_window = self.single_session_body_buffer_window_bounds(app, viewport);
        if let (Some(window_start), Some(window_end)) = (
            self.single_session_body_text_window_start,
            self.single_session_body_text_window_end,
        ) && self.single_session_body_buffer_window_contains(
            app,
            window_start,
            window_end,
            viewport,
        ) {
            self.sync_single_session_body_text_scroll(viewport.start_line, window_start);
            self.sync_single_session_streaming_text_buffer(app, viewport);
            return;
        }

        let (window_start, window_end) = desired_body_window;
        let window_lines = self.single_session_body_lines[window_start..window_end].to_vec();
        if let Some(font_system) = self.font_system.as_mut()
            && let Some(body_buffer) = self.single_session_text_buffers.get_mut(1)
        {
            *body_buffer = single_session_body_text_buffer_from_lines(
                font_system,
                &window_lines,
                self.size,
                app.text_scale(),
            );
            self.single_session_body_text_window_start = Some(window_start);
            self.single_session_body_text_window_end = Some(window_end);
            self.single_session_body_text_scroll_start = None;
            self.sync_single_session_body_text_scroll(viewport.start_line, window_start);
        }
        self.sync_single_session_streaming_text_buffer(app, viewport);
    }

    fn single_session_body_buffer_window_bounds(
        &self,
        app: &SingleSessionApp,
        viewport: &SingleSessionBodyViewport,
    ) -> (usize, usize) {
        let (window_start, window_end) = single_session_body_text_window_bounds(viewport);
        if app.streaming_response.is_empty() || self.single_session_streaming_base_len == 0 {
            return (window_start, window_end);
        }
        let end = window_end.min(self.single_session_streaming_base_len);
        let start = window_start.min(end);
        (start, end)
    }

    fn single_session_body_buffer_window_contains(
        &self,
        app: &SingleSessionApp,
        window_start: usize,
        window_end: usize,
        viewport: &SingleSessionBodyViewport,
    ) -> bool {
        if app.streaming_response.is_empty() || self.single_session_streaming_base_len == 0 {
            return single_session_body_text_window_contains(window_start, window_end, viewport);
        }
        let visible_static_start = viewport
            .start_line
            .min(self.single_session_streaming_base_len);
        let visible_static_end = viewport
            .start_line
            .saturating_add(viewport.lines.len())
            .min(self.single_session_streaming_base_len);
        visible_static_start >= visible_static_end
            || (window_start <= visible_static_start && visible_static_end <= window_end)
    }

    fn sync_single_session_streaming_text_buffer(
        &mut self,
        app: &SingleSessionApp,
        viewport: &SingleSessionBodyViewport,
    ) {
        let Some((start_line, end_line)) =
            self.single_session_streaming_visible_range(app, viewport)
        else {
            self.single_session_streaming_text_key = None;
            self.single_session_streaming_text_start_line = None;
            self.single_session_streaming_text_buffer = None;
            return;
        };

        let mut hasher = DefaultHasher::new();
        (self.size.width, self.size.height).hash(&mut hasher);
        app.text_scale().to_bits().hash(&mut hasher);
        start_line.hash(&mut hasher);
        end_line.hash(&mut hasher);
        self.single_session_body_lines[start_line..end_line].hash(&mut hasher);
        let key = hasher.finish();
        if self.single_session_streaming_text_key == Some(key) {
            return;
        }

        if let Some(font_system) = self.font_system.as_mut() {
            let lines = self.single_session_body_lines[start_line..end_line].to_vec();
            self.single_session_streaming_text_buffer =
                Some(single_session_body_text_buffer_from_lines(
                    font_system,
                    &lines,
                    self.size,
                    app.text_scale(),
                ));
            self.single_session_streaming_text_key = Some(key);
            self.single_session_streaming_text_start_line = Some(start_line);
            self.text_needs_prepare = true;
        }
    }

    fn single_session_streaming_visible_range(
        &self,
        app: &SingleSessionApp,
        viewport: &SingleSessionBodyViewport,
    ) -> Option<(usize, usize)> {
        if app.streaming_response.is_empty() || self.single_session_streaming_base_len == 0 {
            return None;
        }
        let streaming_start_line = self
            .single_session_streaming_base_len
            .saturating_add(usize::from(!app.messages.is_empty()));
        let visible_start = viewport.start_line;
        let visible_end = viewport.start_line.saturating_add(viewport.lines.len());
        let start = streaming_start_line.max(visible_start);
        let end = self.single_session_body_lines.len().min(visible_end);
        (start < end).then_some((start, end))
    }

    fn sync_single_session_body_text_scroll(&mut self, start_line: usize, window_start: usize) {
        if self.single_session_body_text_scroll_start == Some(start_line) {
            return;
        }
        if let Some(body_buffer) = self.single_session_text_buffers.get_mut(1) {
            body_buffer.set_scroll(
                start_line
                    .saturating_sub(window_start)
                    .min(i32::MAX as usize) as i32,
            );
            self.single_session_body_text_scroll_start = Some(start_line);
            self.text_needs_prepare = true;
        }
    }

    fn ensure_font_system(&mut self) {
        self.font_system.get_or_insert_with(|| {
            let mut font_system = FontSystem::new();
            font_system
                .db_mut()
                .load_font_data(include_bytes!("../assets/fonts/Kalam-Regular.ttf").to_vec());
            font_system
        });
    }

    fn ensure_text_renderer(&mut self) {
        if self.text_renderer.is_some() {
            return;
        }
        let mut text_atlas = TextAtlas::new(&self.device, &self.queue, self.config.format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &self.device,
            wgpu::MultisampleState::default(),
            None,
        );
        self.text_atlas = Some(text_atlas);
        self.text_renderer = Some(text_renderer);
        self.text_needs_prepare = true;
    }

    fn upload_primitive_vertices(&mut self, vertices: &[Vertex]) {
        if vertices.is_empty() {
            return;
        }

        if self.primitive_vertex_capacity < vertices.len() {
            self.primitive_vertex_capacity = vertices.len().next_power_of_two();
            let size = (self.primitive_vertex_capacity * std::mem::size_of::<Vertex>())
                as wgpu::BufferAddress;
            self.primitive_vertex_buffer =
                Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jcode-desktop-workspace-vertices"),
                    size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }

        if let Some(vertex_buffer) = self.primitive_vertex_buffer.as_ref() {
            self.queue
                .write_buffer(vertex_buffer, 0, bytemuck::cast_slice(vertices));
        }
    }

    fn cached_single_session_body_lines(
        &mut self,
        app: &SingleSessionApp,
        tick: u64,
    ) -> (u64, bool) {
        let key = app.rendered_body_cache_key((self.size.width, self.size.height));
        if self.single_session_body_key == Some(key) {
            return (key, false);
        }

        if !app.streaming_response.is_empty() {
            let base_key = app.rendered_body_static_cache_key((self.size.width, self.size.height));
            if self.single_session_streaming_base_key != Some(base_key) {
                if let Some(base_lines) =
                    single_session_rendered_static_body_lines_for_streaming(app, self.size, tick)
                {
                    self.single_session_body_lines = base_lines;
                    self.single_session_streaming_base_len = self.single_session_body_lines.len();
                    self.single_session_streaming_base_key = Some(base_key);
                    self.single_session_body_text_scroll_start = None;
                    self.single_session_body_text_window_start = None;
                    self.single_session_body_text_window_end = None;
                } else {
                    self.single_session_body_lines =
                        single_session_rendered_body_lines_for_tick(app, self.size, tick);
                    self.single_session_streaming_base_key = None;
                    self.single_session_streaming_base_len = 0;
                    self.single_session_body_key = Some(key);
                    self.single_session_body_text_scroll_start = None;
                    self.single_session_body_text_window_start = None;
                    self.single_session_body_text_window_end = None;
                    return (key, true);
                }
            } else {
                self.single_session_body_lines
                    .truncate(self.single_session_streaming_base_len);
            }
            append_single_session_streaming_response_rendered_body_lines(
                app,
                self.size,
                &mut self.single_session_body_lines,
            );
        } else {
            self.single_session_body_lines =
                single_session_rendered_body_lines_for_tick(app, self.size, tick);
            self.single_session_streaming_base_key = None;
            self.single_session_streaming_base_len = 0;
            self.single_session_body_text_window_start = None;
            self.single_session_body_text_window_end = None;
        }
        self.single_session_body_key = Some(key);
        self.single_session_body_text_scroll_start = None;
        (key, true)
    }

    fn welcome_hero_reveal_progress(
        &mut self,
        app: &SingleSessionApp,
        now: Instant,
    ) -> (f32, bool) {
        if !app.is_welcome_timeline_visible() {
            self.welcome_hero_reveal_key = None;
            self.welcome_hero_reveal_started_at = None;
            return (1.0, false);
        }

        let key = app.welcome_hero_text();
        if self.welcome_hero_reveal_key.as_deref() != Some(key.as_str()) {
            self.welcome_hero_reveal_key = Some(key);
            self.welcome_hero_reveal_started_at = Some(now);
        }

        let elapsed = self
            .welcome_hero_reveal_started_at
            .map(|started_at| now.saturating_duration_since(started_at))
            .unwrap_or_default();
        let progress = welcome_hero_reveal_progress_for_elapsed(elapsed);
        (progress, welcome_hero_reveal_is_active(progress))
    }

    fn render(
        &mut self,
        app: &DesktopApp,
        monitor_size: Option<PhysicalSize<u32>>,
        smooth_scroll_lines: f32,
    ) -> std::result::Result<bool, SurfaceError> {
        let mut frame_profile = DesktopFrameProfile::new();
        let frame = self.surface.get_current_texture()?;
        frame_profile.checkpoint("surface_acquire");
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("jcode-desktop-render-workspace"),
            });
        let now = Instant::now();
        let spinner_tick = desktop_spinner_tick(now);
        frame_profile.checkpoint("frame_setup");

        let (welcome_hero_reveal_progress, welcome_hero_reveal_active) =
            if let DesktopApp::SingleSession(single_session) = app {
                self.welcome_hero_reveal_progress(single_session, now)
            } else {
                self.welcome_hero_reveal_key = None;
                self.welcome_hero_reveal_started_at = None;
                (1.0, false)
            };
        frame_profile.checkpoint("welcome_reveal");

        let defer_text_this_frame = self.defer_initial_text_frame;
        if defer_text_this_frame {
            self.defer_initial_text_frame = false;
            self.single_session_text_cache_key = None;
            self.single_session_text_buffers.clear();
            self.single_session_streaming_text_key = None;
            self.single_session_streaming_text_start_line = None;
            self.single_session_streaming_text_buffer = None;
            self.single_session_body_text_scroll_start = None;
            self.single_session_body_text_window_start = None;
            self.single_session_body_text_window_end = None;
        } else if let DesktopApp::SingleSession(single_session) = app {
            let (rendered_body_key, rendered_body_changed) =
                self.cached_single_session_body_lines(single_session, spinner_tick);
            self.ensure_font_system();
            self.refresh_cached_single_session_text_buffers(
                single_session,
                now,
                smooth_scroll_lines,
                rendered_body_key,
                rendered_body_changed,
            );
        } else {
            self.single_session_text_cache_key = None;
            self.single_session_text_key = None;
            self.single_session_text_buffers.clear();
            self.single_session_streaming_text_key = None;
            self.single_session_streaming_text_start_line = None;
            self.single_session_streaming_text_buffer = None;
            self.single_session_body_text_scroll_start = None;
            self.single_session_body_text_window_start = None;
            self.single_session_body_text_window_end = None;
        }
        frame_profile.checkpoint("text_cache");
        if !self.single_session_text_buffers.is_empty() {
            self.ensure_text_renderer();
        }
        frame_profile.checkpoint("text_renderer");
        let text_buffers = &self.single_session_text_buffers;
        let has_text_buffers = !text_buffers.is_empty();
        let mut text_area_count = 0usize;
        let mut text_prepared = false;
        if self.text_needs_prepare {
            let text_areas = if let DesktopApp::SingleSession(single_session) = app {
                let viewport = single_session_body_viewport_from_lines(
                    single_session,
                    self.size,
                    smooth_scroll_lines,
                    &self.single_session_body_lines,
                );
                let mut areas = single_session_text_areas_for_app_with_cached_body_viewport(
                    single_session,
                    text_buffers,
                    self.size,
                    smooth_scroll_lines,
                    viewport.clone(),
                );
                if let (Some(buffer), Some(start_line)) = (
                    self.single_session_streaming_text_buffer.as_ref(),
                    self.single_session_streaming_text_start_line,
                ) {
                    areas.push(single_session_streaming_text_area_for_cached_body_viewport(
                        single_session,
                        buffer,
                        self.size,
                        viewport,
                        start_line,
                    ));
                }
                areas
            } else {
                single_session_text_areas(text_buffers, self.size)
            };
            text_area_count = text_areas.len();
            frame_profile.checkpoint("text_areas");
            if text_areas.is_empty() {
                self.text_needs_prepare = false;
            } else {
                text_prepared = true;
                let font_system = self
                    .font_system
                    .as_mut()
                    .expect("font system should be initialized before text prepare");
                let text_atlas = self
                    .text_atlas
                    .as_mut()
                    .expect("text atlas should be initialized before text prepare");
                let text_renderer = self
                    .text_renderer
                    .as_mut()
                    .expect("text renderer should be initialized before text prepare");
                if let Err(error) = text_renderer.prepare(
                    &self.device,
                    &self.queue,
                    font_system,
                    text_atlas,
                    Resolution {
                        width: self.config.width,
                        height: self.config.height,
                    },
                    text_areas,
                    &mut self.swash_cache,
                ) {
                    eprintln!("jcode-desktop: failed to prepare text: {error:?}");
                } else {
                    self.text_needs_prepare = false;
                }
            }
        } else {
            frame_profile.checkpoint("text_areas");
        }
        frame_profile.checkpoint("text_prepare");

        let (mut vertices, animation_active) = match app {
            DesktopApp::SingleSession(single_session) => {
                let focus_pulse = self.focus_pulse.frame(1, now);
                let animation_active = self.focus_pulse.is_animating()
                    || single_session.has_background_work()
                    || welcome_hero_reveal_active;
                (
                    build_single_session_vertices_with_cached_body(
                        single_session,
                        self.size,
                        focus_pulse,
                        spinner_tick,
                        smooth_scroll_lines,
                        welcome_hero_reveal_progress,
                        &self.single_session_body_lines,
                    ),
                    animation_active,
                )
            }
            DesktopApp::Workspace(workspace) => {
                let target_layout = workspace_render_layout(workspace, self.size, monitor_size);
                let render_layout = self.viewport_animation.frame(target_layout, now);
                let focus_pulse = self.focus_pulse.frame(workspace.focused_id, now);
                let animation_active =
                    self.viewport_animation.is_animating() || self.focus_pulse.is_animating();
                (
                    build_vertices(workspace, self.size, render_layout, focus_pulse),
                    animation_active,
                )
            }
        };
        frame_profile.checkpoint("vertices");
        if let DesktopApp::SingleSession(single_session) = app
            && spinner_tick % 6 < 3
        {
            push_single_session_caret(
                &mut vertices,
                single_session,
                self.size,
                text_buffers.get(2),
            );
        }
        frame_profile.checkpoint("caret");
        let primitive_vertex_count = vertices.len();
        self.upload_primitive_vertices(&vertices);
        frame_profile.checkpoint("primitive_upload");

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("jcode-desktop-workspace-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            render_pass.set_pipeline(&self.render_pipeline);
            if let Some(vertex_buffer) = self.primitive_vertex_buffer.as_ref() {
                render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                render_pass.draw(0..vertices.len() as u32, 0..1);
            }
            if has_text_buffers
                && let (Some(text_renderer), Some(text_atlas)) =
                    (self.text_renderer.as_mut(), self.text_atlas.as_ref())
                && let Err(error) = text_renderer.render(text_atlas, &mut render_pass)
            {
                eprintln!("jcode-desktop: failed to render text: {error:?}");
            }
        }
        frame_profile.checkpoint("render_pass");

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        frame_profile.checkpoint("submit_present");
        self.frame_profiler.observe(
            frame_profile,
            DesktopFrameContext {
                mode: match app {
                    DesktopApp::SingleSession(_) => "single_session",
                    DesktopApp::Workspace(_) => "workspace",
                },
                smooth_scroll_lines,
                text_buffer_count: self.single_session_text_buffers.len()
                    + usize::from(self.single_session_streaming_text_buffer.is_some()),
                text_area_count,
                primitive_vertices: primitive_vertex_count,
                text_prepared,
            },
        );
        Ok(animation_active || defer_text_this_frame)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

impl Vertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn build_vertices(
    workspace: &Workspace,
    size: PhysicalSize<u32>,
    render_layout: WorkspaceRenderLayout,
    focus_pulse: f32,
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

    let status_color = match workspace.mode {
        InputMode::Navigation => NAV_STATUS_COLOR,
        InputMode::Insert => INSERT_STATUS_COLOR,
    };
    let status_rect = Rect {
        x: OUTER_PADDING,
        y: OUTER_PADDING,
        width: (width - OUTER_PADDING * 2.0).max(1.0),
        height: STATUS_BAR_HEIGHT,
    };
    push_rounded_rect(
        &mut vertices,
        status_rect,
        STATUS_RADIUS,
        status_color,
        size,
    );

    let active_workspace = workspace.current_workspace();
    let visible_layout = render_layout.visible;
    push_workspace_number(&mut vertices, active_workspace, status_rect, size);
    push_status_preview(
        &mut vertices,
        workspace,
        active_workspace,
        visible_layout,
        status_rect,
        size,
    );
    push_status_text(&mut vertices, workspace, status_rect, size);

    if workspace.zoomed {
        if let Some(surface) = workspace.focused_surface() {
            let rect = Rect {
                x: OUTER_PADDING,
                y: STATUS_BAR_HEIGHT + OUTER_PADDING * 2.0,
                width: (width - OUTER_PADDING * 2.0).max(1.0),
                height: (height - STATUS_BAR_HEIGHT - OUTER_PADDING * 3.0).max(1.0),
            };
            push_surface(
                &mut vertices,
                rect,
                surface.color_index,
                true,
                focus_pulse,
                size,
            );
            let draft = focused_panel_draft(workspace, surface.id);
            push_panel_contents(
                &mut vertices,
                surface,
                rect,
                size,
                true,
                workspace.detail_scroll,
                draft.as_deref(),
            );
        }
        return vertices;
    }

    let workspace_height = (height - STATUS_BAR_HEIGHT - OUTER_PADDING * 3.0).max(1.0);
    let workspace_top = STATUS_BAR_HEIGHT + OUTER_PADDING * 2.0;
    let lane_pitch = workspace_height + GAP;
    let column_width = render_layout.column_width;
    let scroll_offset = render_layout.scroll_offset;
    let vertical_scroll_offset = render_layout.vertical_scroll_offset;

    for surface in &workspace.surfaces {
        let column = surface.column as f32;
        let y = workspace_top + surface.lane as f32 * lane_pitch - vertical_scroll_offset;
        if y + workspace_height < workspace_top || y > workspace_top + workspace_height {
            continue;
        }
        let rect = Rect {
            x: OUTER_PADDING + column * (column_width + GAP) - scroll_offset,
            y,
            width: column_width,
            height: workspace_height,
        };
        let focused = workspace.is_focused(surface.id);
        let surface_pulse = if focused { focus_pulse } else { 0.0 };
        push_surface(
            &mut vertices,
            rect,
            surface.color_index,
            focused,
            surface_pulse,
            size,
        );
        let draft = focused_panel_draft(workspace, surface.id);
        push_panel_contents(
            &mut vertices,
            surface,
            rect,
            size,
            false,
            0,
            draft.as_deref(),
        );
    }

    vertices
}

fn workspace_render_layout(
    workspace: &Workspace,
    size: PhysicalSize<u32>,
    monitor_size: Option<PhysicalSize<u32>>,
) -> WorkspaceRenderLayout {
    let workspace_width = (size.width as f32 - OUTER_PADDING * 2.0).max(1.0);
    let workspace_height = (size.height as f32 - STATUS_BAR_HEIGHT - OUTER_PADDING * 3.0).max(1.0);
    let lane_pitch = workspace_height + GAP;
    let active_workspace = workspace.current_workspace();
    let visible = visible_column_layout(
        workspace,
        size.width,
        monitor_size.map(|size| size.width),
        active_workspace,
    );
    let visible_columns_f = visible.visible_columns as f32;
    let total_gap_width = GAP * (visible_columns_f - 1.0).max(0.0);
    let column_width = ((workspace_width - total_gap_width) / visible_columns_f).max(1.0);
    let scroll_offset = visible.first_visible_column as f32 * (column_width + GAP);
    let vertical_scroll_offset = active_workspace as f32 * lane_pitch;

    WorkspaceRenderLayout {
        visible,
        column_width,
        scroll_offset,
        vertical_scroll_offset,
    }
}

fn visible_column_layout(
    workspace: &Workspace,
    window_width: u32,
    monitor_width: Option<u32>,
    active_workspace: i32,
) -> VisibleColumnLayout {
    let visible_columns = inferred_visible_column_count(
        window_width,
        monitor_width,
        workspace.preferred_panel_screen_fraction(),
    );
    let focused_column = workspace
        .focused_surface()
        .map(|surface| surface.column)
        .unwrap_or_default();
    let (min_column, max_column) = workspace
        .surfaces
        .iter()
        .filter(|surface| surface.lane == active_workspace)
        .map(|surface| surface.column)
        .fold((focused_column, focused_column), |(min, max), column| {
            (min.min(column), max.max(column))
        });
    let visible_columns_i = visible_columns as i32;
    let max_first_column = (max_column - visible_columns_i + 1).max(min_column);
    let preferred_first_column = focused_column - visible_columns_i / 2;
    let first_visible_column = preferred_first_column.clamp(min_column, max_first_column);

    VisibleColumnLayout {
        visible_columns,
        first_visible_column,
    }
}

fn inferred_visible_column_count(
    window_width: u32,
    monitor_width: Option<u32>,
    preferred_panel_screen_fraction: f32,
) -> u32 {
    let Some(monitor_width) = monitor_width.filter(|width| *width > 0) else {
        return 1;
    };

    let preferred_panel_screen_fraction = preferred_panel_screen_fraction.clamp(0.25, 1.0);
    let target_panel_width = monitor_width as f32 * preferred_panel_screen_fraction;
    ((window_width as f32 / target_panel_width + PANEL_FIT_TOLERANCE).floor() as u32).clamp(1, 4)
}

fn push_status_text(
    vertices: &mut Vec<Vertex>,
    workspace: &Workspace,
    status_rect: Rect,
    size: PhysicalSize<u32>,
) {
    let text = workspace_status_text(workspace);
    let text_width = bitmap_text_width(&text, BITMAP_TEXT_PIXEL);
    let x = status_rect.x + status_rect.width - STATUS_TEXT_RIGHT_PADDING - text_width;
    let y = status_rect.y + (status_rect.height - bitmap_text_height(BITMAP_TEXT_PIXEL)) / 2.0;
    if x > status_rect.x {
        push_bitmap_text(
            vertices,
            &text,
            x,
            y,
            BITMAP_TEXT_PIXEL,
            STATUS_TEXT_COLOR,
            size,
            text_width,
        );
    }
}

fn workspace_status_text(workspace: &Workspace) -> String {
    let mode = match workspace.mode {
        InputMode::Navigation => "NAV",
        InputMode::Insert => "INS",
    };
    let panel_percent = (workspace.preferred_panel_screen_fraction() * 100.0).round() as u32;
    format!("{mode} P{panel_percent} {}", desktop_build_hash_label())
}

fn desktop_build_hash_label() -> &'static str {
    option_env!("JCODE_DESKTOP_GIT_HASH").unwrap_or("unknown")
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
