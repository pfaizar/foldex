//! Installed language fonts are loaded only when visible text needs them.
//! Disk access stays off the UI thread; each context requests at most two fonts.
use egui::{Color32, Context, FontId, Galley, Id, IdMap};
use std::sync::Arc;

const MAX_TEXT_LAYOUTS: usize = 512;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const TEXT_CACHE_ID: &str = "foldex-simple-text-layouts";

#[derive(Clone)]
struct CachedText {
    galley: Arc<Galley>,
    seen: u64,
    bytes: usize,
}

#[derive(Clone, Default)]
struct TextCache {
    style: Option<(usize, u32)>,
    frame: Option<u64>,
    items: IdMap<CachedText>,
    bytes: usize,
    #[cfg(test)]
    layouts: usize,
}

impl TextCache {
    fn begin_frame(&mut self, style: (usize, u32), frame: u64) {
        if self.style != Some(style) {
            self.items.clear();
            self.bytes = 0;
            self.style = Some(style);
        }
        if self.frame != Some(frame) {
            self.frame = Some(frame);
            self.items.retain(|_, item| {
                let keep = frame.wrapping_sub(item.seen) <= 2;
                if !keep {
                    self.bytes -= item.bytes;
                }
                keep
            });
        }
    }

    fn insert(&mut self, id: Id, galley: Arc<Galley>, frame: u64) {
        let bytes = retained_bytes(&galley);
        // Do not evict this frame's visible labels to cache a stream of new
        // results or changing widths. Overflow still uses egui's normal cache.
        if self.items.len() >= MAX_TEXT_LAYOUTS || bytes > MAX_TEXT_BYTES - self.bytes {
            return;
        }
        if let Some(previous) = self.items.insert(
            id,
            CachedText {
                galley,
                seen: frame,
                bytes,
            },
        ) {
            self.bytes -= previous.bytes;
        }
        self.bytes += bytes;
    }
}

fn retained_bytes(galley: &Galley) -> usize {
    use egui::epaint::text::{Glyph, LayoutJob, LayoutSection, PlacedRow, Row};
    use std::mem::size_of;
    // Account for backing allocations, including elided input and spare mesh
    // capacity, rather than only the small Arc stored in the table.
    size_of::<Galley>()
        + size_of::<LayoutJob>()
        + galley.job.text.capacity()
        + galley.job.sections.capacity() * size_of::<LayoutSection>()
        + galley.rows.capacity() * size_of::<PlacedRow>()
        + galley
            .rows
            .iter()
            .map(|row| {
                size_of::<Row>()
                    + row.glyphs.capacity() * size_of::<Glyph>()
                    + row.visuals.mesh.vertices.capacity() * size_of::<egui::epaint::Vertex>()
                    + row.visuals.mesh.indices.capacity() * size_of::<u32>()
                    + 2 * size_of::<usize>()
            })
            .sum::<usize>()
        + 4 * size_of::<usize>()
}

/// Reuse simple immutable labels before allocating their String and section
/// vector. egui still performs all shaping, wrapping and elision on cache misses.
/// Placeholder-colored layouts can be painted in any hover/selection color.
pub(crate) fn layout(
    ctx: &Context,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    max_rows: usize,
) -> Arc<Galley> {
    // Match epaint's integer wrap-width normalization exactly; rounding here
    // avoids retaining equivalent layouts during subpixel width animation.
    let width = if width.is_finite() {
        width.round()
    } else {
        width
    };
    let frame = ctx.cumulative_frame_nr();
    let style = (revision(ctx), ctx.pixels_per_point().to_bits());
    let id = Id::new((text, &font, color, width.to_bits(), max_rows));
    let cached = ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<TextCache>(Id::new(TEXT_CACHE_ID));
        cache.begin_frame(style, frame);
        let item = cache.items.get_mut(&id)?;
        let job = &item.galley.job;
        // Validate content as well as the hash: a collision only costs a miss.
        if job.text != text
            || job.sections[0].format.font_id != font
            || job.sections[0].format.color != color
            || job.wrap.max_width.to_bits() != width.to_bits()
            || job.wrap.max_rows != max_rows
        {
            return None;
        }
        item.seen = frame;
        Some(item.galley.clone())
    });
    if let Some(cached) = cached {
        return cached;
    }
    request_text(ctx, text);
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width);
    job.wrap.max_rows = max_rows;
    job.wrap.break_anywhere = max_rows == 1;
    let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
    ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<TextCache>(Id::new(TEXT_CACHE_ID));
        #[cfg(test)]
        {
            cache.layouts += 1;
        }
        cache.insert(id, galley.clone(), frame);
    });
    galley
}

