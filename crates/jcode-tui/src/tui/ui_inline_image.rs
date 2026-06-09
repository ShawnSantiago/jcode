//! Inline image transcript section.
//!
//! Images attached to the conversation (pasted screenshots, `read` of an image
//! file, generated images) render directly in the chat flow, sized to fit the
//! chat width with a capped height. This replaces the old "pinned image side
//! panel" surface.
//!
//! Design goals:
//! * **Lazy.** Prepare only needs each image's `(id, width, height)`, obtained
//!   from a cheap header parse (no full decode, no disk write, no retained
//!   bytes). The full decode + terminal transmit happens at draw time, and only
//!   for images currently on screen.
//! * **Single source of pixels.** The base64 payloads stay in their existing
//!   owner (`App::side_pane_images()`); this section keeps only ids and a small
//!   ingest-time payload registry so the draw step can materialize on demand.
//! * **Correct fit.** Images scale to fit width (preserving aspect) and cap at a
//!   fraction of the viewport so a tall screenshot never buries the transcript.

use crate::tui::mermaid;
use jcode_tui_messages::{ImageRegion, ImageRegionRender, PreparedMessages};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::{LazyLock, Mutex};

#[inline]
fn div_ceil_u32(value: u32, divisor: u32) -> u32 {
    let divisor = divisor.max(1);
    value.div_ceil(divisor)
}

/// One image to render inline, resolved from a `RenderedImage`.
pub(crate) struct InlineImageItem {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub label: String,
}

/// Default font cell size when the terminal has not reported one yet.
const DEFAULT_CELL: (u16, u16) = (8, 16);
/// Cap an inline image at this fraction of the chat viewport height so a tall
/// image cannot push the rest of the transcript off-screen.
const MAX_VIEWPORT_FRACTION_PERCENT: u16 = 55;
/// Never shrink an inline image below this many rows.
const MIN_IMAGE_ROWS: u16 = 3;

/// Ingest-time registry mapping image id -> (media_type, base64) so the draw
/// step can materialize bytes without threading payloads through the cached
/// prepared-frame model. Bounded; entries are cheap (two `String`s + id).
static PAYLOAD_REGISTRY: LazyLock<Mutex<PayloadRegistry>> =
    LazyLock::new(|| Mutex::new(PayloadRegistry::new()));

const PAYLOAD_REGISTRY_MAX: usize = 512;

struct PayloadRegistry {
    map: std::collections::HashMap<u64, (String, String)>,
    order: std::collections::VecDeque<u64>,
}

impl PayloadRegistry {
    fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
        }
    }

    fn insert(&mut self, id: u64, media_type: String, data_b64: String) {
        if self.map.contains_key(&id) {
            return;
        }
        self.map.insert(id, (media_type, data_b64));
        self.order.push_back(id);
        while self.order.len() > PAYLOAD_REGISTRY_MAX {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    fn get(&self, id: u64) -> Option<(String, String)> {
        self.map.get(&id).cloned()
    }
}

/// Record an image payload so [`materialize_visible`] can decode it on demand.
pub(crate) fn register_payload(id: u64, media_type: &str, data_b64: &str) {
    if let Ok(mut reg) = PAYLOAD_REGISTRY.lock() {
        reg.insert(id, media_type.to_string(), data_b64.to_string());
    }
}

/// Ensure the image with `id` is materialized (decoded + cached) so it can be
/// drawn. Returns true on success. Cheap and idempotent on repeat.
pub(crate) fn materialize_visible(id: u64) -> bool {
    if let Some((media_type, data_b64)) = PAYLOAD_REGISTRY
        .lock()
        .ok()
        .and_then(|reg| reg.get(id))
    {
        return mermaid::materialize_inline_image(&media_type, &data_b64).is_some();
    }
    false
}

/// Resolve the app's rendered images into lazily-sized inline items. Performs
/// only header-level work (no full decode) and registers each payload for the
/// later draw-time materialize.
pub(crate) fn resolve_items(images: &[crate::session::RenderedImage]) -> Vec<InlineImageItem> {
    let mut items = Vec::new();
    for image in images {
        let Some((id, width, height)) =
            mermaid::inline_image_dims(&image.media_type, &image.data)
        else {
            continue;
        };
        register_payload(id, &image.media_type, &image.data);
        let label = image
            .label
            .clone()
            .unwrap_or_else(|| image.media_type.clone());
        items.push(InlineImageItem {
            id,
            width,
            height,
            label,
        });
    }
    items
}

/// Compute how many rows an inline image should occupy at `chat_width`, given a
/// viewport height to cap against.
fn fit_rows(width: u32, height: u32, chat_width: u16, viewport_height: u16) -> u16 {
    if width == 0 || height == 0 {
        return MIN_IMAGE_ROWS;
    }
    let (cell_w, cell_h) = mermaid::get_font_size().unwrap_or(DEFAULT_CELL);
    let cell_w = cell_w.max(1) as u32;
    let cell_h = cell_h.max(1) as u32;

    // Available width in pixels (leave 1 cell for the left border bar).
    let avail_cells = chat_width.saturating_sub(1).max(1) as u32;
    let avail_px = avail_cells * cell_w;

    // Native pixel height, unless the image is wider than the pane, in which
    // case it scales down to fit width (preserving aspect ratio).
    let scaled_h_px = if width <= avail_px {
        height
    } else {
        height.saturating_mul(avail_px) / width.max(1)
    };

    let rows = div_ceil_u32(scaled_h_px.max(1), cell_h).max(MIN_IMAGE_ROWS as u32) as u16;

    // Cap to a fraction of the viewport so tall images stay manageable.
    let cap = ((viewport_height as u32 * MAX_VIEWPORT_FRACTION_PERCENT as u32) / 100)
        .max(MIN_IMAGE_ROWS as u32) as u16;
    rows.min(cap.max(MIN_IMAGE_ROWS))
}

/// Build the inline-images prepared section: a heading + correctly-sized
/// placeholder per image, with explicit `image_regions` (render = Fit) that the
/// viewport draws lazily.
pub(crate) fn build_section(
    items: &[InlineImageItem],
    width: u16,
    viewport_height: u16,
    prefix_blank: bool,
) -> PreparedMessages {
    use std::sync::Arc;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut image_regions: Vec<ImageRegion> = Vec::new();

    if items.is_empty() {
        return empty();
    }

    if prefix_blank {
        lines.push(Line::from(""));
    }

    for item in items {
        // Label line (dim), e.g. "🖼 screenshot.png  1920×1080".
        let dims = format!("{}×{}", item.width, item.height);
        let label = if item.label.is_empty() {
            dims.clone()
        } else {
            format!("{}  {}", item.label, dims)
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  🖼 ",
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled(label, Style::default().add_modifier(Modifier::DIM)),
        ]));

        let rows = fit_rows(item.width, item.height, width, viewport_height);
        let region_start = lines.len();
        for _ in 0..rows {
            lines.push(Line::from(""));
        }
        image_regions.push(ImageRegion {
            abs_line_idx: region_start,
            end_line: region_start + rows as usize,
            hash: item.id,
            height: rows,
            render: ImageRegionRender::Fit,
        });
        // Trailing spacer between images.
        lines.push(Line::from(""));
    }

    let line_count = lines.len();
    let plain: Vec<String> = lines.iter().map(jcode_tui_render::line_plain_text).collect();

    PreparedMessages {
        wrapped_lines: lines,
        wrapped_plain_lines: Arc::new(plain),
        wrapped_copy_offsets: Arc::new(vec![0; line_count]),
        raw_plain_lines: Arc::new(Vec::new()),
        wrapped_line_map: Arc::new(Vec::new()),
        wrapped_user_indices: Vec::new(),
        wrapped_user_prompt_starts: Vec::new(),
        wrapped_user_prompt_ends: Vec::new(),
        user_prompt_texts: Vec::new(),
        image_regions,
        edit_tool_ranges: Vec::new(),
        copy_targets: Vec::new(),
    }
}

