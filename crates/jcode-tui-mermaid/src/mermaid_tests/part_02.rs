#[test]
fn precise_viewport_accepts_high_auto_zoom_without_panicking() {
    let area = ratatui::prelude::Rect::new(0, 0, 40, 20);
    let mut buf = ratatui::buffer::Buffer::empty(area);

    // No picker/cache is installed in this unit test, so rendering returns 0.
    // The important regression coverage is that the high-zoom precise API is
    // accepted and follows the normal graceful early-return path.
    assert_eq!(
        super::render_image_widget_viewport_precise(0xdef, area, &mut buf, 12, 0, 1000, false),
        0
    );
}

#[test]
fn viewport_crop_resize_scales_complete_zoomed_crops_to_fill_destination() {
    // A high-zoom fit-fill viewport crops a small source rectangle, then must
    // scale that crop back up to the destination cell area. Rendering it with
    // Fit caused the pane to report fit-fill while visually staying tiny.
    assert!(super::viewport_render::viewport_crop_should_scale_to_area(
        280, 180, 280, 180
    ));

    // When the requested viewport is larger than the source on an axis, the
    // crop is the whole remaining source image. That case should keep aspect
    // ratio instead of stretching a non-cropped image.
    assert!(!super::viewport_render::viewport_crop_should_scale_to_area(
        280, 120, 280, 180
    ));
    assert!(!super::viewport_render::viewport_crop_should_scale_to_area(
        200, 180, 280, 180
    ));
}