pub(crate) fn no_wrap(ctx: &Context, text: &str, font: FontId) -> Arc<Galley> {
    layout(
        ctx,
        text,
        font,
        Color32::PLACEHOLDER,
        f32::INFINITY,
        usize::MAX,
    )
}

/// Include this in retained galley keys. Font replacement, text option changes
/// and recycling a full atlas all invalidate retained glyph coordinates.
pub fn revision(ctx: &Context) -> usize {
    ctx.fonts(|fonts| fonts.atlas_revision())
}

pub fn request_text(ctx: &Context, text: &str) {
    #[cfg(windows)]
    {
        if text.is_ascii() {
            return;
        }
        let mut scripts = 0_u8;
        for character in text.chars() {
            match character as u32 {
                0x1100..=0x11ff | 0x3130..=0x318f | 0xa960..=0xa97f | 0xac00..=0xd7ff => {
                    scripts |= 2
                }
                0x2e80..=0xa4cf
                | 0xf900..=0xfaff
                | 0xfe30..=0xfe4f
                | 0xff00..=0xffef
                | 0x20000..=0x323af => scripts |= 1,
                _ => {}
            }
        }
        if scripts == 0 {
            return;
        }
        let pending = ctx.data_mut(|data| {
            let key = egui::Id::new("foldex-language-font-requests");
            let requested = data.get_temp::<u8>(key).unwrap_or(0);
            data.insert_temp(key, requested | scripts);
            scripts & !requested
        });
        if pending == 0 {
            return;
        }
        let context = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("foldex-language-fonts".into())
            .spawn(move || {
                let directory = std::env::var_os("WINDIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| "C:/Windows".into())
                    .join("Fonts");
                for (bit, name, candidates) in [
                    (
                        1,
                        "Foldex CJK fallback",
                        &["msyh.ttc", "YuGothR.ttc", "msgothic.ttc"][..],
                    ),
                    (
                        2,
                        "Foldex Hangul fallback",
                        &["malgun.ttf", "gulim.ttc"][..],
                    ),
                ] {
                    if pending & bit == 0 {
                        continue;
                    }
                    let Some(data) = candidates
                        .iter()
                        .find_map(|file| read_font(&directory.join(file)))
                    else {
                        continue;
                    };
                    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
                    context.add_font(FontInsert::new(
                        name,
                        egui::FontData::from_owned(data),
                        vec![
                            InsertFontFamily {
                                family: egui::FontFamily::Proportional,
                                priority: FontPriority::Lowest,
                            },
                            InsertFontFamily {
                                family: egui::FontFamily::Monospace,
                                priority: FontPriority::Lowest,
                            },
                        ],
                    ));
                }
                context.request_repaint();
            });
    }
    #[cfg(not(windows))]
    let _ = (ctx, text);
}

#[cfg(windows)]
fn read_font(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    const LIMIT: usize = 24 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 || length > LIMIT {
        return None;
    }
    let mut bytes = Vec::with_capacity(length);
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= LIMIT).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_stats(ctx: &Context) -> (usize, usize, usize) {
        ctx.data_mut(|data| {
            let cache = data.get_temp_mut_or_default::<TextCache>(Id::new(TEXT_CACHE_ID));
            (cache.items.len(), cache.bytes, cache.layouts)
        })
    }

    #[test]
    fn simple_labels_reuse_layouts_across_repaints_and_subpixel_widths() {
        let ctx = Context::default();
        let mut original = None;
        for frame in 0..8 {
            ctx.run_ui(Default::default(), |ui| {
                let galley = layout(
                    ui.ctx(),
                    "Unchanged menu label",
                    FontId::proportional(12.0),
                    Color32::PLACEHOLDER,
                    200.1 + frame as f32 * 0.01,
                    1,
                );
                if let Some(original) = &original {
                    assert!(Arc::ptr_eq(original, &galley));
                } else {
                    original = Some(galley.clone());
                }
                // Changing only the paint color must not build a new job.
                ui.painter().galley(
                    egui::pos2(20.0, 20.0),
                    galley,
                    if frame % 2 == 0 {
                        Color32::RED
                    } else {
                        Color32::GREEN
                    },
                );
            })
            .drop_without_applying_deltas();
        }
        assert_eq!(cache_stats(&ctx).2, 1, "one owned job for eight repaints");
    }

    #[test]
    fn simple_cache_preserves_egui_wrapping_newlines_and_elision() {
        let ctx = Context::default();
        ctx.run_ui(Default::default(), |ui| {
            for text in ["A label with several words", "naïve → file\nsecond line"] {
                for width in [48.2, 101.8, f32::INFINITY] {
                    for max_rows in [1, 2, usize::MAX] {
                        let font = FontId::proportional(12.0);
                        let cached = layout(
                            ui.ctx(),
                            text,
                            font.clone(),
                            Color32::WHITE,
                            width,
                            max_rows,
                        );
                        let mut job =
                            egui::text::LayoutJob::simple(text.into(), font, Color32::WHITE, width);
                        job.wrap.max_rows = max_rows;
                        job.wrap.break_anywhere = max_rows == 1;
                        let expected = ui.painter().layout_job(job);
                        assert_eq!(cached, expected);
                    }
                }
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn simple_cache_invalidates_after_atlas_and_dpi_changes() {
        let ctx = Context::default();
        let mut original = None;
        for (side, dpi, size) in [(2048, 1.0, 12.0), (1024, 1.0, 12.0), (2048, 2.0, 14.0)] {
            ctx.set_pixels_per_point(dpi);
            ctx.run_ui(
                egui::RawInput {
                    max_texture_side: Some(side),
                    ..Default::default()
                },
                |ui| {
                    let font = FontId::proportional(size);
                    let cached = no_wrap(ui.ctx(), "Same retained label", font.clone());
                    let expected = ui.painter().layout_no_wrap(
                        "Same retained label".into(),
                        font,
                        Color32::PLACEHOLDER,
                    );
                    assert_eq!(
                        cached, expected,
                        "cached glyphs must use the current atlas and DPI"
                    );
                    if let Some(original) = &original {
                        assert!(!Arc::ptr_eq(original, &cached));
                    }
                    original = Some(cached);
                },
            )
            .drop_without_applying_deltas();
        }
        assert_eq!(cache_stats(&ctx).2, 3);
    }

    #[test]
    fn simple_cache_bounds_visible_churn_and_retires_old_labels() {
        let ctx = Context::default();
        ctx.run_ui(Default::default(), |ui| {
            for index in 0..MAX_TEXT_LAYOUTS + 32 {
                no_wrap(
                    ui.ctx(),
                    &format!("menu entry {index}"),
                    FontId::proportional(12.0),
                );
            }
        })
        .drop_without_applying_deltas();
        let (items, bytes, _) = cache_stats(&ctx);
        assert!(items > 0 && items <= MAX_TEXT_LAYOUTS);
        assert!(bytes <= MAX_TEXT_BYTES);
        // Let egui's own one-pass cache expire too, as when another view is open.
        for _ in 0..3 {
            ctx.run_ui(Default::default(), |_| {})
                .drop_without_applying_deltas();
        }
        ctx.run_ui(Default::default(), |ui| {
            no_wrap(ui.ctx(), "new view", FontId::proportional(12.0));
        })
        .drop_without_applying_deltas();
        assert_eq!(cache_stats(&ctx).0, 1);
    }

    #[test]
    fn simple_cache_budget_includes_retained_glyph_and_mesh_allocations() {
        let ctx = Context::default();
        ctx.run_ui(Default::default(), |ui| {
            let galley = no_wrap(
                ui.ctx(),
                &"wide label ".repeat(100),
                FontId::proportional(12.0),
            );
            let mut cache = TextCache::default();
            cache.begin_frame((1, 1.0_f32.to_bits()), 0);
            for index in 0..MAX_TEXT_LAYOUTS {
                cache.insert(Id::new(index), galley.clone(), 0);
            }
            assert!(
                cache.items.len() < MAX_TEXT_LAYOUTS,
                "the byte budget must bind first"
            );
            assert!(cache.bytes <= MAX_TEXT_BYTES);
            assert!(cache.bytes + retained_bytes(&galley) > MAX_TEXT_BYTES);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn retained_glyphs_follow_atlas_recreation() {
        let ctx = Context::default();
        let mut cached: Option<(usize, Arc<egui::Galley>)> = None;
        let mut original = None;
        let mut revisions = Vec::new();
        for side in [2048, 1024, 2048] {
            let input = egui::RawInput {
                max_texture_side: Some(side),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                if original.is_none() {
                    // Allocate other glyphs first, as menus and other folders do.
                    let _ = ui.painter().layout_no_wrap(
                        "ABCDEFGHIJKLMNOP abcdefghijklmnop 0123456789".into(),
                        egui::FontId::proportional(27.0),
                        egui::Color32::WHITE,
                    );
                }
                let revision = revision(ui.ctx());
                revisions.push(revision);
                let fresh = ui.painter().layout_no_wrap(
                    "amd64_microsoft-windows.resources_31bf3856ad364e35".into(),
                    egui::FontId::proportional(13.0),
                    egui::Color32::WHITE,
                );
                if let Some(original) = &original {
                    assert!(original != &fresh, "The atlas reset must move the glyphs");
                } else {
                    original = Some(fresh.clone());
                }
                if cached.as_ref().is_none_or(|(old, _)| *old != revision) {
                    cached = Some((revision, fresh.clone()));
                }
                assert!(
                    cached.as_ref().unwrap().1 == fresh,
                    "Retained text points into a replaced atlas"
                );
                assert_eq!(revision, super::revision(ui.ctx()));
            });
            output.textures_delta.clear();
        }
        assert!(revisions.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn atlas_revision_tracks_recycling_without_invalidating_glyph_appends() {
        let ctx = Context::default();
        let mut before = 0;
        let mut filled = false;
        for frame in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    max_texture_side: Some(1024),
                    ..Default::default()
                },
                |ui| {
                    let current = revision(ui.ctx());
                    if frame == 0 {
                        before = current;
                        let text: String = ('!'..='~').collect();
                        for size in 10..=256 {
                            let _ = ui.painter().layout_no_wrap(
                                text.clone(),
                                egui::FontId::proportional(size as f32),
                                egui::Color32::WHITE,
                            );
                            assert_eq!(current, revision(ui.ctx()), "Appending glyphs is safe");
                            if ui.fonts(|fonts| fonts.font_atlas_fill_ratio()) > 0.8 {
                                filled = true;
                                break;
                            }
                        }
                        assert!(
                            filled,
                            "The workload must reach the real recycling threshold"
                        );
                    } else {
                        assert_ne!(current, before, "Recycling must invalidate retained glyphs");
                    }
                },
            );
            output.textures_delta.clear();
        }
    }
}
