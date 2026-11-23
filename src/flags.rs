//! Bundled flag atlas (`assets/flags_4x3.webp`).
//!
//! The atlas holds 195 flags at 48x36 px each, 16 columns, row-major.
//! One FIDE code per line in `assets/flags_4x3.txt` (Atlas order).
//!
//! `egui` 0.37 draws a regional-indicator pair such as `NO NO` as two
//! boxed letters, not as a flag glyph (`monochrome_emoji_fonts` does no
//! shaping), so flags are rendered as textures. Historical federations
//! without a modern flag (`URS`, `YUG`, `GDR`, `TCH`, `SCG`, `FID`) are
//! not in the atlas and fall back to a muted text pill of the same height.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{
    self, Color32, Context, FontId, Painter, Rect, Response, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, vec2,
};

use crate::country::Country;

/// Atlas layout: 16 columns, each flag 48x36 px.
const COLS: usize = 16;
const FLAG_PX_W: f32 = 48.0;
const FLAG_PX_H: f32 = 36.0;
/// Atlas pixel dimensions: 16 * 48 = 768, ceil(195 / 16) * 36 = 13 * 36 = 468.
const ATLAS_PX_W: f32 = 768.0;
const ATLAS_PX_H: f32 = 468.0;

/// Display size in list rows.
pub const LIST_SIZE: egui::Vec2 = vec2(16.0, 12.0);
/// Display size in the profile card.
pub const PROFILE_SIZE: egui::Vec2 = vec2(20.0, 15.0);
/// 1 px hairline so white flags stay visible on light surfaces (`#00000040`).
pub fn hairline() -> Color32 {
    Color32::from_rgba_unmultiplied(0, 0, 0, 64)
}

const ATLAS_BYTES: &[u8] = include_bytes!("../assets/flags_4x3.webp");
const CODES_TEXT: &str = include_str!("../assets/flags_4x3.txt");

thread_local! {
    static ATLAS_CACHE: RefCell<Option<(usize, TextureHandle)>> = const { RefCell::new(None) };
}

fn code_index() -> &'static HashMap<String, usize> {
    static MAP: OnceLock<HashMap<String, usize>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        // Sequential index over code lines only: leading `#` comment lines
        // must not shift the row-major atlas position.
        let mut index = 0usize;
        for line in CODES_TEXT.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            map.insert(trimmed.to_ascii_uppercase(), index);
            index += 1;
        }
        map
    })
}

/// Atlas index (row-major) for a FIDE code, if the flag is in the atlas.
pub fn index_for_code(code: &str) -> Option<usize> {
    code_index().get(&code.trim().to_ascii_uppercase()).copied()
}

/// Atlas index for a [`Country`].
pub fn index_for(country: Country) -> Option<usize> {
    index_for_code(country.code)
}

/// Alias: true when a flag image exists for this country.
pub fn has_flag(country: Country) -> bool {
    index_for(country).is_some()
}

/// Alias: true when a flag image exists for this code.
pub fn has(code: &str) -> bool {
    index_for_code(code).is_some()
}

/// Returns `true` when `code` has a bespoke vector flag painting for a
/// dissolved historical federation (`URS`, `YUG`, `GDR`, `TCH`, `SCG`).
pub fn has_former_flag(code: &str) -> bool {
    matches!(
        code.trim().to_ascii_uppercase().as_str(),
        "URS" | "YUG" | "GDR" | "TCH" | "SCG"
    )
}

/// Returns `true` when `country` renders as a 4:3 visual flag (either from
/// the WebP atlas or from the dissolved-federation vector painter) rather
/// than a wide fallback text pill.
pub fn has_visual_flag(country: Country) -> bool {
    has_flag(country) || has_former_flag(country.code)
}

/// UV rectangle in normalized 0..1 atlas space for a FIDE code.
pub fn uv_for_code(code: &str) -> Option<Rect> {
    let index = index_for_code(code)?;
    let col = (index % COLS) as f32;
    let row = (index / COLS) as f32;
    Some(Rect::from_min_max(
        egui::pos2(col * FLAG_PX_W / ATLAS_PX_W, row * FLAG_PX_H / ATLAS_PX_H),
        egui::pos2(
            (col + 1.0) * FLAG_PX_W / ATLAS_PX_W,
            (row + 1.0) * FLAG_PX_H / ATLAS_PX_H,
        ),
    ))
}

