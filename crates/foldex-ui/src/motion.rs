//! Retargetable motion using the four interpolation gains recovered through REA.
//!
//! Source helper 0x140176a20 uses `1 - 2^(-rate * dt)` at rates 50/70/30/20.
//! These define error half-lives, not fixed animation durations. The legacy 145 ms theme
//! token is a speed scale: 145 ms preserves those rates; doubling it halves them.
//! Zero duration is reduced motion. Layout retains at most 512 visible identities.
use egui::{Id, IdMap, Rect, Vec2, vec2};
use std::collections::hash_map::Entry;

pub const SOURCE_RATES: [f32; 4] = [50.0, 70.0, 30.0, 20.0];
pub const LEGACY_REFERENCE_DURATION: f32 = 0.145;
pub const PIXEL_SNAP: f32 = 0.25;
pub const NORMALIZED_SNAP: f32 = 0.01;
pub const MAX_MOTION_ITEMS: usize = 512;

/// Container geometry follows direct manipulation; hover and other state motion continue.
pub fn set_direct_layout(ctx: &egui::Context, direct: bool) {
    ctx.data_mut(|data| data.insert_temp(Id::new("foldex-direct-layout"), direct));
}
pub fn direct_layout(ctx: &egui::Context) -> bool {
    ctx.data(|data| {
        data.get_temp::<bool>(Id::new("foldex-direct-layout"))
            .unwrap_or(false)
    })
}
pub fn layout_gains(ctx: &egui::Context, duration: f32) -> MotionGains {
    let direct = direct_layout(ctx);
    MotionGains::new(
        ctx.input(|input| input.stable_dt),
        if direct { 0.0 } else { duration },
    )
}

