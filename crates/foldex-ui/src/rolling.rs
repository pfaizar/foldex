//! Bounded, interruptible numeric reels, drawn directly by egui.
//!
//! Keep one [`RollingNumbers`] per visible surface and use a stable semantic ID
//! (pane + entry + column, for example), never the formatted value as the ID.
//! New identities render immediately. Only subsequent numeric changes roll.
//! Call [`RollingNumbers::clear`] when reusing identities for another directory.
//! The cache also forgets identities absent for more than two painted frames.
//! The caller owns a stable rectangle; target text is right aligned by default.
//!
//! Spring, wheel targeting, entry hold/stagger and symbol accents are adapted
//! from Kit Langton's Rolling Number: <https://github.com/kitlangton/rolling-number>.
//! This native renderer retains only the visible reel faces, with a piecewise
//! linear alpha mask in text meshes; no background-colored cover or JS is used.
//!
//! MIT License
//!
//! Copyright (c) 2026 Kit Langton
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use egui::{Align, Color32, FontId, Galley, Id, IdMap, Painter, Pos2, Rect, pos2, vec2};
use std::sync::Arc;

pub const MAX_ROLLING_NUMBERS: usize = 512;
pub const MAX_NUMBER_GLYPHS: usize = 64;
const MAX_COLUMNS: usize = MAX_NUMBER_GLYPHS * 2;
const DEFAULT_DURATION: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RollDirection {
    /// Follow each displayed magnitude, ignoring its sign, without f64 parsing.
    #[default]
    Auto,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
pub struct RollingOptions {
    /// Seconds. Zero (or a non-finite value) settles immediately; capped at ten.
    pub duration: f64,
    pub align: Align,
    pub direction: RollDirection,
    /// Linear fade at each reel edge, in em. Zero gives a hard vertical clip.
    pub edge_fade: f32,
}

impl Default for RollingOptions {
    fn default() -> Self {
        Self {
            duration: DEFAULT_DURATION,
            align: Align::RIGHT,
            direction: RollDirection::Auto,
            edge_fade: 0.12,
        }
    }
}

impl RollingOptions {
    /// The reference uses 500 ms reels. Respect Foldex's global speed scale and
    /// make both reduced motion and a zero motion token immediate.
    pub fn from_theme(theme: &crate::Theme) -> Self {
        Self {
            duration: f64::from(theme.duration() / crate::motion::LEGACY_REFERENCE_DURATION)
                * DEFAULT_DURATION,
            ..Self::default()
        }
    }

    fn seconds(self) -> f64 {
        if self.duration.is_finite() {
            self.duration.clamp(0.0, 10.0)
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RollingResponse {
    pub moving: bool,
    /// Intrinsic target width, independent of animation and reserved rectangle.
    pub target_width: f32,
}

/// A lazy, fixed-capacity cache. Overflow renders static text rather than
/// evicting/restarting counters already visible in this frame.
pub struct RollingNumbers {
    numbers: IdMap<Number>,
    frame: Option<u64>,
    capacity: usize,
}

impl Default for RollingNumbers {
    fn default() -> Self {
        Self::new(128)
    }
}

impl RollingNumbers {
    pub fn new(capacity: usize) -> Self {
        Self {
            numbers: IdMap::default(),
            frame: None,
            capacity: capacity.clamp(1, MAX_ROLLING_NUMBERS),
        }
    }

    pub fn clear(&mut self) {
        self.numbers.clear();
        self.frame = None;
    }

    pub fn retained_numbers(&self) -> usize {
        self.numbers.len()
    }

    fn begin_frame(&mut self, frame: u64) {
        if self.frame != Some(frame) {
            self.frame = Some(frame);
            self.numbers
                .retain(|_, number| frame.wrapping_sub(number.seen) <= 2);
        }
    }

    /// Paint a preformatted, single-line numeric label in an existing rectangle.
    /// ASCII digits roll; punctuation, units and signs crossfade in place.
    /// Decimal points use `.` and grouping uses `,` in the supplied text.
    /// Multiple values such as `23/456` and `1:02.3` have independent reels.
    /// Non-Latin numerals, bidi text, scientific notation, long labels and elided
    /// values use intact static text. This paints no widget or accessibility node:
    /// keep the latest target in the owning control's WidgetInfo/tooltip.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &Painter,
        id: Id,
        rect: Rect,
        text: &str,
        font: FontId,
        color: Color32,
        options: RollingOptions,
    ) -> RollingResponse {
        self.paint_inner(painter, id, rect, text, font, color, options, false)
    }

    /// Paint inside a continuously resizing slot without discarding its reels.
    /// Whole columns reveal/fade at a moving horizontal edge and their masks
    /// remain inside the slot. Font size and glyph proportions stay unchanged.
    /// Use `paint` for a genuinely constrained slot that should elide its value.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_fitted(
        &mut self,
        painter: &Painter,
        id: Id,
        rect: Rect,
        text: &str,
        font: FontId,
        color: Color32,
        options: RollingOptions,
    ) -> RollingResponse {
        self.paint_inner(painter, id, rect, text, font, color, options, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_inner(
        &mut self,
        painter: &Painter,
        id: Id,
        rect: Rect,
        text: &str,
        font: FontId,
        color: Color32,
        options: RollingOptions,
        fitted: bool,
    ) -> RollingResponse {
        let ctx = painter.ctx();
        let frame = ctx.cumulative_frame_nr();
        self.begin_frame(frame);
        if !rect.is_finite()
            || !rect.is_positive()
            || !rect.intersects(painter.clip_rect())
            || !painter.is_visible()
            || painter.opacity() == 0.0
            || color == Color32::TRANSPARENT
        {
            return RollingResponse::default();
        }
        let galley = crate::fonts::no_wrap(ctx, text, font.clone());
        let width = galley.size().x;
        let response = RollingResponse {
            moving: false,
            target_width: width,
        };
        let style = Style {
            font: font.clone(),
            pixels_per_point: ctx.pixels_per_point().to_bits(),
            revision: crate::fonts::revision(ctx),
            align: options.align,
        };
        if (!fitted && width > rect.width() + 0.25)
            || !rollable(text)
            || galley.rows.len() != 1
            || galley.rows[0].glyphs.len() != text.chars().count()
        {
            self.numbers.remove(&id);
            let elided =
                crate::fonts::layout(ctx, text, font, Color32::PLACEHOLDER, rect.width(), 1);
            paint_static(painter, rect, elided, color, options.align);
            return response;
        }
        if !self.numbers.contains_key(&id) && self.numbers.len() >= self.capacity {
            paint_static(painter, rect, galley, color, options.align);
            return response;
        }
        let now = ctx.input(|input| input.time);
        let now = if now.is_finite() { now } else { 0.0 };
        let number = self.numbers.entry(id).or_insert_with(|| {
            let model = Model::new(text, &galley, options.align);
            let columns = model.tokens.iter().copied().map(Column::settled).collect();
            Number {
                model,
                columns,
                style: style.clone(),
                seen: frame,
                last_time: now,
            }
        });
        let changed = number.model.text != text;
        let reset = number.columns.is_empty() || number.style != style || now < number.last_time;
        if changed || reset {
            let next = Model::new(text, &galley, options.align);
            number.update(
                next,
                now,
                if reset { 0.0 } else { options.seconds() },
                options.direction,
            );
            number.style = style;
        } else if options.seconds() == 0.0 && number.columns.iter().any(|column| column.moving(now))
        {
            number.finish();
        }
        number.seen = frame;
        number.last_time = now;
        number.prune(now);
        let moving = number.columns.iter().any(|column| column.moving(now));
        if !moving && (!fitted || width <= rect.width() + 0.25) {
            paint_static(painter, rect, galley, color, options.align);
            return response;
        }

        let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let anchor = aligned_anchor(rect, options.align);
        let height = galley.size().y;
        let top = rect.center().y - height * 0.5;
        for column in &number.columns {
            let mut opacity = column.alpha.sample(now).position.clamp(0.0, 1.0) as f32;
            if opacity <= 0.001 {
                continue;
            }
            let x = anchor + column.x.sample(now).position as f32;
            let mut viewport = Rect::from_min_size(pos2(x, top), vec2(column.width, height));
            if fitted {
                let Some((inside, reveal)) = fit_column(viewport, rect) else {
                    continue;
                };
                viewport = inside;
                opacity *= reveal;
            }
            if opacity <= 0.001 {
                continue;
            }
            let scale = column.scale.sample(now).position as f32;
            if column.token.digit().is_some() {
                let roll = column.roll.sample(now).position;
                let entry = column.entry.sample(now).position;
                let base = roll.floor();
                for face in [base, base + 1.0] {
                    let glyph = char::from(b'0' + (face as i64).rem_euclid(10) as u8);
                    let offset = ((face - roll + entry) as f32) * height;
                    paint_face(
                        &painter,
                        viewport,
                        glyph,
                        &font,
                        color,
                        opacity,
                        offset,
                        1.0,
                        Some(options.edge_fade),
                    );
                }
            } else {
                paint_face(
                    &painter,
                    viewport,
                    column.token.glyph,
                    &font,
                    color,
                    opacity,
                    0.0,
                    scale,
                    None,
                );
            }
        }
        if moving {
            ctx.request_repaint();
        }
        RollingResponse { moving, ..response }
    }
}

fn fit_column(viewport: Rect, slot: Rect) -> Option<(Rect, f32)> {
    // Adding an advance to a fractional screen coordinate can round its
    // reconstructed width by an ulp. Do not drop a complete cell for that.
    let roundoff = f32::EPSILON
        * 4.0
        * viewport
            .left()
            .abs()
            .max(viewport.right().abs())
            .max(slot.left().abs())
            .max(slot.right().abs())
            .max(1.0);
    if viewport.width() > slot.width() + roundoff || viewport.width() <= 0.0 || slot.width() <= 0.0
    {
        return None;
    }
    // Reveal a complete glyph instead of horizontally slicing a reel. The
    // mapping is continuous in both its sampled position and the live slot.
    let reveal = ((viewport.right().min(slot.right()) - viewport.left().max(slot.left()))
        / viewport.width())
    .clamp(0.0, 1.0);
    let width = viewport.width().min(slot.width());
    let left = viewport.left().clamp(slot.left(), slot.right() - width);
    Some((
        Rect::from_min_max(
            pos2(left, viewport.top()),
            pos2((left + width).min(slot.right()), viewport.bottom()),
        ),
        reveal,
    ))
}

#[derive(Clone, PartialEq)]
struct Style {
    font: FontId,
    pixels_per_point: u32,
    revision: usize,
    align: Align,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Identity {
    Digit { run: u8, place: i8 },
    Group { run: u8, place: i8 },
    Decimal { run: u8 },
    Symbol { run: u8, occurrence: u8 },
}

#[derive(Clone, Copy, Debug)]
struct Token {
    identity: Identity,
    glyph: char,
    x: f32,
    width: f32,
}

impl Token {
    fn digit(self) -> Option<u8> {
        self.glyph.is_ascii_digit().then(|| self.glyph as u8 - b'0')
    }

    fn same_key(self, other: Self) -> bool {
        self.identity == other.identity && (self.digit().is_some() || self.glyph == other.glyph)
    }
}

#[derive(Default)]
struct Magnitude {
    integer: String,
    fraction: String,
}

impl Magnitude {
    fn trend(&self, next: &Self) -> i8 {
        let integer = self.integer.trim_start_matches('0');
        let next_integer = next.integer.trim_start_matches('0');
        let order = integer
            .len()
            .cmp(&next_integer.len())
            .then_with(|| integer.cmp(next_integer));
        let order = order.then_with(|| {
            let count = self.fraction.len().max(next.fraction.len());
            self.fraction
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(count)
                .cmp(
                    next.fraction
                        .bytes()
                        .chain(std::iter::repeat(b'0'))
                        .take(count),
                )
        });
        match order {
            std::cmp::Ordering::Less => 1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => -1,
        }
    }
}

struct Model {
    text: String,
    tokens: Vec<Token>,
    magnitudes: Vec<Magnitude>,
}

impl Model {
    fn new(text: &str, galley: &Galley, align: Align) -> Self {
        let chars: Vec<_> = text.chars().collect();
        let glyphs = &galley.rows[0].glyphs;
        let mut model = Self {
            text: text.to_owned(),
            tokens: Vec::with_capacity(chars.len()),
            magnitudes: Vec::new(),
        };
        let origin = galley.size().x * alignment(align);
        let mut at = 0;
        let mut symbol = 0;
        while at < chars.len() {
            if chars[at].is_ascii_digit() {
                let start = at;
                while at < chars.len()
                    && (chars[at].is_ascii_digit()
                        || matches!(chars[at], '.' | ',')
                            && chars.get(at + 1).is_some_and(char::is_ascii_digit))
                {
                    at += 1;
                }
                let run = model.magnitudes.len() as u8;
                let decimal = (start..at).find(|&index| chars[index] == '.').unwrap_or(at);
                let mut place = chars[start..decimal]
                    .iter()
                    .filter(|c| c.is_ascii_digit())
                    .count() as i8;
                let mut magnitude = Magnitude::default();
                for index in start..at {
                    let glyph = chars[index];
                    let identity = if glyph.is_ascii_digit() {
                        place -= 1;
                        if index < decimal {
                            magnitude.integer.push(glyph);
                        } else {
                            magnitude.fraction.push(glyph);
                        }
                        Identity::Digit { run, place }
                    } else if glyph == '.' {
                        place = 0;
                        Identity::Decimal { run }
                    } else {
                        Identity::Group { run, place }
                    };
                    model.tokens.push(Token {
                        identity,
                        glyph,
                        x: glyphs[index].pos.x - origin,
                        width: glyphs[index].advance_width,
                    });
                }
                model.magnitudes.push(magnitude);
                symbol = 0;
            } else {
                model.tokens.push(Token {
                    identity: Identity::Symbol {
                        run: model.magnitudes.len() as u8,
                        occurrence: symbol,
                    },
                    glyph: chars[at],
                    x: glyphs[at].pos.x - origin,
                    width: glyphs[at].advance_width,
                });
                symbol += 1;
                at += 1;
            }
        }
        // Runs are identified from the right: adding a selected/total prefix
        // or a minute field preserves the existing total/seconds reels.
        let runs = model.magnitudes.len() as u8;
        for token in &mut model.tokens {
            match &mut token.identity {
                Identity::Digit { run, .. }
                | Identity::Group { run, .. }
                | Identity::Decimal { run } => {
                    *run = runs - 1 - *run;
                }
                Identity::Symbol { run, .. } => *run = runs.saturating_sub(*run),
            }
        }
        model.magnitudes.reverse();
        model
    }
}

fn rollable(text: &str) -> bool {
    if text.is_empty()
        || text.len() > MAX_NUMBER_GLYPHS * 4
        || text.chars().count() > MAX_NUMBER_GLYPHS
    {
        return false;
    }
    !text.chars().any(|glyph| {
        glyph.is_control()
            || glyph.is_numeric() && !glyph.is_ascii_digit()
            || matches!(glyph as u32, 0x0590..=0x08ff | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069 | 0xfb1d..=0xfeff)
    }) && !text.as_bytes().windows(2).any(|pair| {
        (pair[0].is_ascii_digit() && matches!(pair[1], b'e' | b'E'))
            || (matches!(pair[0], b'e' | b'E') && (pair[1].is_ascii_digit() || matches!(pair[1], b'+' | b'-')))
    })
}

struct Number {
    model: Model,
    columns: Vec<Column>,
    style: Style,
    seen: u64,
    last_time: f64,
}

impl Number {
    fn finish(&mut self) {
        self.columns.clear();
        self.columns
            .extend(self.model.tokens.iter().copied().map(Column::settled));
    }

    fn prune(&mut self, now: f64) {
        self.columns
            .retain(|column| !column.exiting || !column.alpha.finished(now));
    }

    fn update(&mut self, next: Model, now: f64, duration: f64, direction: RollDirection) {
        self.prune(now);
        if duration <= 0.0 {
            self.model = next;
            self.finish();
            return;
        }
        let mut old: Vec<_> = self
            .columns
            .iter()
            .map(|column| (column.token, column.x.sample(now).position, column.width))
            .collect();
        old.sort_by(|a, b| a.1.total_cmp(&b.1));
        let retained: Vec<_> = next
            .tokens
            .iter()
            .map(|token| {
                token.digit().is_some()
                    && self
                        .columns
                        .iter()
                        .any(|column| !column.exiting && column.token.same_key(*token))
            })
            .collect();
        let ranks: Vec<_> = (0..next.tokens.len())
            .map(|index| {
                if retained[index] {
                    0
                } else {
                    retained
                        .iter()
                        .enumerate()
                        .filter(|(_, kept)| **kept)
                        .map(|(other, _)| index.abs_diff(other))
                        .min()
                        .unwrap_or(index + 1)
                }
            })
            .collect();
        let span = ranks
            .iter()
            .copied()
            .max()
            .unwrap_or(1)
            .saturating_sub(1)
            .max(1);
        let step = (duration * 0.045).min(duration * 0.3 / span as f64);
        for (index, token) in next.tokens.iter().copied().enumerate() {
            let existing = self
                .columns
                .iter()
                .position(|column| column.token.same_key(token));
            let replaced = self.model.tokens.iter().find(|previous| {
                previous.identity == token.identity && previous.glyph != token.glyph
            });
            let replacement = replaced
                .and_then(|previous| old.iter().find(|(old, _, _)| old.same_key(*previous)));
            let delay = ranks[index].saturating_sub(1) as f64 * step;
            if let Some(existing) = existing {
                let column = &mut self.columns[existing];
                column.x.retarget(f64::from(token.x), now, duration);
                if column.exiting {
                    column.alpha.retarget(
                        1.0,
                        now,
                        if token.digit().is_some() {
                            duration
                        } else {
                            duration.min(0.18)
                        },
                    );
                    column.scale.retarget(1.0, now, duration.min(0.18));
                }
                if let Some(digit) = token.digit()
                    && column.token.glyph != token.glyph
                {
                    let run = match token.identity {
                        Identity::Digit { run, .. } => run as usize,
                        _ => 0,
                    };
                    let trend = match direction {
                        RollDirection::Up => 1,
                        RollDirection::Down => -1,
                        RollDirection::Auto => self
                            .model
                            .magnitudes
                            .get(run)
                            .zip(next.magnitudes.get(run))
                            .map_or(0, |(old, next)| old.trend(next)),
                    };
                    let sample = column.roll.sample(now);
                    // Rebase complete revolutions so rapid updates cannot grow
                    // coordinates or allocate an ever-longer intermediate reel.
                    let from = sample.position.rem_euclid(10.0);
                    column.roll = Spring::new(
                        from,
                        roll_target(from, digit, trend),
                        sample.velocity,
                        now,
                        duration,
                    );
                }
                column.token = token;
                column.width = token.width;
                column.exiting = false;
            } else {
                let start = replacement
                    .map(|(_, x, _)| *x)
                    .unwrap_or_else(|| collapse(index, &next.tokens, &old, f64::from(token.x)));
                let mut column = Column::settled(token);
                column.x = Spring::new(start, f64::from(token.x), 0.0, now, duration);
                column.alpha = Spring::new(
                    0.0,
                    1.0,
                    0.0,
                    now + delay,
                    if token.digit().is_some() {
                        duration
                    } else {
                        duration.min(0.18)
                    },
                );
                if token.digit().is_some() {
                    column.entry = Spring::new(
                        1.0,
                        0.0,
                        0.0,
                        now + delay + duration * 0.14,
                        duration * 0.86,
                    );
                } else if replacement.is_some() {
                    column.scale = Spring::new(0.96, 1.0, 0.0, now, duration.min(0.18));
                }
                self.columns.push(column);
            }
        }
        let mut exit_geometry = None;
        for column in &mut self.columns {
            if column.exiting || next.tokens.iter().any(|token| token.same_key(column.token)) {
                continue;
            }
            column.exiting = true;
            let replacement = next
                .tokens
                .iter()
                .find(|token| token.identity == column.token.identity);
            if let Some(replacement) = replacement {
                column.x.retarget(f64::from(replacement.x), now, duration);
                if column.token.digit().is_none() {
                    column.scale.retarget(1.04, now, duration.min(0.18));
                }
            } else if let Some(index) = old
                .iter()
                .position(|(token, _, _)| token.same_key(column.token))
            {
                let (old_tokens, target_positions) = exit_geometry.get_or_insert_with(|| {
                    let old_tokens: Vec<_> = old.iter().map(|(token, _, _)| *token).collect();
                    let target_positions: Vec<_> = next
                        .tokens
                        .iter()
                        .map(|token| (*token, f64::from(token.x), token.width))
                        .collect();
                    (old_tokens, target_positions)
                });
                let target = collapse(
                    index,
                    old_tokens,
                    target_positions,
                    column.x.sample(now).position,
                );
                column.x.retarget(target, now, duration);
            }
            let fade = if column.token.digit().is_some() {
                duration * 0.65
            } else {
                duration.min(0.18)
            };
            column.alpha.retarget(0.0, now, fade);
        }
        // Rapid symbol replacements have at most one bounded set of exits. Drop
        // the least visible exit if updates outrun their short fade.
        while self.columns.len() > MAX_COLUMNS {
            let least = self
                .columns
                .iter()
                .enumerate()
                .filter(|(_, column)| column.exiting)
                .min_by(|(_, a), (_, b)| {
                    a.alpha
                        .sample(now)
                        .position
                        .total_cmp(&b.alpha.sample(now).position)
                })
                .map(|(index, _)| index);
            if let Some(index) = least {
                self.columns.remove(index);
            } else {
                break;
            }
        }
        self.model = next;
    }
}

fn collapse(index: usize, order: &[Token], positions: &[(Token, f64, f32)], fallback: f64) -> f64 {
    for token in &order[index + 1..] {
        if let Some((_, x, _)) = positions.iter().find(|(old, _, _)| old.same_key(*token)) {
            return *x;
        }
    }
    for token in order[..index].iter().rev() {
        if let Some((_, x, width)) = positions.iter().find(|(old, _, _)| old.same_key(*token)) {
            return *x + f64::from(*width);
        }
    }
    fallback
}

struct Column {
    token: Token,
    width: f32,
    x: Spring,
    roll: Spring,
    alpha: Spring,
    entry: Spring,
    scale: Spring,
    exiting: bool,
}

impl Column {
    fn settled(token: Token) -> Self {
        Self {
            token,
            width: token.width,
            x: Spring::settled(f64::from(token.x)),
            roll: Spring::settled(f64::from(token.digit().unwrap_or(0))),
            alpha: Spring::settled(1.0),
            entry: Spring::settled(0.0),
            scale: Spring::settled(1.0),
            exiting: false,
        }
    }

    fn moving(&self, now: f64) -> bool {
        [self.x, self.roll, self.alpha, self.entry, self.scale]
            .iter()
            .any(|spring| !spring.finished(now))
    }
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    position: f64,
    velocity: f64,
}

#[derive(Clone, Copy, Debug)]
struct Spring {
    from: f64,
    target: f64,
    velocity: f64,
    start: f64,
    duration: f64,
}

impl Spring {
    fn settled(target: f64) -> Self {
        Self {
            from: target,
            target,
            velocity: 0.0,
            start: 0.0,
            duration: 0.0,
        }
    }

    fn new(from: f64, target: f64, velocity: f64, start: f64, duration: f64) -> Self {
        if duration <= 0.0 || (from - target).abs() < 0.00001 && velocity.abs() < 0.00001 {
            return Self::settled(target);
        }
        let limit = (from - target).abs().max(1.0) * 12.0 / duration;
        Self {
            from,
            target,
            velocity: velocity.clamp(-limit, limit),
            start,
            duration,
        }
    }

    fn finished(self, now: f64) -> bool {
        self.duration <= 0.0 || now >= self.start + self.duration
    }

    fn sample(self, now: f64) -> Sample {
        if self.finished(now) {
            return Sample {
                position: self.target,
                velocity: 0.0,
            };
        }
        if now < self.start {
            return Sample {
                position: self.from,
                velocity: 0.0,
            };
        }
        let time = ((now - self.start) / self.duration).clamp(0.0, 1.0);
        let distance = self.from - self.target;
        let inherited = self.velocity * self.duration + 10.0 * distance;
        let offset = distance + inherited * time;
        let decay = (-10.0 * time).exp();
        Sample {
            position: self.target + offset * decay,
            velocity: (inherited - 10.0 * offset) * decay / self.duration,
        }
    }

    fn retarget(&mut self, target: f64, now: f64, duration: f64) {
        // Unchanged targets keep their original deadline even at high update rates.
        if self.target != target {
            let sample = self.sample(now);
            *self = Self::new(sample.position, target, sample.velocity, now, duration);
        }
    }
}

fn roll_target(position: f64, digit: u8, trend: i8) -> f64 {
    let mut target = (position / 10.0).floor() * 10.0 + f64::from(digit);
    if trend > 0 && target < position - 0.001 {
        target += 10.0;
    } else if trend < 0 && target > position + 0.001 {
        target -= 10.0;
    } else if trend == 0 {
        // JavaScript Math.round chooses the positive direction at a half tie.
        target += (((position - target) / 10.0) + 0.5).floor() * 10.0;
    }
    target
}

fn alignment(align: Align) -> f32 {
    match align {
        Align::Min => 0.0,
        Align::Center => 0.5,
        Align::Max => 1.0,
    }
}

fn aligned_anchor(rect: Rect, align: Align) -> f32 {
    rect.left() + rect.width() * alignment(align)
}

fn paint_static(painter: &Painter, rect: Rect, galley: Arc<Galley>, color: Color32, align: Align) {
    let x = aligned_anchor(rect, align) - galley.size().x * alignment(align);
    painter
        .with_clip_rect(rect.intersect(painter.clip_rect()))
        .galley(
            pos2(x, rect.center().y - galley.size().y * 0.5),
            galley,
            color,
        );
}

#[allow(clippy::too_many_arguments)]
fn paint_face(
    painter: &Painter,
    viewport: Rect,
    glyph: char,
    font: &FontId,
    color: Color32,
    opacity: f32,
    offset: f32,
    scale: f32,
    fade: Option<f32>,
) {
    let galley = crate::fonts::no_wrap(painter.ctx(), &glyph.to_string(), font.clone());
    if galley.is_empty() || galley.rows.len() != 1 {
        return;
    }
    let shift = vec2(
        (viewport.width() - galley.size().x * scale) * 0.5,
        (viewport.height() - galley.size().y * scale) * 0.5 + offset,
    );
    let source = &galley.rows[0];
    let mut row = (*source.row).clone();
    let mut mesh = egui::epaint::Mesh::default();
    let edge = fade.map(|fade| {
        if fade.is_finite() {
            (fade * font.size).clamp(0.0, viewport.height() * 0.5)
        } else {
            0.0
        }
    });
    // Simple single-glyph layouts have one axis-aligned quad. Keep UVs in texels
    // in a TextShape so a later font-atlas growth in this frame remains correct.
    for quad in source.visuals.mesh.vertices.chunks_exact(4) {
        let ink = Rect::from_min_max(
            (quad[0].pos.to_vec2() * scale + source.pos.to_vec2() + shift).to_pos2(),
            (quad[3].pos.to_vec2() * scale + source.pos.to_vec2() + shift).to_pos2(),
        );
        let uv = Rect::from_min_max(quad[0].uv, quad[3].uv);
        append_masked_quad(&mut mesh, ink, uv, viewport.height(), edge, color, opacity);
    }
    if mesh.is_empty() {
        return;
    }
    let bounds = mesh.calc_bounds();
    row.visuals.glyph_vertex_range = 0..mesh.vertices.len();
    row.visuals.mesh_bounds = bounds;
    row.visuals.mesh = mesh;
    let mut visual = (*galley).clone();
    visual.num_vertices = row.visuals.mesh.vertices.len();
    visual.num_indices = row.visuals.mesh.indices.len();
    visual.mesh_bounds = bounds;
    visual.rows[0].pos = Pos2::ZERO;
    visual.rows[0].row = Arc::new(row);
    // The stable viewport origin is pixel snapped by the normal text painter;
    // fractional reel motion lives inside the mesh and stays continuous.
    painter.add(egui::epaint::TextShape::new(
        viewport.min,
        Arc::new(visual),
        color,
    ));
}

fn append_masked_quad(
    mesh: &mut egui::epaint::Mesh,
    ink: Rect,
    uv: Rect,
    height: f32,
    edge: Option<f32>,
    color: Color32,
    opacity: f32,
) {
    if !ink.is_positive() {
        return;
    }
    let cuts = match edge {
        Some(edge) => [0.0, edge, height - edge, height],
        None => [ink.top(), ink.top(), ink.bottom(), ink.bottom()],
    };
    for band in cuts.windows(2) {
        let top = ink.top().max(band[0]);
        let bottom = ink.bottom().min(band[1]);
        if bottom <= top {
            continue;
        }
        let alpha = |y: f32| {
            let mask = edge.filter(|edge| *edge > 0.0).map_or(1.0, |edge| {
                (y / edge).min((height - y) / edge).clamp(0.0, 1.0)
            });
            color.gamma_multiply((opacity * mask).clamp(0.0, 1.0))
        };
        let uv_y = |y| uv.top() + (y - ink.top()) / ink.height() * uv.height();
        let first = mesh.vertices.len() as u32;
        mesh.add_rect_with_uv(
            Rect::from_min_max(pos2(ink.left(), top), pos2(ink.right(), bottom)),
            Rect::from_min_max(pos2(uv.left(), uv_y(top)), pos2(uv.right(), uv_y(bottom))),
            alpha(top),
        );
        mesh.vertices[first as usize + 2].color = alpha(bottom);
        mesh.vertices[first as usize + 3].color = alpha(bottom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(ui: &egui::Ui, text: &str) -> Model {
        Model::new(
            text,
            &crate::fonts::no_wrap(ui.ctx(), text, FontId::proportional(12.0)),
            Align::RIGHT,
        )
    }

    fn number(ui: &egui::Ui, text: &str) -> Number {
        let mut number = Number {
            model: model(ui, text),
            columns: Vec::new(),
            style: Style {
                font: FontId::proportional(12.0),
                pixels_per_point: 1.0_f32.to_bits(),
                revision: 0,
                align: Align::RIGHT,
            },
            seen: 0,
            last_time: 0.0,
        };
        number.finish();
        number
    }

    #[test]
    fn wheel_direction_wraps_by_at_most_one_revolution() {
        assert_eq!(roll_target(9.0, 0, 1), 10.0);
        assert_eq!(roll_target(0.0, 9, -1), -1.0);
        assert_eq!(roll_target(0.0, 5, 0), 5.0);
        for position in [-120.25, -9.9, -0.1, 0.0, 2.75, 9.999, 453.1] {
            for digit in 0..10 {
                for trend in [-1, 0, 1] {
                    let target = roll_target(position, digit, trend);
                    assert!((target - position).abs() <= 10.001);
                    assert!((target.rem_euclid(10.0) - f64::from(digit)).abs() < 0.00001);
                    assert!(trend <= 0 || target >= position - 0.001);
                    assert!(trend >= 0 || target <= position + 0.001);
                }
            }
        }
    }

    #[test]
    fn interruption_samples_position_and_velocity_and_has_a_finite_deadline() {
        let mut spring = Spring::new(8.0, 11.0, 0.0, 10.0, 0.5);
        let before = spring.sample(10.13);
        spring.retarget(2.0, 10.13, 0.5);
        let after = spring.sample(10.13);
        assert!((before.position - after.position).abs() < 0.0000001);
        assert!((before.velocity - after.velocity).abs() < 0.0000001);
        let start = spring.start;
        spring.retarget(2.0, 10.2, 0.5);
        assert_eq!(
            spring.start, start,
            "unchanged values cannot extend an animation"
        );
        assert!(spring.finished(10.64));
        assert_eq!(spring.sample(10.64).position, 2.0);
        assert_eq!(spring.sample(10.64).velocity, 0.0);
        let bounded = Spring::new(0.0, 1.0, 1.0e30, 0.0, 0.5);
        assert_eq!(bounded.velocity, 24.0);
    }

    #[test]
    fn displayed_magnitudes_preserve_large_integer_precision_and_ignore_signs() {
        let magnitude = |integer: &str, fraction: &str| Magnitude {
            integer: integer.into(),
            fraction: fraction.into(),
        };
        assert_eq!(
            magnitude("9007199254740992", "").trend(&magnitude("9007199254740993", "")),
            1
        );
        assert_eq!(magnitude("0012", "30").trend(&magnitude("12", "3")), 0);
        assert_eq!(magnitude("12", "").trend(&magnitude("11", "")), -1);
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            assert_eq!(
                model(ui, "-12").magnitudes[0].trend(&model(ui, "-11").magnitudes[0]),
                -1
            );
            assert_eq!(model(ui, "1,234.50").magnitudes[0].integer, "1234");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn carries_and_new_selected_or_minute_fields_keep_existing_digit_identities() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            let before = model(ui, "99");
            let after = model(ui, "100");
            assert_eq!(before.tokens[1].identity, after.tokens[2].identity);
            assert_eq!(before.tokens[0].identity, after.tokens[1].identity);
            let before = model(ui, "26");
            let selected = model(ui, "1/26");
            assert!(
                before
                    .tokens
                    .iter()
                    .zip(&selected.tokens[2..])
                    .all(|(a, b)| a.same_key(*b))
            );
            let seconds = model(ui, "59.9 sec");
            let minutes = model(ui, "1:00.0");
            assert_eq!(seconds.tokens[0].identity, minutes.tokens[2].identity);
            for text in ["1", "12345678", "1,234.50", "23/456"] {
                let target = model(ui, text);
                let last = target.tokens.last().unwrap();
                assert!(
                    (last.x + last.width).abs() <= 1.0,
                    "{text} must keep its right anchor"
                );
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn rapid_updates_remain_bounded_and_latest_value_settles_without_a_queue() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| {
            let mut number = number(ui, "0");
            for update in 0..500 {
                let units = char::from(b'A' + (update % 26) as u8)
                    .to_string()
                    .repeat(32);
                let text = format!("{} {units}", update * 671);
                number.update(
                    model(ui, &text),
                    f64::from(update) / 10000.0,
                    0.5,
                    RollDirection::Auto,
                );
                assert!(number.columns.len() <= MAX_COLUMNS);
                for column in &number.columns {
                    let sample = column.roll.sample(f64::from(update) / 10000.0);
                    assert!(sample.position.is_finite() && sample.velocity.is_finite());
                    assert!(sample.position.abs() < 40.0);
                }
            }
            number.prune(2.0);
            assert!(number.columns.len() <= MAX_NUMBER_GLYPHS);
            assert_eq!(number.columns.len(), number.model.tokens.len());
            assert!(
                number
                    .columns
                    .iter()
                    .all(|column| !column.moving(2.0) && !column.exiting)
            );
            assert!(number.model.text.starts_with("334829 "));
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn reduced_motion_settles_active_reels_and_new_identities_start_static() {
        let ctx = egui::Context::default();
        let mut numbers = RollingNumbers::new(2);
        for frame in 0..4 {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(frame) / 60.0),
                    ..Default::default()
                },
                |ui| {
                    let response = numbers.paint(
                        ui.painter(),
                        Id::new("count"),
                        Rect::from_min_size(pos2(5.0, 5.0), vec2(200.0, 30.0)),
                        if frame == 0 { "9" } else { "100" },
                        FontId::proportional(12.0),
                        Color32::WHITE,
                        RollingOptions {
                            duration: if frame >= 2 { 0.0 } else { 0.5 },
                            ..Default::default()
                        },
                    );
                    assert_eq!(response.moving, frame == 1);
                    if frame >= 2 {
                        let number = &numbers.numbers[&Id::new("count")];
                        assert_eq!(number.columns.len(), 3);
                        assert!(number.columns.iter().all(|column| !column.moving(1.0)));
                    }
                },
            )
            .drop_without_applying_deltas();
        }
    }

    #[test]
    fn fitted_growth_and_shrink_preserve_numeric_state_instead_of_eliding() {
        let ctx = egui::Context::default();
        let mut numbers = RollingNumbers::new(1);
        let id = Id::new("adaptive-count");
        let font = FontId::proportional(13.0);
        for frame in 0..45 {
            let text = match frame {
                0 => "9",
                1 => "1000000000",
                2 => "22",
                3 => "12345678901234567890",
                _ => "1",
            };
            ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(frame) / 60.0),
                    ..Default::default()
                },
                |ui| {
                    // A narrow first animation frame must retain the new model and
                    // the exiting columns even though its full target does not fit.
                    let rect = Rect::from_min_size(pos2(5.0, 5.0), vec2(18.0, 26.0));
                    let response = numbers.paint_fitted(
                        ui.painter(),
                        id,
                        rect,
                        text,
                        font.clone(),
                        Color32::WHITE,
                        RollingOptions::default(),
                    );
                    assert_eq!(numbers.retained_numbers(), 1);
                    let number = &numbers.numbers[&id];
                    assert_eq!(number.model.text, text);
                    assert_eq!(
                        number.style.font, font,
                        "slot motion must not resize glyphs"
                    );
                    assert!(number.columns.len() <= MAX_COLUMNS);
                    if (1..=4).contains(&frame) {
                        assert!(response.moving);
                    }
                    if frame == 1 || frame == 3 {
                        assert!(response.target_width > rect.width());
                        assert!(number.columns.len() > 2);
                    }
                    if frame == 4 {
                        assert!(number.columns.iter().any(|column| column.exiting));
                    }
                    if frame == 44 {
                        assert!(!response.moving);
                        assert_eq!(number.columns.len(), 1);
                    }
                },
            )
            .drop_without_applying_deltas();
        }
    }

    #[test]
    fn fitted_column_masks_stay_inside_the_moving_slot_without_scaling() {
        for width in [8.0, 8.5, 12.0, 24.0, 80.0, 180.0] {
            let slot = Rect::from_min_size(pos2(30.0, 20.0), vec2(width, 20.0));
            let mut previous: Option<(f32, f32)> = None;
            for step in 0..=100 {
                let viewport =
                    Rect::from_min_size(pos2(20.0 + step as f32 * 0.1, 24.0), vec2(8.0, 12.0));
                let (inside, reveal) = fit_column(viewport, slot).unwrap();
                assert!(slot.contains_rect(inside));
                assert!((inside.size() - viewport.size()).abs().max_elem() < 0.00001);
                assert!((0.0..=1.0).contains(&reveal));
                if let Some((left, alpha)) = previous {
                    assert!((inside.left() - left).abs() <= 0.101);
                    assert!((reveal - alpha).abs() <= 0.013);
                }
                previous = Some((inside.left(), reveal));
            }
        }
    }

    #[test]
    fn settled_labels_do_not_schedule_repaints_and_cache_only_keeps_visible_ids() {
        let ctx = egui::Context::default();
        let mut numbers = RollingNumbers::new(1);
        let mut delay = std::time::Duration::ZERO;
        for frame in 0..12 {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(frame)),
                    ..Default::default()
                },
                |ui| {
                    for index in 0..2 {
                        let response = numbers.paint(
                            ui.painter(),
                            Id::new((if frame < 3 { 0 } else { 1 }, index)),
                            Rect::from_min_size(
                                pos2(5.0, 5.0 + index as f32 * 30.0),
                                vec2(200.0, 25.0),
                            ),
                            "123",
                            FontId::proportional(12.0),
                            Color32::WHITE,
                            RollingOptions::default(),
                        );
                        assert!(!response.moving);
                    }
                    assert_eq!(numbers.retained_numbers(), 1);
                },
            );
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.drop_without_applying_deltas();
        }
        assert_eq!(delay, std::time::Duration::MAX);
        assert!(numbers.numbers.contains_key(&Id::new((1, 0))));
        assert!(!numbers.numbers.contains_key(&Id::new((0, 0))));
    }

    #[test]
    fn soft_mask_splits_at_each_linear_alpha_boundary_and_preserves_texel_uvs() {
        let mut mesh = egui::epaint::Mesh::default();
        append_masked_quad(
            &mut mesh,
            Rect::from_min_max(pos2(0.0, -2.0), pos2(8.0, 12.0)),
            Rect::from_min_max(pos2(10.0, 100.0), pos2(18.0, 128.0)),
            10.0,
            Some(2.0),
            Color32::WHITE,
            1.0,
        );
        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), 12);
        assert_eq!(mesh.indices.len(), 18);
        assert_eq!(
            mesh.calc_bounds(),
            Rect::from_min_size(Pos2::ZERO, vec2(8.0, 10.0))
        );
        assert_eq!(mesh.vertices[0].color.a(), 0);
        assert_eq!(mesh.vertices[2].color.a(), 255);
        assert_eq!(mesh.vertices[10].color.a(), 0);
        assert_eq!(mesh.vertices[0].uv.y, 104.0);
        assert_eq!(mesh.vertices[10].uv.y, 124.0);
    }

    #[test]
    fn unsupported_and_overlong_formats_use_static_fallbacks() {
        assert!(!rollable("١٢٣"));
        assert!(!rollable("123\u{202e}"));
        assert!(!rollable("1.2e+12"));
        assert!(!rollable(&"1".repeat(MAX_NUMBER_GLYPHS + 1)));
        assert!(rollable("23/26"));
        assert!(rollable("1,234.56 MiB"));
    }
}