/// UV rectangle for a [`Country`]; `None` for historical federations.
pub fn uv_for(country: Country) -> Option<Rect> {
    uv_for_code(country.code)
}

/// Alias of [`uv_for`].
pub fn uv(country: Country) -> Option<Rect> {
    uv_for(country)
}

/// Alias of [`uv_for_code`].
pub fn uv_lookup(code: &str) -> Option<Rect> {
    uv_for_code(code)
}

fn texture_id(ctx_ptr: usize) -> Option<TextureHandle> {
    ATLAS_CACHE.with(|cache| {
        if let Some((ptr, handle)) = &*cache.borrow()
            && *ptr == ctx_ptr
        {
            return Some(handle.clone());
        }
        None
    })
}

fn store_texture(ctx_ptr: usize, handle: &TextureHandle) {
    ATLAS_CACHE.with(|cache| {
        *cache.borrow_mut() = Some((ctx_ptr, handle.clone()));
    });
}

/// Load (or reuse) the atlas texture for this context.
///
/// Decodes the bundled WebP once via `image::load_from_memory` and uploads
/// it as `brett-flags` with linear sampling.
pub fn texture(ctx: &Context) -> Option<TextureHandle> {
    let ptr = ctx as *const Context as usize;
    if let Some(handle) = texture_id(ptr) {
        return Some(handle);
    }
    let image = image::load_from_memory(ATLAS_BYTES).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = image.into_raw();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    let handle = ctx.load_texture("brett-flags", color_image, TextureOptions::LINEAR);
    store_texture(ptr, &handle);
    Some(handle)
}

/// Alias of [`texture`].
pub fn atlas(ctx: &Context) -> Option<TextureHandle> {
    texture(ctx)
}

/// Alias of [`texture`].
pub fn load(ctx: &Context) -> Option<TextureHandle> {
    texture(ctx)
}

/// Alias of [`texture`].
pub fn ensure_loaded(ctx: &Context) -> Option<TextureHandle> {
    texture(ctx)
}

/// Draw the flag image for `country` into `rect` on a painter.
///
/// Returns the horizontal space consumed (flag width, or pill width for
/// historical codes). The flag carries a 1 px hairline so white flags
/// (for example Japan) stay visible on light surfaces.
pub fn paint_flag(painter: &Painter, country: Country, rect: Rect) -> f32 {
    if let Some(uv) = uv_for(country)
        && let Some(handle) = texture(painter.ctx())
    {
        painter.image(handle.id(), rect, uv, Color32::WHITE);
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, hairline()), StrokeKind::Inside);
        return rect.width();
    }
    if paint_former_flag(painter, country.code, rect) {
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, hairline()), StrokeKind::Inside);
        return rect.width();
    }
    // Historical federation without an atlas image: muted text pill,
    // same height as the flag image.
    paint_pill(painter, country, rect)
}

/// Paint the 1933–1935 German war ensign without a swastika for the
/// historically specific Keres federation tooltip.
///
/// This uses the requested black-white-red field and a small hoist-side Iron
/// Cross treatment; it intentionally omits the central emblem.
pub fn paint_german_war_ensign(painter: &Painter, rect: Rect) {
    let stripe_h = rect.height() / 3.0;
    for (index, color) in [
        Color32::BLACK,
        Color32::WHITE,
        Color32::from_rgb(0xD0, 0x1C, 0x1F),
    ]
    .into_iter()
    .enumerate()
    {
        painter.rect_filled(
            Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + stripe_h * index as f32),
                vec2(rect.width(), stripe_h),
            ),
            0.0,
            color,
        );
    }

    // A compact outlined cross keeps the historical ensign recognizable at
    // list-icon scale without reproducing the prohibited emblem.
    let cross_size = rect.height() * 0.48;
    let center = egui::pos2(rect.left() + rect.width() * 0.29, rect.center().y);
    let iron_cross = |scale: f32| {
        let arm = cross_size * scale;
        let half = arm * 0.5;
        let neck = arm * 0.17;
        vec![
            egui::pos2(center.x - neck, center.y - half),
            egui::pos2(center.x + neck, center.y - half),
            egui::pos2(center.x + neck, center.y - neck),
            egui::pos2(center.x + half, center.y - neck),
            egui::pos2(center.x + half, center.y + neck),
            egui::pos2(center.x + neck, center.y + neck),
            egui::pos2(center.x + neck, center.y + half),
            egui::pos2(center.x - neck, center.y + half),
            egui::pos2(center.x - neck, center.y + neck),
            egui::pos2(center.x - half, center.y + neck),
            egui::pos2(center.x - half, center.y - neck),
            egui::pos2(center.x - neck, center.y - neck),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        iron_cross(1.0),
        Color32::WHITE,
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        iron_cross(0.76),
        Color32::BLACK,
        Stroke::NONE,
    ));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, hairline()), StrokeKind::Inside);
}

