//! View geometry derived from File Pilot's recovered helper 0x1400cfaa0.
//!
//! `docs/file-pilot-motion-evidence.md` and the retained REA record establish the six
//! stops, row-major layout, icon interpolation, proportional padding and width sharing.
//! The original small-font/UI-metric conversion and caption measurement remain unknown.
//! Here 12 logical font pixels map to the source's 9-point metric unit; caption height
//! approximates the recovered zoom-dependent font call. Fractional coordinates avoid
//! introducing integer rounding steps before the renderer applies its own pixel scaling.
use egui::{Rect, Vec2, pos2, vec2};

pub const ZOOM_STOPS: [f32; 6] = [0.0, 0.1, 0.2, 0.3, 0.65, 1.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Details,
    List,
    Columns,
    MediumIcons,
    LargeIcons,
    ExtraLargeIcons,
}

const VIEW_STOPS: [(ViewMode, f32); 6] = [
    (ViewMode::Details, 0.0),
    (ViewMode::List, 0.1),
    (ViewMode::Columns, 0.2),
    (ViewMode::MediumIcons, 0.3),
    (ViewMode::LargeIcons, 0.65),
    (ViewMode::ExtraLargeIcons, 1.0),
];

impl ViewMode {
    /// Source thresholds select the preceding stop, rather than the nearest stop.
    pub fn from_zoom(zoom: f32) -> Self {
        let zoom = normalized_zoom(zoom);
        if zoom < 0.1 {
            Self::Details
        } else if zoom < 0.2 {
            Self::List
        } else if zoom < 0.3 {
            Self::Columns
        } else if zoom < 0.65 {
            Self::MediumIcons
        } else if zoom < 1.0 {
            Self::LargeIcons
        } else {
            Self::ExtraLargeIcons
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Details => "Details",
            Self::List => "List",
            Self::Columns => "Columns",
            Self::MediumIcons => "M Icons",
            Self::LargeIcons => "L Icons",
            Self::ExtraLargeIcons => "XL Icons",
        }
    }
    pub const fn icon(self) -> crate::Icon {
        match self {
            Self::Details => crate::Icon::List,
            Self::List => crate::Icon::Menu,
            Self::Columns => crate::Icon::Columns,
            _ => crate::Icon::Picture,
        }
    }
    pub const fn zoom(self) -> f32 {
        match self {
            Self::Details => 0.0,
            Self::List => 0.1,
            Self::Columns => 0.2,
            Self::MediumIcons => 0.3,
            Self::LargeIcons => 0.65,
            Self::ExtraLargeIcons => 1.0,
        }
    }
    pub const fn stops() -> &'static [(Self, f32); 6] {
        &VIEW_STOPS
    }
    pub const fn is_grid(self) -> bool {
        matches!(
            self,
            Self::MediumIcons | Self::LargeIcons | Self::ExtraLargeIcons
        )
    }
    pub const fn is_compact(self) -> bool {
        !self.is_grid()
    }
}

fn normalized_zoom(zoom: f32) -> f32 {
    if zoom.is_nan() {
        0.0
    } else {
        zoom.clamp(0.0, 1.0)
    }
}
fn positive_metric(value: f32, fallback: f32, max: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(1.0, max)
    } else {
        fallback
    }
}
fn nonnegative_metric(value: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, max)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ZoomMetrics {
    pub zoom: f32,
    pub mode: ViewMode,
    /// 16 for compact rows; 48, 152 and 256 at the three icon stops.
    pub nominal_icon_size: f32,
    /// Nominal size scaled with the font, following the recovered source behavior.
    pub icon_size: f32,
    /// Reconstruction mapping: source option 9 points corresponds to a 12-pixel font.
    pub metric_unit: f32,
}
impl ZoomMetrics {
    pub fn new(zoom: f32, font_size: f32) -> Self {
        let zoom = normalized_zoom(zoom);
        let font_size = positive_metric(font_size, 12.0, 256.0);
        let nominal_icon_size = if zoom < 0.3 {
            16.0
        } else {
            48.0 + 208.0 * ((zoom - 0.3) / 0.7).clamp(0.0, 1.0)
        };
        Self {
            zoom,
            mode: ViewMode::from_zoom(zoom),
            nominal_icon_size,
            icon_size: (nominal_icon_size * (font_size / 12.0)).max(16.0),
            metric_unit: font_size * 0.75,
        }
    }
}