fn empty() -> PreparedMessages {
    use std::sync::Arc;
    PreparedMessages {
        wrapped_lines: Vec::new(),
        wrapped_plain_lines: Arc::new(Vec::new()),
        wrapped_copy_offsets: Arc::new(Vec::new()),
        raw_plain_lines: Arc::new(Vec::new()),
        wrapped_line_map: Arc::new(Vec::new()),
        wrapped_user_indices: Vec::new(),
        wrapped_user_prompt_starts: Vec::new(),
        wrapped_user_prompt_ends: Vec::new(),
        user_prompt_texts: Vec::new(),
        image_regions: Vec::new(),
        edit_tool_ranges: Vec::new(),
        copy_targets: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(width: u32, height: u32) -> InlineImageItem {
        InlineImageItem {
            id: 0xABCD,
            width,
            height,
            label: "test.png".to_string(),
        }
    }

    #[test]
    fn fit_rows_caps_tall_image_to_viewport_fraction() {
        // A very tall image must be capped so it cannot bury the transcript.
        let rows = fit_rows(100, 100_000, 80, 40);
        let cap = ((40u32 * MAX_VIEWPORT_FRACTION_PERCENT as u32) / 100) as u16;
        assert!(rows <= cap, "rows {rows} should be <= cap {cap}");
        assert!(rows >= MIN_IMAGE_ROWS);
    }

    #[test]
    fn fit_rows_never_below_minimum() {
        let rows = fit_rows(10, 10, 80, 40);
        assert!(rows >= MIN_IMAGE_ROWS);
    }

    #[test]
    fn build_section_emits_one_fit_region_per_image_with_label() {
        let items = vec![item(600, 400), item(800, 600)];
        let section = build_section(&items, 80, 40, true);
        assert_eq!(section.image_regions.len(), 2);
        for region in &section.image_regions {
            assert_eq!(region.render, ImageRegionRender::Fit);
            assert_eq!(region.hash, 0xABCD);
            // The region must point at blank placeholder lines, never the label.
            let first = &section.wrapped_lines[region.abs_line_idx];
            assert!(
                jcode_tui_render::line_plain_text(first).trim().is_empty(),
                "region should start on a blank placeholder line"
            );
            // Region height must match its line span.
            assert_eq!(
                region.end_line - region.abs_line_idx,
                region.height as usize
            );
        }
        // A dim label line precedes the first region.
        let label_line = jcode_tui_render::line_plain_text(&section.wrapped_lines[1]);
        assert!(label_line.contains("test.png"), "label missing: {label_line:?}");
    }

    #[test]
    fn build_section_is_empty_for_no_items() {
        let section = build_section(&[], 80, 40, false);
        assert!(section.wrapped_lines.is_empty());
        assert!(section.image_regions.is_empty());
    }

    #[test]
    fn payload_registry_roundtrips() {
        register_payload(0xDEAD_BEEF, "image/png", "AAAA");
        let got = PAYLOAD_REGISTRY.lock().unwrap().get(0xDEAD_BEEF);
        assert_eq!(got, Some(("image/png".to_string(), "AAAA".to_string())));
    }
}