fn paint_star(painter: &Painter, center: egui::Pos2, outer_r: f32, inner_r: f32, fill: Color32) {
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(center, fill);
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * (std::f32::consts::PI / 5.0);
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        let pt = egui::pos2(center.x + r * angle.cos(), center.y + r * angle.sin());
        mesh.colored_vertex(pt, fill);
    }
    for i in 0..10u32 {
        let next = (i + 1) % 10;
        mesh.add_triangle(0, 1 + i, 1 + next);
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn paint_tricolor(painter: &Painter, rect: Rect, colors: [Color32; 3]) {
    let h = rect.height() / 3.0;
    for (i, color) in colors.into_iter().enumerate() {
        let r = Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y + h * i as f32),
            vec2(rect.width(), h),
        );
        painter.rect_filled(r, 0.0, color);
    }
}

/// Vector flag paintings for dissolved federations, drawn at 4:3 flag size
/// so historical federations read as flags rather than wide text pills.
fn paint_former_flag(painter: &Painter, code: &str, rect: Rect) -> bool {
    match code.trim().to_ascii_uppercase().as_str() {
        "URS" => {
            // Crimson Soviet field + gold star and crossed hammer & sickle in the canton.
            painter.rect_filled(rect, 0.0, Color32::from_rgb(0xC8, 0x10, 0x2E));
            let gold = Color32::from_rgb(0xFF, 0xD7, 0x00);
            let w = rect.width();
            let h = rect.height();
            let stroke_w = (h * 0.085).max(1.0);
            let star_center = egui::pos2(rect.left() + w * 0.24, rect.top() + h * 0.18);
            let star_r = (h * 0.095).max(1.2);
            paint_star(painter, star_center, star_r, star_r * 0.42, gold);

            let arc_center = egui::pos2(rect.left() + w * 0.23, rect.top() + h * 0.37);
            let arc_r = (h * 0.13).max(1.5);
            let mut sickle_pts = Vec::with_capacity(7);
            for step in 0..=6 {
                let t = step as f32 / 6.0;
                let angle = -2.25 + t * 3.55;
                sickle_pts.push(egui::pos2(
                    arc_center.x + arc_r * angle.cos(),
                    arc_center.y + arc_r * angle.sin(),
                ));
            }
            painter.add(egui::Shape::line(sickle_pts, Stroke::new(stroke_w, gold)));
            painter.line_segment(
                [
                    egui::pos2(rect.left() + w * 0.14, rect.top() + h * 0.46),
                    egui::pos2(rect.left() + w * 0.30, rect.top() + h * 0.30),
                ],
                Stroke::new(stroke_w, gold),
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + w * 0.26, rect.top() + h * 0.27),
                    egui::pos2(rect.left() + w * 0.33, rect.top() + h * 0.33),
                ],
                Stroke::new((stroke_w * 1.25).max(1.2), gold),
            );
            true
        }
        "YUG" => {
            paint_tricolor(
                painter,
                rect,
                [
                    Color32::from_rgb(0x0C, 0x3C, 0x7C),
                    Color32::from_rgb(0xF1, 0xF1, 0xF1),
                    Color32::from_rgb(0xE6, 0x3E, 0x2A),
                ],
            );
            let outer = (rect.height() * 0.24).max(2.6);
            paint_star(
                painter,
                rect.center(),
                outer,
                outer * 0.40,
                Color32::from_rgb(0xFC, 0xD1, 0x16),
            );
            let inner = outer * 0.78;
            paint_star(
                painter,
                rect.center(),
                inner,
                inner * 0.40,
                Color32::from_rgb(0xD8, 0x1E, 0x28),
            );
            true
        }
        "GDR" => {
            paint_tricolor(
                painter,
                rect,
                [
                    Color32::from_rgb(0x00, 0x00, 0x00),
                    Color32::from_rgb(0xDD, 0x00, 0x00),
                    Color32::from_rgb(0xFF, 0xCC, 0x00),
                ],
            );
            let gold = Color32::from_rgb(0xFF, 0xCC, 0x00);
            let r = (rect.height() * 0.16).max(1.8);
            painter.circle_filled(rect.center(), r, gold);
            painter.circle_filled(
                rect.center(),
                (r * 0.55).max(0.9),
                Color32::from_rgb(0xDD, 0x00, 0x00),
            );
            painter.circle_stroke(
                rect.center(),
                r,
                Stroke::new((rect.height() * 0.06).max(0.9), gold),
            );
            true
        }
        "TCH" => {
            let half_h = rect.height() * 0.5;
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(rect.width(), half_h)),
                0.0,
                Color32::from_rgb(0xF1, 0xF1, 0xF1),
            );
            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.min.y + half_h),
                    vec2(rect.width(), rect.height() - half_h),
                ),
                0.0,
                Color32::from_rgb(0xD7, 0x14, 0x1A),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    rect.left_top(),
                    egui::pos2(rect.left() + rect.width() * 0.5, rect.center().y),
                    rect.left_bottom(),
                ],
                Color32::from_rgb(0x11, 0x45, 0x7E),
                Stroke::NONE,
            ));
            true
        }
        "SCG" => {
            paint_tricolor(
                painter,
                rect,
                [
                    Color32::from_rgb(0x0C, 0x3C, 0x7C),
                    Color32::from_rgb(0xF1, 0xF1, 0xF1),
                    Color32::from_rgb(0xE6, 0x3E, 0x2A),
                ],
            );
            true
        }
        _ => false,
    }
}

