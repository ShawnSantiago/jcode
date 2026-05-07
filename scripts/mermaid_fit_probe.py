#!/usr/bin/env python3
"""Probe Jcode pinned Mermaid fit planning for screenshot repros.

This intentionally mirrors the geometry math in src/tui/ui_diagram_pane.rs so a
bad crop can be reproduced without compiling the whole Rust test binary.
Defaults are the 2026-05-07 Beetle/Harbor clipped Mermaid screenshot:
  inner pane 73x46 cells, font 8x16 px, PNG 1180x1470 px.
"""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

TARGET_UTILIZATION_PERCENT = 85.0
MIN_READABLE_ZOOM_PERCENT = 70
MAX_AUTO_FILL_ZOOM_PERCENT = 1000


def clamp(value: int, lo: int, hi: int) -> int:
    return max(lo, min(hi, value))


def utilization_percent(used: int, total: int) -> float:
    return 0.0 if total == 0 else (used * 100.0) / total


def div_ceil(value: int, divisor: int) -> int:
    return 0 if divisor == 0 else (value + divisor - 1) // divisor


def axis_fill_zoom_percent(available_cells: int, image_px: int, cell_px: int) -> int:
    if available_cells == 0 or image_px == 0 or cell_px == 0:
        return 100
    return clamp((available_cells * cell_px * 100) // max(image_px, 1), 1, MAX_AUTO_FILL_ZOOM_PERCENT)


def fit_zoom_percent_for_area(width_cells: int, height_cells: int, img_w_px: int, img_h_px: int, font_w: int, font_h: int) -> int:
    if width_cells == 0 or height_cells == 0 or img_w_px == 0 or img_h_px == 0:
        return 100
    zoom_w = (width_cells * max(font_w, 1) * 100) // max(img_w_px, 1)
    zoom_h = (height_cells * max(font_h, 1) * 100) // max(img_h_px, 1)
    return clamp(min(zoom_w, zoom_h), 1, MAX_AUTO_FILL_ZOOM_PERCENT)


def vcenter_fitted(width_cells: int, height_cells: int, img_w_px: int, img_h_px: int, font_w: int, font_h: int) -> dict[str, int]:
    if width_cells == 0 or height_cells == 0 or img_w_px == 0 or img_h_px == 0:
        return {"x": 0, "y": 0, "width": width_cells, "height": height_cells}
    area_w_px = width_cells * max(font_w, 1)
    area_h_px = height_cells * max(font_h, 1)
    scale = min(area_w_px / img_w_px, area_h_px / img_h_px)
    fitted_w = min(math.ceil((img_w_px * scale) / max(font_w, 1)), width_cells)
    fitted_h = min(math.ceil((img_h_px * scale) / max(font_h, 1)), height_cells)
    return {
        "x": (width_cells - fitted_w) // 2,
        "y": (height_cells - fitted_h) // 2,
        "width": fitted_w,
        "height": fitted_h,
    }


def centered_viewport_scroll_cells(image_px: int, area_cells: int, zoom_percent: int, cell_px: int) -> int:
    if image_px == 0 or area_cells == 0 or zoom_percent == 0 or cell_px == 0:
        return 0
    view_px = area_cells * cell_px * 100 // zoom_percent
    max_scroll_px = max(0, image_px - view_px)
    if max_scroll_px == 0:
        return 0
    cell_px_at_zoom = max(div_ceil(cell_px * 100, zoom_percent), 1)
    return (max_scroll_px // 2) // cell_px_at_zoom


def plan(width_cells: int, height_cells: int, img_w_px: int, img_h_px: int, font_w: int, font_h: int) -> dict[str, Any]:
    contain = vcenter_fitted(width_cells, height_cells, img_w_px, img_h_px, font_w, font_h)
    fit_zoom = fit_zoom_percent_for_area(width_cells, height_cells, img_w_px, img_h_px, font_w, font_h)
    width_fill_zoom = axis_fill_zoom_percent(width_cells, img_w_px, font_w)
    height_fill_zoom = axis_fill_zoom_percent(height_cells, img_h_px, font_h)
    preferred_fill_zoom = clamp(max(width_fill_zoom, height_fill_zoom), MIN_READABLE_ZOOM_PERCENT, MAX_AUTO_FILL_ZOOM_PERCENT)

    width_utilization = utilization_percent(contain["width"], width_cells)
    height_utilization = utilization_percent(contain["height"], height_cells)
    area_utilization = utilization_percent(contain["width"] * contain["height"], width_cells * height_cells)
    underutilized = (
        width_utilization < TARGET_UTILIZATION_PERCENT
        or height_utilization < TARGET_UTILIZATION_PERCENT
        or area_utilization < TARGET_UTILIZATION_PERCENT
    )
    meaningfully_larger = preferred_fill_zoom > fit_zoom + 5

    old_would_fill = (fit_zoom < MIN_READABLE_ZOOM_PERCENT or underutilized) and meaningfully_larger
    fixed_would_fill = underutilized and meaningfully_larger

    fill_plan = {
        "mode": f"fit-fill@{preferred_fill_zoom}%",
        "zoom_percent": preferred_fill_zoom,
        "scroll_x": centered_viewport_scroll_cells(img_w_px, width_cells, preferred_fill_zoom, font_w),
        "scroll_y": centered_viewport_scroll_cells(img_h_px, height_cells, preferred_fill_zoom, font_h),
    }

    return {
        "input": {
            "inner_width_cells": width_cells,
            "inner_height_cells": height_cells,
            "image_width_px": img_w_px,
            "image_height_px": img_h_px,
            "font_width_px": font_w,
            "font_height_px": font_h,
        },
        "contain_rect_cells": contain,
        "utilization_percent": {
            "width": width_utilization,
            "height": height_utilization,
            "area": area_utilization,
        },
        "fit_zoom_percent": fit_zoom,
        "axis_fill_zoom_percent": {"width": width_fill_zoom, "height": height_fill_zoom},
        "preferred_fill_zoom_percent": preferred_fill_zoom,
        "underutilized": underutilized,
        "meaningfully_larger": meaningfully_larger,
        "old_buggy_plan": fill_plan if old_would_fill else {"mode": "fit", "rect": contain},
        "fixed_plan": fill_plan if fixed_would_fill else {"mode": "fit", "rect": contain},
        "repro_was_clipping_bug": old_would_fill and not fixed_would_fill,
    }


def maybe_png_info(path: str | None) -> dict[str, Any] | None:
    if not path:
        return None
    png = Path(path).expanduser()
    info: dict[str, Any] = {"path": str(png), "exists": png.exists()}
    if not png.exists():
        return info
    try:
        from PIL import Image  # type: ignore
    except Exception as exc:  # pragma: no cover - diagnostic fallback
        info["pil_error"] = str(exc)
        return info
    im = Image.open(png).convert("RGBA")
    bbox = im.getchannel("A").getbbox()
    info["size"] = list(im.size)
    info["alpha_bbox"] = list(bbox) if bbox else None
    if bbox:
        left, top, right, bottom = bbox
        info["content_size"] = [right - left, bottom - top]
        info["transparent_margins"] = {
            "left": left,
            "top": top,
            "right": im.size[0] - right,
            "bottom": im.size[1] - bottom,
        }
    return info


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inner", default="73x46", help="inner pane size in cells, WIDTHxHEIGHT")
    parser.add_argument("--image", default="1180x1470", help="rendered PNG size in px, WIDTHxHEIGHT")
    parser.add_argument("--font", default="8x16", help="terminal cell size in px, WIDTHxHEIGHT")
    parser.add_argument("--png", help="optional rendered PNG path to inspect alpha bounds")
    args = parser.parse_args()

    def parse_pair(raw: str) -> tuple[int, int]:
        left, right = raw.lower().split("x", 1)
        return int(left), int(right)

    width_cells, height_cells = parse_pair(args.inner)
    img_w_px, img_h_px = parse_pair(args.image)
    font_w, font_h = parse_pair(args.font)
    result = plan(width_cells, height_cells, img_w_px, img_h_px, font_w, font_h)
    png_info = maybe_png_info(args.png)
    if png_info is not None:
        result["png"] = png_info
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["repro_was_clipping_bug"] or result["fixed_plan"]["mode"] == "fit" else 1


if __name__ == "__main__":
    raise SystemExit(main())