pub fn set_interactive_resize(ctx: &egui::Context, resizing: bool) {
    ctx.data_mut(|data| data.insert_temp(Id::new("foldex-interactive-resize"), resizing));
}
pub fn interactive_resize(ctx: &egui::Context) -> bool {
    ctx.data(|data| {
        data.get_temp::<bool>(Id::new("foldex-interactive-resize"))
            .unwrap_or(false)
    })
}
pub fn popup_gains(ctx: &egui::Context, duration: f32) -> MotionGains {
    let resizing = interactive_resize(ctx);
    MotionGains::new(
        ctx.input(|input| input.stable_dt),
        if resizing { 0.0 } else { duration },
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub enum MotionProfile {
    #[default]
    Standard = 0,
    Fast = 1,
    Slow = 2,
    Slowest = 3,
}
impl MotionProfile {
    pub const fn rate(self) -> f32 {
        SOURCE_RATES[self as usize]
    }
    pub const fn from_selector(selector: usize) -> Self {
        match selector {
            1 => Self::Fast,
            2 => Self::Slow,
            3 => Self::Slowest,
            _ => Self::Standard,
        }
    }
}

/// Compute once per frame, then reuse for positions, dimensions and normalized state.
#[derive(Clone, Copy, Debug)]
pub struct MotionGains {
    gains: [f32; 4],
    immediate: bool,
}
impl MotionGains {
    pub fn new(dt: f32, duration: f32) -> Self {
        if !duration.is_finite() || duration <= f32::EPSILON {
            return Self {
                gains: [1.0; 4],
                immediate: true,
            };
        }
        let dt = if dt.is_nan() || dt < 0.0 {
            0.0
        } else {
            dt.min(1.0)
        };
        // The theme validates a much smaller range. This protects external callers from
        // a huge duration rounding every gain to zero and creating an asymptotic idle loop.
        let scale = LEGACY_REFERENCE_DURATION / duration.min(10.0);
        let gains = SOURCE_RATES.map(|rate| (1.0 - (-rate * dt * scale).exp2()).clamp(0.0, 1.0));
        Self {
            gains,
            immediate: false,
        }
    }
    pub fn gain(self, profile: MotionProfile) -> f32 {
        self.gains[profile as usize]
    }
    pub fn is_immediate(self) -> bool {
        self.immediate
    }
    pub fn step_pixels(self, value: &mut f32, target: f32, profile: MotionProfile) -> bool {
        self.step(value, target, profile, PIXEL_SNAP)
    }
    pub fn step_normalized(self, value: &mut f32, target: f32, profile: MotionProfile) -> bool {
        *value = if value.is_finite() {
            (*value).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let target = if target.is_finite() {
            target.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.step(value, target, profile, NORMALIZED_SNAP)
    }
    /// Returns true only while another frame is needed. `epsilon` uses caller coordinates.
    pub fn step(self, value: &mut f32, target: f32, profile: MotionProfile, epsilon: f32) -> bool {
        if !target.is_finite() {
            if !value.is_finite() {
                *value = 0.0;
            }
            return false;
        }
        let epsilon = if epsilon.is_finite() && epsilon > 0.0 {
            epsilon
        } else {
            PIXEL_SNAP
        };
        if self.immediate || !value.is_finite() || (*value - target).abs() <= epsilon {
            *value = target;
            return false;
        }
        let gain = self.gain(profile);
        let previous = *value;
        // Weighted interpolation avoids overflow from subtracting opposite large values.
        let next = previous * (1.0 - gain) + target * gain;
        let next = if next.is_finite() {
            next.clamp(previous.min(target), previous.max(target))
        } else {
            next
        };
        if !next.is_finite() || (next - target).abs() <= epsilon || (gain > 0.0 && next == previous)
        {
            // Deep virtual lists can have content coordinates whose f32 spacing exceeds
            // .25px. A rounded-away final step must not sustain redraws forever.
            *value = target;
            false
        } else {
            *value = next;
            true
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VisualPlacement {
    pub rect: Rect,
    pub opacity: f32,
    pub moving: bool,
}
#[derive(Clone, Copy)]
struct Item {
    rect: Rect,
    opacity: f32,
    seen: u64,
}

/// Geometry only, in content coordinates: ordinary scrolling must not animate each file.
/// New-item 6-point offset/fade is a restrained reconstruction choice, not recovered timing.
pub struct LayoutMotion {
    items: IdMap<Item>,
    frame: u64,
    capacity: usize,
    gains: MotionGains,
    pixels_per_point: f32,
    moving: bool,
}
impl Default for LayoutMotion {
    fn default() -> Self {
        Self::new(MAX_MOTION_ITEMS)
    }
}
impl LayoutMotion {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_MOTION_ITEMS);
        Self {
            // Dormant tabs allocate no viewport-sized animation table.
            // Grow only for items actually drawn; the cap remains fixed.
            items: IdMap::default(),
            frame: 0,
            capacity,
            gains: MotionGains::new(0.0, 0.0),
            pixels_per_point: 1.0,
            moving: false,
        }
    }
    pub fn clear(&mut self) {
        self.items.clear();
        self.moving = false;
    }
    pub fn set_pixels_per_point(&mut self, value: f32) {
        self.pixels_per_point = if value.is_finite() && value > 0.0 {
            value.clamp(0.25, 16.0)
        } else {
            1.0
        };
    }
    pub fn begin_frame(&mut self, dt: f32, duration: f32) {
        self.begin_frame_with_gains(MotionGains::new(dt, duration));
    }
    pub fn begin_frame_with_gains(&mut self, gains: MotionGains) {
        if self.frame == u64::MAX {
            self.items.clear();
            self.frame = 0;
        }
        self.frame += 1;
        self.gains = gains;
        self.moving = false;
        let earliest = self.frame.saturating_sub(2);
        self.items.retain(|_, item| item.seen >= earliest);
    }
    pub fn place(&mut self, id: Id, target: Rect, appear: bool) -> VisualPlacement {
        self.place_with_profile(id, target, appear, MotionProfile::Standard)
    }
    /// A result arriving from outside the viewport appears near its destination.
    /// Visible neighbours still reorder from their existing positions.
    pub fn place_in_view(
        &mut self,
        id: Id,
        target: Rect,
        appear: bool,
        viewport: Rect,
    ) -> VisualPlacement {
        if target.intersects(viewport) && viewport.height() >= target.height() * 0.5 {
            if let Some(item) = self.items.get_mut(&id) {
                if !item.rect.intersects(viewport)
                    && (item.rect.center() - target.center()).length() > target.height() * 2.0
                {
                    item.rect = target.translate(vec2(0.0, 6.0));
                    item.opacity = 0.35;
                }
            }
        }
        self.place(id, target, appear)
    }
    pub fn place_with_profile(
        &mut self,
        id: Id,
        target: Rect,
        appear: bool,
        profile: MotionProfile,
    ) -> VisualPlacement {
        if !target.min.is_finite() || !target.max.is_finite() || !target.size().is_finite() {
            return VisualPlacement {
                rect: self.items.get(&id).map_or(Rect::ZERO, |item| item.rect),
                opacity: 1.0,
                moving: false,
            };
        }
        let target = Rect::from_min_size(target.min, target.size().max(Vec2::ZERO));
        let can_insert = self.items.len() < self.capacity;
        let item = match self.items.entry(id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                if !can_insert {
                    return VisualPlacement {
                        rect: target,
                        opacity: 1.0,
                        moving: false,
                    };
                }
                let animated_appearance = appear && !self.gains.is_immediate();
                entry.insert(Item {
                    rect: if animated_appearance {
                        target.translate(vec2(0.0, 6.0))
                    } else {
                        target
                    },
                    opacity: if animated_appearance { 0.0 } else { 1.0 },
                    seen: self.frame.wrapping_sub(1),
                })
            }
        };
        let epsilon = PIXEL_SNAP / self.pixels_per_point;
        if self.gains.is_immediate() {
            item.rect = target;
            item.opacity = 1.0;
            item.seen = self.frame;
            return VisualPlacement {
                rect: target,
                opacity: 1.0,
                moving: false,
            };
        }
        // A repeated layout/paint lookup in the same frame must not apply the gain twice.
        if item.seen == self.frame {
            let moving = (item.rect.min - target.min).abs().max_elem() > epsilon
                || (item.rect.size() - target.size()).abs().max_elem() > epsilon
                || item.opacity < 1.0;
            self.moving |= moving;
            return VisualPlacement {
                rect: item.rect,
                opacity: item.opacity,
                moving,
            };
        }
        item.seen = self.frame;
        let mut min = item.rect.min.to_vec2();
        let mut size = item.rect.size();
        let a = animate_vec(&mut min, target.min.to_vec2(), self.gains, profile, epsilon);
        let b = animate_vec(&mut size, target.size(), self.gains, profile, epsilon);
        let c = self.gains.step_normalized(&mut item.opacity, 1.0, profile);
        item.rect = Rect::from_min_size(min.to_pos2(), size.max(Vec2::ZERO));
        let moving = a || b || c;
        self.moving |= moving;
        VisualPlacement {
            rect: item.rect,
            opacity: item.opacity,
            moving,
        }
    }
    pub fn is_moving(&self) -> bool {
        self.moving
    }
    pub fn retained_items(&self) -> usize {
        self.items.len()
    }
    /// Follow a dragged item exactly; release can ease from its last drawn position.
    pub fn snap(&mut self, id: Id, rect: Rect) {
        if rect.is_finite() && (self.items.len() < self.capacity || self.items.contains_key(&id)) {
            self.items.insert(
                id,
                Item {
                    rect,
                    opacity: 1.0,
                    seen: self.frame,
                },
            );
        }
    }
    /// A newly visible item can start at its previous view geometry, including
    /// when that geometry was outside the old viewport.
    pub fn seed(&mut self, id: Id, rect: Rect) {
        if rect.is_finite() && self.items.len() < self.capacity && !self.items.contains_key(&id) {
            self.items.insert(
                id,
                Item {
                    rect,
                    opacity: 1.0,
                    seen: self.frame.wrapping_sub(1),
                },
            );
        }
    }
}
fn animate_vec(
    value: &mut Vec2,
    target: Vec2,
    gains: MotionGains,
    profile: MotionProfile,
    epsilon: f32,
) -> bool {
    let a = gains.step(&mut value.x, target.x, profile, epsilon);
    let b = gains.step(&mut value.y, target.y, profile, epsilon);
    a || b
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(y: f32) -> Rect {
        Rect::from_min_size(egui::pos2(0.0, y), vec2(200.0, 26.0))
    }
    #[test]
    fn source_half_lives_and_legacy_speed_scaling_are_preserved() {
        for selector in 0..4 {
            let profile = MotionProfile::from_selector(selector);
            let gain =
                MotionGains::new(1.0 / profile.rate(), LEGACY_REFERENCE_DURATION).gain(profile);
            assert!((gain - 0.5).abs() < 0.000001);
            let slower = MotionGains::new(2.0 / profile.rate(), LEGACY_REFERENCE_DURATION * 2.0)
                .gain(profile);
            assert!((gain - slower).abs() < 0.000001);
        }
    }
    #[test]
    fn interpolation_is_consistent_across_frame_rates() {
        let mut slow = 0.0;
        let mut fast = 0.0;
        for _ in 0..3 {
            MotionGains::new(1.0 / 30.0, 0.145).step_pixels(
                &mut slow,
                1000.0,
                MotionProfile::Standard,
            );
        }
        for _ in 0..12 {
            MotionGains::new(1.0 / 120.0, 0.145).step_pixels(
                &mut fast,
                1000.0,
                MotionProfile::Standard,
            );
        }
        assert!((slow - fast).abs() < 0.002);
    }
    #[test]
    fn reversing_a_reorder_continues_from_its_current_geometry() {
        let mut motion = LayoutMotion::default();
        let id = Id::new("stable file");
        motion.begin_frame(1.0 / 60.0, 0.145);
        motion.place(id, rect(0.0), false);
        motion.begin_frame(1.0 / 60.0, 0.145);
        let going = motion.place(id, rect(100.0), false);
        assert!(going.rect.top() > 0.0 && going.rect.top() < 100.0);
        let same_frame = motion.place(id, rect(0.0), false);
        assert_eq!(same_frame.rect, going.rect);
        motion.begin_frame(1.0 / 60.0, 0.145);
        let reversed = motion.place(id, rect(0.0), false);
        assert!(reversed.rect.top() > 0.0 && reversed.rect.top() < going.rect.top());
        for _ in 0..120 {
            motion.begin_frame(1.0 / 120.0, 0.145);
            motion.place(id, rect(0.0), false);
        }
        assert!(!motion.is_moving());
    }
    #[test]
    fn viewport_churn_cannot_grow_the_motion_cache() {
        let mut motion = LayoutMotion::new(9999);
        for frame in 0..100 {
            motion.begin_frame(1.0 / 60.0, 0.145);
            for row in 0..1000 {
                motion.place(Id::new((frame, row)), rect(row as f32 * 26.0), true);
            }
            assert!(motion.retained_items() <= 512);
        }
    }
    #[test]
    fn reduced_motion_and_long_stalls_settle_exactly_and_stop_frames() {
        let mut motion = LayoutMotion::default();
        let id = Id::new(1);
        motion.begin_frame(1.0 / 60.0, 0.145);
        motion.place(id, rect(0.0), true);
        motion.begin_frame(1.0 / 60.0, 0.0);
        let placement = motion.place(id, rect(500.0), true);
        assert_eq!(placement.rect, rect(500.0));
        assert_eq!(placement.opacity, 1.0);
        assert!(!placement.moving);
        assert_eq!(motion.place(id, rect(600.0), false).rect, rect(600.0));
        motion.begin_frame(1.0, 0.145);
        let placement = motion.place(id, rect(1000.0), false);
        assert_eq!(placement.rect, rect(1000.0));
        assert!(!placement.moving);
    }
    #[test]
    fn pixel_and_normalized_tails_snap_and_invalid_values_stay_finite() {
        let gains = MotionGains::new(1.0 / 60.0, 0.145);
        let mut pixel = 0.2;
        let mut normalized = 0.995;
        assert!(!gains.step_pixels(&mut pixel, 0.0, MotionProfile::Standard));
        assert_eq!(pixel, 0.0);
        assert!(!gains.step_normalized(&mut normalized, 1.0, MotionProfile::Standard));
        assert_eq!(normalized, 1.0);
        let mut invalid = f32::NAN;
        assert!(!gains.step_pixels(&mut invalid, 10.0, MotionProfile::Standard));
        assert_eq!(invalid, 10.0);
    }
    #[test]
    fn stale_items_leave_after_two_frame_grace() {
        let mut motion = LayoutMotion::default();
        motion.begin_frame(0.01, 0.145);
        motion.place(Id::new(1), rect(0.0), false);
        motion.begin_frame(0.01, 0.145);
        motion.begin_frame(0.01, 0.145);
        assert_eq!(motion.retained_items(), 1);
        motion.begin_frame(0.01, 0.145);
        assert_eq!(motion.retained_items(), 0);
        assert!(!motion.is_moving());
    }

    #[test]
    fn physical_pixel_threshold_and_invalid_geometry_are_bounded() {
        let mut motion = LayoutMotion::default();
        motion.set_pixels_per_point(2.0);
        let id = Id::new("dpi-aware layout");
        motion.begin_frame(0.0, 0.145);
        motion.place(id, rect(0.0), false);
        motion.begin_frame(0.0, 0.145);
        let pending = motion.place(id, rect(0.2), false);
        assert!(
            pending.moving,
            ".2 points at 2x DPI exceeds the .25-pixel snap threshold"
        );
        assert_eq!(pending.rect, rect(0.0));
        motion.begin_frame(0.01, 0.145);
        let invalid = motion.place(id, Rect::NOTHING, false);
        assert!(invalid.rect.min.is_finite() && invalid.rect.max.is_finite());
        assert!(!invalid.moving);
        assert!(!motion.is_moving());
    }

    #[test]
    fn distant_virtual_list_coordinates_do_not_leave_an_idle_redraw_tail() {
        let gains = MotionGains::new(1.0 / 60.0, 0.145);
        for origin in [1_000_000.0_f32, 6_500_000.0, 26_000_000.0, 1_000_000_000.0] {
            let target = origin + 128.0;
            let mut current = origin;
            let mut moving = true;
            for _ in 0..240 {
                moving = gains.step_pixels(&mut current, target, MotionProfile::Standard);
            }
            assert_eq!(current, target);
            assert!(!moving);
        }
    }
}