/// Alias of [`paint_flag`].
pub fn paint(painter: &Painter, country: Country, rect: Rect) -> f32 {
    paint_flag(painter, country, rect)
}

/// Alias of [`paint_flag`].
pub fn paint_flag_painter(painter: &Painter, country: Country, rect: Rect) -> f32 {
    paint_flag(painter, country, rect)
}

fn paint_pill(painter: &Painter, country: Country, rect: Rect) -> f32 {
    let font = FontId::proportional(9.0);
    let text_color = Color32::from_rgb(0x9A, 0xA3, 0xB2);
    // Measure the code to size the pill; always at least the flag height.
    let galley = painter.layout_no_wrap(country.code.to_string(), font.clone(), text_color);
    let pill_w = (galley.size().x + 10.0).max(rect.width());
    let pill_rect = Rect::from_min_size(rect.min, vec2(pill_w, rect.height()));
    painter.rect_filled(pill_rect, 3.0, Color32::from_rgb(0x2D, 0x37, 0x48));
    painter.rect_stroke(
        pill_rect,
        3.0,
        Stroke::new(1.0, Color32::from_rgb(0x26, 0x2B, 0x35)),
        StrokeKind::Inside,
    );
    // Re-layout centred inside the pill.
    let galley = painter.layout_no_wrap(country.code.to_string(), font, text_color);
    painter.galley(pill_rect.center() - galley.size() * 0.5, galley, text_color);
    pill_w
}

/// Widget painter: allocate `size` and paint the flag into it.
///
/// Historical codes without an atlas image fall back to a muted text pill
/// of the same height; the allocation is widened to fit it so the following
/// label never overlaps. The returned response carries hover text
/// `"{Name} ({CODE})"`.
pub fn flag(ui: &mut Ui, country: Country, size: egui::Vec2) -> Response {
    let alloc = if has_visual_flag(country) {
        size
    } else {
        // Measure the pill width the same way `paint_pill` does.
        let galley = ui.painter().layout_no_wrap(
            country.code.to_string(),
            FontId::proportional(9.0),
            Color32::from_rgb(0x9A, 0xA3, 0xB2),
        );
        let pill_w = (galley.size().x + 10.0).max(size.x);
        vec2(pill_w, size.y)
    };
    let (_, response) = ui.allocate_exact_size(alloc, Sense::hover());
    let painter = ui.painter_at(response.rect);
    let consumed = paint_flag(&painter, country, response.rect);
    let _ = consumed;
    response.on_hover_text(format!("{} ({})", country.name, country.code))
}