/// Content-local row-major geometry. File spacing is independent of control/panel spacing.
/// `row_height` excludes the vertical gap; pass zero spacing if it already includes that gap.
/// Column counts change discretely; retain item identities in LayoutMotion when reflowing.
#[derive(Clone, Copy, Debug)]
pub struct GridMetrics {
    pub mode: ViewMode,
    pub columns: usize,
    pub item_size: Vec2,
    pub gap: Vec2,
    /// Fits the available cell width even while a pane closes or becomes very narrow.
    pub icon_size: f32,
}
impl GridMetrics {
    pub fn new(width: f32, font_size: f32, row_height: f32, spacing: f32, zoom: f32) -> Self {
        let width = nonnegative_metric(width, 1_000_000.0);
        let row_height = positive_metric(row_height, 26.0, 16_384.0);
        let spacing = nonnegative_metric(spacing, 4096.0);
        let zoom = ZoomMetrics::new(zoom, font_size);
        let unit = zoom.metric_unit;
        if matches!(zoom.mode, ViewMode::Details | ViewMode::List) {
            return Self {
                mode: zoom.mode,
                columns: 1,
                item_size: vec2(width, row_height),
                gap: vec2(spacing, spacing),
                icon_size: zoom.icon_size.min(width).min(row_height),
            };
        }
        if zoom.mode == ViewMode::Columns {
            let gap = vec2(spacing + 1.4 * unit, spacing);
            let nominal_width = (gap.x + 30.0 * unit).min(width).max(1.0);
            let columns = (width / nominal_width).floor().max(1.0) as usize;
            let cell_width = (width / columns as f32 - gap.x).max(0.0);
            return Self {
                mode: zoom.mode,
                columns,
                item_size: vec2(cell_width, row_height),
                gap,
                icon_size: zoom.icon_size.min(cell_width).min(row_height),
            };
        }

        // The recovered helper adds 1.4 units vertically and .7 horizontally to the
        // base gaps, uses two .5-unit horizontal pads, and shares viewport width.
        let gap = vec2(spacing + 0.7 * unit, spacing + 1.4 * unit);
        let nominal_width = (zoom.icon_size + unit + gap.x).min(width).max(1.0);
        let columns = (width / nominal_width).floor().max(1.0) as usize;
        let cell_width = (width / columns as f32 - gap.x).max(0.0);
        // Exact text shaping/line wrapping was not recovered. This caption term fits the
        // observed default-font heights without claiming the original font implementation.
        let caption = (1.0 + 0.8 * zoom.zoom) * unit;
        let height = zoom.icon_size + (1.8 + 1.4 + 2.0 * 0.5 + 0.2) * unit + caption;
        Self {
            mode: zoom.mode,
            columns,
            item_size: vec2(cell_width, height),
            gap,
            icon_size: zoom.icon_size.min((cell_width - unit).max(0.0)),
        }
    }
    pub fn count_rows(self, count: usize) -> usize {
        count.div_ceil(self.columns.max(1))
    }
    pub fn stride(self) -> Vec2 {
        self.item_size + self.gap
    }
    pub fn rect_for(self, index: usize) -> Rect {
        let columns = self.columns.max(1);
        let stride = self.stride();
        Rect::from_min_size(
            pos2(
                (index % columns) as f32 * stride.x,
                (index / columns) as f32 * stride.y,
            ),
            self.item_size,
        )
    }
    pub fn total_height(self, count: usize) -> f32 {
        let rows = self.count_rows(count);
        if rows == 0 {
            0.0
        } else {
            rows as f32 * self.item_size.y + rows.saturating_sub(1) as f32 * self.gap.y
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_stops_and_threshold_labels_are_preserved() {
        for &(mode, stop) in ViewMode::stops() {
            assert_eq!(mode, ViewMode::from_zoom(stop));
            assert_eq!(mode.zoom(), stop);
            assert!(!mode.label().is_empty());
        }
        assert_eq!(ViewMode::from_zoom(0.2999), ViewMode::Columns);
        assert_eq!(ViewMode::from_zoom(0.64), ViewMode::MediumIcons);
        assert_eq!(ViewMode::from_zoom(0.999), ViewMode::LargeIcons);
        assert_eq!(ViewMode::from_zoom(f32::NAN), ViewMode::Details);
        assert_eq!(
            ViewMode::from_zoom(f32::INFINITY),
            ViewMode::ExtraLargeIcons
        );
    }
    #[test]
    fn nominal_icons_follow_recovered_continuous_interpolation() {
        for (zoom, expected) in [(0.2, 16.0), (0.3, 48.0), (0.65, 152.0), (1.0, 256.0)] {
            let metrics = ZoomMetrics::new(zoom, 12.0);
            assert!((metrics.nominal_icon_size - expected).abs() < 0.001);
            assert!((metrics.icon_size - expected).abs() < 0.001);
        }
        let before = ZoomMetrics::new(0.5, 12.0).icon_size;
        let after = ZoomMetrics::new(0.5001, 12.0).icon_size;
        assert!(after > before && after - before < 0.04);
    }
    #[test]
    fn geometry_tracks_the_source_default_font_observations() {
        let medium = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.3);
        let large = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.65);
        let extra = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 1.0);
        assert_eq!((medium.columns, large.columns, extra.columns), (11, 4, 2));
        for (metrics, measured) in [
            (medium, vec2(60.0, 100.0)),
            (large, vec2(188.0, 205.0)),
            (extra, vec2(390.0, 310.0)),
        ] {
            assert!((metrics.item_size.x - measured.x).abs() < 4.0);
            assert!((metrics.item_size.y - measured.y).abs() < 4.0);
        }
    }
    #[test]
    fn compact_columns_are_row_major_with_exact_row_counts() {
        let metrics = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.2);
        assert_eq!(metrics.columns, 2);
        assert_eq!(metrics.count_rows(0), 0);
        assert_eq!(metrics.count_rows(5), 3);
        assert_eq!(metrics.rect_for(0).top(), metrics.rect_for(1).top());
        assert!(metrics.rect_for(2).top() > metrics.rect_for(1).top());
        assert_eq!(metrics.rect_for(2).left(), metrics.rect_for(0).left());
        assert_eq!(metrics.total_height(0), 0.0);
        assert_eq!(metrics.total_height(1), 26.0);
        assert!(metrics.rect_for(5).right() <= 805.001);
    }
    #[test]
    fn finite_narrow_geometry_and_fractional_reflow_never_create_negative_sizes() {
        for width in [
            f32::NAN,
            f32::INFINITY,
            -100.0,
            0.0,
            0.01,
            1.0,
            12.0,
            200.0,
            805.0,
        ] {
            for &zoom in &ZOOM_STOPS {
                let metrics = GridMetrics::new(width, f32::NAN, -1.0, f32::NAN, zoom);
                for value in [
                    metrics.item_size.x,
                    metrics.item_size.y,
                    metrics.gap.x,
                    metrics.gap.y,
                    metrics.icon_size,
                    metrics.total_height(1_000_000),
                ] {
                    assert!(value.is_finite() && value >= 0.0, "{metrics:?}");
                }
                assert!(metrics.columns >= 1);
                let rect = metrics.rect_for(1_000_000);
                assert!(rect.min.is_finite() && rect.max.is_finite());
            }
        }
        let a = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.65);
        let b = GridMetrics::new(805.1, 12.0, 26.0, 8.0, 0.65001);
        assert_eq!(a.columns, b.columns);
        assert!((a.item_size.x - b.item_size.x).abs() < 0.1);
        assert!((a.item_size.y - b.item_size.y).abs() < 0.1);
    }
}