/// Alias of [`flag`].
pub fn flag_response(ui: &mut Ui, country: Country, size: egui::Vec2) -> Response {
    flag(ui, country, size)
}

/// Alias of [`flag`].
pub fn paint_flag_response(ui: &mut Ui, country: Country, size: egui::Vec2) -> Response {
    flag(ui, country, size)
}

/// Alias of [`flag`].
pub fn paint_flag_widget(ui: &mut Ui, country: Country, size: egui::Vec2) -> Response {
    flag(ui, country, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_lists_every_flag_exactly_once() {
        assert_eq!(code_index().len(), 195, "atlas must hold 195 flags");
    }

    #[test]
    fn atlas_indices_start_at_zero_and_stay_in_range() {
        // `flags_4x3.txt` leads with `#` comment lines; they must not shift
        // the row-major position of the first/last flag.
        assert_eq!(index_for_code("AFG"), Some(0));
        assert_eq!(index_for_code("ALB"), Some(1));
        assert_eq!(index_for_code("NOR"), Some(132));
        assert_eq!(index_for_code("FRG"), Some(194));
        assert!(
            code_index().values().all(|&index| index < 195),
            "every index must fit the 13-row atlas"
        );
    }

    #[test]
    fn uv_lookup_covers_modern_flags() {
        for code in ["AFG", "NED", "NOR", "USA", "CAN", "GBR", "FRG"] {
            let uv = uv_for_code(code).unwrap_or_else(|| panic!("{code} must be in atlas"));
            assert!(
                uv.width() > 0.0 && uv.height() > 0.0,
                "{code} uv degenerate"
            );
            assert!(
                uv.min.x >= 0.0 && uv.max.x <= 1.0 && uv.min.y >= 0.0 && uv.max.y <= 1.0,
                "{code} uv out of range"
            );
            assert!(has(code));
        }
    }

    #[test]
    fn historical_federations_have_no_flag_image() {
        for code in ["URS", "YUG", "GDR", "TCH", "SCG", "FID"] {
            assert_eq!(uv_for_code(code), None, "{code} must not be in atlas");
            assert!(!has(code));
        }
    }

    #[test]
    fn dissolved_federations_render_as_visual_flags_at_4_3() {
        for code in ["URS", "YUG", "GDR", "TCH", "SCG"] {
            assert!(
                has_former_flag(code),
                "{code} must have a vector former-flag painting"
            );
            let country = crate::country::normalize(code).expect("historical country");
            assert!(
                has_visual_flag(country),
                "{code} must allocate 4:3 flag geometry instead of wide pill"
            );
        }
        assert!(!has_former_flag("FID"));
        let fid = crate::country::normalize("FID").expect("FIDE country");
        assert!(!has_visual_flag(fid));
    }

    #[test]
    fn list_and_profile_sizes_have_flag_aspect() {
        // 4:3 flags: width / height must stay 4/3 at both display sizes.
        for size in [LIST_SIZE, PROFILE_SIZE] {
            assert!(
                (size.x / size.y - 4.0 / 3.0).abs() < 0.001,
                "size {size:?} must keep 4:3"
            );
        }
        assert_eq!(LIST_SIZE, vec2(16.0, 12.0));
        assert_eq!(PROFILE_SIZE, vec2(20.0, 15.0));
    }

    #[test]
    fn row_major_uv_shares_rows_across_columns() {
        let afg = uv_for_code("AFG").unwrap();
        let alb = uv_for_code("ALB").unwrap();
        let beh = uv_for_code("BEL").unwrap();
        assert!((alb.min.y - afg.min.y).abs() < 1e-6, "first row shares y");
        assert!(alb.min.x > afg.min.x, "second column is right of first");
        assert!(beh.min.y > afg.min.y, "later row is lower");
    }
}
