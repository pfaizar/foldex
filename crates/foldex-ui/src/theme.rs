//! Appearance preferences, paired color schemes, and legacy theme migration.
use crate::{components, focus};
use egui::{Color32, FontId, Stroke, vec2};
use serde::{Deserialize, Serialize};

pub const MAX_THEME_BYTES: usize = 64 * 1024;
pub const MAX_SCHEME_NAME: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Dark,
    Light,
    #[default]
    System,
}
impl ColorMode {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::System];
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "System",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResolvedMode {
    #[default]
    Dark,
    Light,
}
impl ResolvedMode {
    pub fn egui(self) -> egui::Theme {
        match self {
            Self::Dark => egui::Theme::Dark,
            Self::Light => egui::Theme::Light,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

/// A scheme always supplies every color for both modes. Missing colors in an
/// imported file are rejected instead of silently mixing incompatible defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub background: [u8; 3],
    pub sidebar: [u8; 3],
    pub chrome: [u8; 3],
    pub surface: [u8; 3],
    pub hover: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub accent: [u8; 3],
    pub selection: [u8; 3],
    pub folder: [u8; 3],
    pub danger: [u8; 3],
}
impl Palette {
    pub fn graphite() -> Self {
        Self {
            background: [24, 24, 24],
            sidebar: [28, 28, 28],
            chrome: [37, 37, 37],
            surface: [32, 32, 32],
            hover: [45, 45, 45],
            border: [55, 55, 55],
            text: [232, 232, 232],
            muted: [146, 146, 146],
            accent: [236, 236, 236],
            selection: [58, 58, 58],
            folder: [240, 198, 92],
            danger: [240, 108, 108],
        }
    }
    pub fn paper() -> Self {
        Self {
            background: [250, 250, 250],
            sidebar: [243, 243, 243],
            chrome: [236, 236, 236],
            surface: [255, 255, 255],
            hover: [230, 230, 230],
            border: [220, 220, 220],
            text: [24, 24, 24],
            muted: [108, 108, 108],
            accent: [32, 32, 32],
            selection: [224, 224, 224],
            folder: [224, 166, 34],
            danger: [204, 52, 52],
        }
    }
    /// The cyan Default used before the neutral palette. Kept so untouched
    /// copies upgrade; docs/themes/Classic.toml restores it as a named scheme.
    fn classic_graphite() -> Self {
        Self {
            background: [23, 23, 23],
            sidebar: [29, 29, 29],
            chrome: [35, 35, 35],
            surface: [28, 28, 28],
            hover: [45, 51, 53],
            border: [53, 58, 60],
            text: [214, 218, 219],
            muted: [126, 135, 138],
            accent: [23, 161, 204],
            selection: [23, 57, 68],
            folder: [255, 205, 79],
            danger: [234, 111, 117],
        }
    }
    fn classic_paper() -> Self {
        Self {
            background: [246, 247, 248],
            sidebar: [237, 240, 242],
            chrome: [231, 235, 238],
            surface: [250, 251, 252],
            hover: [223, 231, 237],
            border: [207, 214, 220],
            text: [39, 48, 56],
            muted: [112, 123, 132],
            accent: [0, 120, 169],
            selection: [214, 234, 243],
            folder: [218, 164, 28],
            danger: [184, 49, 62],
        }
    }
    fn previous_graphite() -> Self {
        Self {
            background: [26, 28, 29],
            sidebar: [31, 35, 36],
            chrome: [37, 41, 42],
            surface: [30, 34, 35],
            hover: [45, 51, 53],
            border: [53, 58, 60],
            text: [214, 218, 219],
            muted: [126, 135, 138],
            accent: [23, 161, 204],
            selection: [23, 57, 68],
            folder: [255, 205, 79],
            danger: [234, 111, 117],
        }
    }
    /// Exact earlier factory palettes upgrade to the current Default. Any
    /// edited palette stays user-owned.
    fn upgrade_factory(&mut self) {
        if *self == Self::previous_graphite() || *self == Self::classic_graphite() {
            *self = Self::graphite();
        } else if *self == Self::classic_paper() {
            *self = Self::paper();
        }
    }
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            background: theme.background,
            sidebar: theme.sidebar,
            chrome: theme.chrome,
            surface: theme.surface,
            hover: theme.hover,
            border: theme.border,
            text: theme.text,
            muted: theme.muted,
            accent: theme.accent,
            selection: theme.selection,
            folder: theme.folder,
            danger: theme.danger,
        }
    }
    fn apply(&self, theme: &mut Theme) {
        theme.background = self.background;
        theme.sidebar = self.sidebar;
        theme.chrome = self.chrome;
        theme.surface = self.surface;
        theme.hover = self.hover;
        theme.border = self.border;
        theme.text = self.text;
        theme.muted = self.muted;
        theme.accent = self.accent;
        theme.selection = self.selection;
        theme.folder = self.folder;
        theme.danger = self.danger;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorScheme {
    pub name: String,
    pub dark: Palette,
    pub light: Palette,
}
impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            dark: Palette::graphite(),
            light: Palette::paper(),
        }
    }
}
impl ColorScheme {
    pub fn validate(&self) -> Result<(), String> {
        validate_scheme_name(&self.name)
    }
    pub fn read(text: &str) -> Result<Self, String> {
        if text.len() > MAX_THEME_BYTES {
            return Err("Theme file exceeds 64 KiB.".into());
        }
        let scheme: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        scheme.validate()?;
        Ok(scheme)
    }
    /// Imports also accept a complete appearance file or a legacy flat palette.
    /// Explicit paired files remain strict: a missing mode never falls back.
    pub fn read_import(text: &str) -> Result<Self, String> {
        let paired_error = match Self::read(text) {
            Ok(scheme) => return Ok(scheme),
            Err(error) => error,
        };
        if text.len() > MAX_THEME_BYTES {
            return Err(paired_error);
        }
        let value: toml::Value = toml::from_str(text).map_err(|error| error.to_string())?;
        if value.get("dark").is_some() || value.get("light").is_some() {
            return Err(paired_error);
        }
        if value.get("scheme").is_none() {
            if value.get("background").is_none() {
                return Err(paired_error);
            }
            if let Some(name) = value.get("name").and_then(toml::Value::as_str) {
                validate_scheme_name(name)?;
            }
        }
        Theme::read(text)
            .map(|theme| theme.scheme)
            .map_err(|error| error.to_string())
    }
    pub fn write(&self) -> Result<String, String> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|error| error.to_string())
    }
    pub fn palette(&self, mode: ResolvedMode) -> &Palette {
        match mode {
            ResolvedMode::Dark => &self.dark,
            ResolvedMode::Light => &self.light,
        }
    }
}

/// Names are also safe Windows file stems. The library never interprets a name
/// as a path and uses case-insensitive identity, matching the target filesystem.
pub fn validate_scheme_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.chars().count() > MAX_SCHEME_NAME
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        return Err(
            "Use 1–64 letters, numbers, spaces, hyphens or underscores for the theme name.".into(),
        );
    }
    let upper = name.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        return Err("Choose a theme name that is not a Windows device name.".into());
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Legacy active-palette label; color scheme selection uses `scheme.name`.
    pub name: String,
    pub background: [u8; 3],
    pub sidebar: [u8; 3],
    pub chrome: [u8; 3],
    pub surface: [u8; 3],
    pub hover: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub accent: [u8; 3],
    pub selection: [u8; 3],
    pub folder: [u8; 3],
    pub danger: [u8; 3],
    pub font_size: f32,
    /// Independent inspector base size in logical pixels. Local preview zoom is separate.
    pub inspector_font_size: f32,
    pub row_height: f32,
    pub sidebar_width: f32,
    pub radius: u8,
    pub gap: f32,
    pub motion_ms: f32,
    pub reduced_motion: bool,
    pub color_mode: ColorMode,
    pub scheme: ColorScheme,
    /// Rendering-only state. System preference arrives through winit/egui events.
    #[serde(skip)]
    pub resolved_mode: ResolvedMode,
}
impl Default for Theme {
    fn default() -> Self {
        let palette = Palette::graphite();
        Self {
            name: "Graphite".into(),
            background: palette.background,
            sidebar: palette.sidebar,
            chrome: palette.chrome,
            surface: palette.surface,
            hover: palette.hover,
            border: palette.border,
            text: palette.text,
            muted: palette.muted,
            accent: palette.accent,
            selection: palette.selection,
            folder: palette.folder,
            danger: palette.danger,
            font_size: 12.0,
            inspector_font_size: 40.0 / 3.0,
            row_height: 25.0,
            sidebar_width: 280.0,
            radius: 5,
            gap: 8.0,
            motion_ms: 145.0,
            reduced_motion: false,
            color_mode: ColorMode::System,
            scheme: ColorScheme::default(),
            resolved_mode: ResolvedMode::Dark,
        }
    }
}

impl Theme {
    pub fn read(text: &str) -> Result<Self, toml::de::Error> {
        let invalid = |message: String| <toml::de::Error as serde::de::Error>::custom(message);
        if text.len() > MAX_THEME_BYTES {
            return Err(invalid("Theme file exceeds 64 KiB".into()));
        }
        let value: toml::Value = toml::from_str(text)?;
        let paired = value.get("scheme").is_some();
        let mut theme: Self = value.try_into()?;
        for (name, value, min, max) in [
            ("font_size", theme.font_size, 10.0, 24.0),
            ("inspector_font_size", theme.inspector_font_size, 8.0, 28.0),
            ("row_height", theme.row_height, 18.0, 64.0),
            ("sidebar_width", theme.sidebar_width, 160.0, 500.0),
            ("gap", theme.gap, 0.0, 32.0),
            ("motion_ms", theme.motion_ms, 0.0, 1000.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(invalid(format!(
                    "{name} must be finite and between {min} and {max}"
                )));
            }
        }
        if theme.radius > 24 {
            return Err(invalid("radius must be between 0 and 24".into()));
        }
        if paired {
            theme.scheme.validate().map_err(invalid)?;
            // Update the known factory palettes, including the inactive mode.
            // Saved schemes and individually edited palettes retain their colors.
            if theme.scheme.name == "Default" {
                theme.scheme.dark.upgrade_factory();
                theme.scheme.light.upgrade_factory();
            }
            // Retain the last resolved mode for System until the native input
            // supplies a preference. Explicit modes do not depend on luminance.
            theme.resolved_mode = match theme.color_mode {
                ColorMode::Dark => ResolvedMode::Dark,
                ColorMode::Light => ResolvedMode::Light,
                ColorMode::System => mode_for_background(theme.background),
            };
            theme.load_active_palette();
        } else {
            // Only an exact previous factory palette is upgraded. A custom
            // palette remains user-owned even when it is named Graphite.
            let mut palette = Palette::from_theme(&theme);
            if ["Graphite", "Paper", "Default"]
                .iter()
                .any(|name| theme.name.eq_ignore_ascii_case(name))
            {
                palette.upgrade_factory();
                palette.apply(&mut theme);
            }
            theme.resolved_mode = mode_for_background(theme.background);
            theme.color_mode = match theme.resolved_mode {
                ResolvedMode::Dark => ColorMode::Dark,
                ResolvedMode::Light => ColorMode::Light,
            };
            let builtin = palette == Palette::graphite() || palette == Palette::paper();
            if !builtin {
                theme.scheme.name = if validate_scheme_name(&theme.name).is_ok()
                    && !theme.name.eq_ignore_ascii_case("default")
                {
                    theme.name.clone()
                } else {
                    "Custom".into()
                };
            }
            theme.remember_active_palette();
        }
        Ok(theme)
    }
    pub fn write(&self) -> Result<String, toml::ser::Error> {
        let mut saved = self.clone();
        saved.remember_active_palette();
        toml::to_string_pretty(&saved)
    }
    /// Flush legacy flat editing controls into exactly the active mode.
    pub fn remember_active_palette(&mut self) {
        let palette = Palette::from_theme(self);
        match self.resolved_mode {
            ResolvedMode::Dark => self.scheme.dark = palette,
            ResolvedMode::Light => self.scheme.light = palette,
        }
    }
    fn load_active_palette(&mut self) {
        let palette = self.scheme.palette(self.resolved_mode).clone();
        palette.apply(self);
        self.name = if self.scheme.name == "Default" {
            match self.resolved_mode {
                ResolvedMode::Dark => "Graphite".into(),
                ResolvedMode::Light => "Paper".into(),
            }
        } else {
            self.scheme.name.clone()
        };
    }
    pub fn set_color_mode(&mut self, mode: ColorMode, system: Option<egui::Theme>) {
        self.remember_active_palette();
        self.color_mode = mode;
        self.resolved_mode = self.resolve_mode(system);
        self.load_active_palette();
    }
    fn resolve_mode(&self, system: Option<egui::Theme>) -> ResolvedMode {
        match self.color_mode {
            ColorMode::Dark => ResolvedMode::Dark,
            ColorMode::Light => ResolvedMode::Light,
            ColorMode::System => match system {
                Some(egui::Theme::Light) => ResolvedMode::Light,
                Some(egui::Theme::Dark) => ResolvedMode::Dark,
                None => self.resolved_mode,
            },
        }
    }
    /// Constant-time event-state comparison; no registry, timers, or I/O.
    pub fn resolve_system(&mut self, system: Option<egui::Theme>) -> bool {
        let mode = self.resolve_mode(system);
        if mode == self.resolved_mode {
            return false;
        }
        self.remember_active_palette();
        self.resolved_mode = mode;
        self.load_active_palette();
        true
    }
    pub fn set_scheme(&mut self, scheme: ColorScheme) {
        self.scheme = scheme;
        self.load_active_palette();
    }
    pub fn saved_scheme(&self, name: &str) -> Result<ColorScheme, String> {
        let mut saved = self.clone();
        saved.remember_active_palette();
        saved.scheme.name = name.trim().to_owned();
        saved.scheme.validate()?;
        if saved.scheme.name.eq_ignore_ascii_case("default") {
            return Err("Default is always available. Choose another name for your theme.".into());
        }
        Ok(saved.scheme)
    }
    pub fn color(rgb: [u8; 3]) -> Color32 {
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    }
    pub fn bg(&self) -> Color32 {
        Self::color(self.background)
    }
    pub fn fg(&self) -> Color32 {
        Self::color(self.text)
    }
    pub fn dim(&self) -> Color32 {
        Self::color(self.muted)
    }
    pub fn line(&self) -> Color32 {
        Self::color(self.border)
    }
    pub fn accent(&self) -> Color32 {
        Self::color(self.accent)
    }
    pub fn font(&self) -> FontId {
        FontId::proportional(self.font_size)
    }
    pub fn metrics(&self) -> components::Metrics {
        components::Metrics::new(self.font_size)
    }
    pub fn file_row_height(&self) -> f32 {
        self.row_height * self.metrics().scale
    }
    pub fn duration(&self) -> f32 {
        if self.reduced_motion {
            0.0
        } else {
            self.motion_ms / 1000.0
        }
    }
    pub fn apply(&self, ctx: &egui::Context) {
        focus::install(ctx);
        let resolved = self.resolved_mode.egui();
        let mut style = (*ctx.style_of(resolved)).clone();
        // Begin with the correct mode's defaults so secondary egui visuals
        // (shadows, cursors, warnings, disabled text) follow the selected mode.
        style.visuals = resolved.default_visuals();
        style.override_font_id = Some(self.font());
        style.text_styles.insert(egui::TextStyle::Body, self.font());
        style
            .text_styles
            .insert(egui::TextStyle::Button, self.font());
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            FontId::monospace(self.font_size),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            FontId::proportional((self.font_size - 1.0).max(10.0)),
        );
        style.text_styles.insert(
            egui::TextStyle::Heading,
            FontId::proportional(self.font_size + 3.0),
        );
        style.spacing.item_spacing = vec2(self.gap, 3.0);
        style.spacing.button_padding = vec2(9.0, 5.0);
        style.spacing.interact_size = vec2(self.metrics().control, self.metrics().control);
        style.spacing.icon_width = self.metrics().icon;
        style.spacing.icon_spacing = self.metrics().gap;
        style.spacing.window_margin = egui::Margin::same(14);
        style.animation_time = self.duration();
        let v = &mut style.visuals;
        v.dark_mode = self.resolved_mode == ResolvedMode::Dark;
        v.override_text_color = Some(self.fg());
        v.weak_text_color = Some(self.dim());
        v.hyperlink_color = self.accent();
        v.panel_fill = self.bg();
        v.window_fill = Self::color(self.surface);
        v.extreme_bg_color = self.bg();
        v.faint_bg_color = Self::color(self.sidebar);
        v.window_stroke = Stroke::new(1.0, self.line());
        v.window_corner_radius = egui::CornerRadius::same(self.radius.saturating_add(3));
        v.menu_corner_radius = egui::CornerRadius::same(self.radius.saturating_add(2));
        v.selection.bg_fill = Self::color(self.selection);
        v.selection.stroke = Stroke::new(1.0, self.accent());
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.line());
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.dim());
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_fill = Self::color(self.chrome);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.line());
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.fg());
        v.widgets.hovered.weak_bg_fill = Self::color(self.hover);
        v.widgets.hovered.bg_fill = Self::color(self.hover);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, self.accent());
        v.widgets.active.bg_fill = Self::color(self.selection);
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.accent());
        for widget in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.noninteractive,
        ] {
            widget.corner_radius = egui::CornerRadius::same(self.radius);
        }
        ctx.set_theme(resolved);
        ctx.set_style_of(resolved, style);
    }
    pub fn midnight() -> Self {
        let mut theme = Self {
            name: "Midnight".into(),
            background: [20, 23, 31],
            sidebar: [24, 29, 39],
            chrome: [30, 36, 48],
            surface: [25, 30, 40],
            hover: [36, 45, 61],
            border: [46, 56, 73],
            accent: [127, 155, 255],
            selection: [37, 49, 79],
            color_mode: ColorMode::Dark,
            ..Self::default()
        };
        theme.scheme.name = theme.name.clone();
        theme.remember_active_palette();
        theme
    }
    pub fn paper() -> Self {
        let mut theme = Self {
            color_mode: ColorMode::Light,
            resolved_mode: ResolvedMode::Light,
            ..Self::default()
        };
        theme.load_active_palette();
        theme
    }
}

fn mode_for_background(rgb: [u8; 3]) -> ResolvedMode {
    if u32::from(rgb[0]) * 299 + u32::from(rgb[1]) * 587 + u32::from(rgb[2]) * 114 < 128_000 {
        ResolvedMode::Dark
    } else {
        ResolvedMode::Light
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_default_dark_palette_upgrades_only_backgrounds_in_both_modes() {
        let previous = Palette::previous_graphite();
        let mut expected_other_colors = Palette::classic_graphite();
        expected_other_colors.background = previous.background;
        expected_other_colors.sidebar = previous.sidebar;
        expected_other_colors.chrome = previous.chrome;
        expected_other_colors.surface = previous.surface;
        assert_eq!(expected_other_colors, previous);

        for mode in [ColorMode::Dark, ColorMode::Light] {
            let mut theme = Theme {
                font_size: 17.0,
                row_height: 31.0,
                ..Theme::default()
            };
            previous.apply(&mut theme);
            theme.set_color_mode(mode, None);
            let loaded = Theme::read(&theme.write().unwrap()).unwrap();
            assert_eq!(loaded.scheme.dark, Palette::graphite());
            assert_eq!(loaded.scheme.light, Palette::paper());
            assert_eq!((loaded.font_size, loaded.row_height), (17.0, 31.0));
            assert_eq!(loaded.color_mode, mode);
        }

        let flat = format!("name = 'Graphite'\n{}", toml::to_string(&previous).unwrap());
        let loaded = Theme::read(&flat).unwrap();
        assert_eq!(loaded.scheme.name, "Default");
        assert_eq!(loaded.scheme.dark, Palette::graphite());
    }

    #[test]
    fn classic_default_upgrades_to_the_neutral_palette_in_both_modes() {
        for mode in [ColorMode::Dark, ColorMode::Light] {
            let mut theme = Theme::default();
            theme.scheme.dark = Palette::classic_graphite();
            theme.scheme.light = Palette::classic_paper();
            theme.load_active_palette();
            theme.set_color_mode(mode, None);
            assert_ne!(theme.scheme, ColorScheme::default());
            let loaded = Theme::read(&theme.write().unwrap()).unwrap();
            assert_eq!(loaded.scheme, ColorScheme::default());
            assert_eq!(
                loaded.accent,
                loaded.scheme.palette(loaded.resolved_mode).accent
            );
        }
        for (name, palette) in [
            ("Graphite", Palette::classic_graphite()),
            ("Paper", Palette::classic_paper()),
        ] {
            let flat = format!(
                "name = '{name}'
{}",
                toml::to_string(&palette).unwrap()
            );
            let loaded = Theme::read(&flat).unwrap();
            assert_eq!(loaded.scheme, ColorScheme::default());
        }
        // A saved scheme keeps the classic colors.
        let mut theme = Theme::default();
        theme.scheme.name = "Mine".into();
        theme.scheme.dark = Palette::classic_graphite();
        theme.load_active_palette();
        let loaded = Theme::read(&theme.write().unwrap()).unwrap();
        assert_eq!(loaded.scheme.dark, Palette::classic_graphite());
    }

    #[test]
    fn classic_backup_file_restores_the_previous_default() {
        let scheme = ColorScheme::read(include_str!("../../../docs/themes/Classic.toml")).unwrap();
        assert_eq!(scheme.name, "Classic");
        assert_eq!(scheme.dark, Palette::classic_graphite());
        assert_eq!(scheme.light, Palette::classic_paper());
    }

    #[test]
    fn background_upgrade_preserves_custom_palettes_and_named_schemes() {
        for custom_palette in [false, true] {
            let mut theme = Theme::default();
            let mut palette = Palette::previous_graphite();
            if custom_palette {
                palette.surface = [27, 30, 31];
            } else {
                theme.scheme.name = "Saved theme".into();
            }
            palette.apply(&mut theme);
            let loaded = Theme::read(&theme.write().unwrap()).unwrap();
            assert_eq!(loaded.scheme.dark, palette);
            assert_eq!(loaded.scheme.name, theme.scheme.name);
            assert_eq!(loaded.scheme.light, Palette::paper());
        }
    }

    #[test]
    fn modes_keep_independent_edits_and_preserve_layout_and_motion() {
        let mut theme = Theme {
            font_size: 17.0,
            inspector_font_size: 19.0,
            row_height: 33.0,
            sidebar_width: 320.0,
            radius: 9,
            gap: 11.0,
            motion_ms: 200.0,
            reduced_motion: true,
            ..Theme::default()
        };
        theme.surface = [1, 2, 3];
        theme.set_color_mode(ColorMode::Light, None);
        assert_eq!(theme.surface, Palette::paper().surface);
        theme.surface = [210, 220, 230];
        theme.set_color_mode(ColorMode::Dark, None);
        assert_eq!(theme.surface, [1, 2, 3]);
        theme.set_color_mode(ColorMode::System, Some(egui::Theme::Light));
        assert_eq!(theme.surface, [210, 220, 230]);
        let saved = Theme::read(&theme.write().unwrap()).unwrap();
        assert_eq!(saved.scheme.dark.surface, [1, 2, 3]);
        assert_eq!(saved.scheme.light.surface, [210, 220, 230]);
        assert_eq!(
            (
                saved.font_size,
                saved.inspector_font_size,
                saved.row_height,
                saved.sidebar_width,
                saved.radius,
                saved.gap,
                saved.motion_ms,
                saved.reduced_motion
            ),
            (17.0, 19.0, 33.0, 320.0, 9, 11.0, 200.0, true)
        );
    }

    #[test]
    fn legacy_custom_named_graphite_is_preserved_in_migration() {
        let theme = Theme::read(
            "name = 'Graphite'\nsurface = [11, 22, 33]\nfont_size = 16\nreduced_motion = true",
        )
        .unwrap();
        assert_eq!(theme.color_mode, ColorMode::Dark);
        assert_eq!(theme.scheme.dark.surface, [11, 22, 33]);
        assert_eq!(theme.scheme.light, Palette::paper());
        assert_eq!(theme.scheme.name, "Graphite");
        assert_eq!(theme.font_size, 16.0);
        assert!(theme.reduced_motion);
        let saved = Theme::read(&theme.write().unwrap()).unwrap();
        assert_eq!(saved.scheme, theme.scheme);
    }

    #[test]
    fn legacy_light_and_unsafe_names_migrate_without_losing_colors() {
        let theme =
            Theme::read("name = '../custom'\nbackground = [240, 230, 220]\naccent = [7, 8, 9]")
                .unwrap();
        assert_eq!(theme.color_mode, ColorMode::Light);
        assert_eq!(theme.scheme.name, "Custom");
        assert_eq!(theme.scheme.light.accent, [7, 8, 9]);
        assert_eq!(theme.scheme.dark, Palette::graphite());
    }

    #[test]
    fn system_changes_are_idempotent_and_egui_uses_the_resolved_mode() {
        let ctx = egui::Context::default();
        let mut theme = Theme::default();
        assert!(theme.resolve_system(Some(egui::Theme::Light)));
        assert!(!theme.resolve_system(Some(egui::Theme::Light)));
        theme.apply(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert!(!ctx.style_of(egui::Theme::Light).visuals.dark_mode);
        theme.set_color_mode(ColorMode::Dark, Some(egui::Theme::Light));
        assert!(!theme.resolve_system(Some(egui::Theme::Light)));
        theme.apply(&ctx);
        assert!(ctx.style_of(egui::Theme::Dark).visuals.dark_mode);
    }

    #[test]
    fn scheme_files_require_both_complete_bounded_palettes() {
        let scheme = ColorScheme::default();
        assert_eq!(ColorScheme::read(&scheme.write().unwrap()).unwrap(), scheme);
        assert!(ColorScheme::read("name = 'Incomplete'\n[dark]\nbackground = [1,2,3]").is_err());
        assert!(ColorScheme::read(&"x".repeat(MAX_THEME_BYTES + 1)).is_err());
        for name in ["../escape", "CON", "LPT1", "", " padded", "bad:name"] {
            assert!(validate_scheme_name(name).is_err(), "{name}");
        }
        assert!(Theme::default().saved_scheme("Default").is_err());
    }
    #[test]
    fn imports_accept_legacy_and_full_appearance_files_but_validate_names() {
        let legacy = ColorScheme::read_import(
            "name = 'Old Ocean'\nbackground = [10, 20, 30]\naccent = [7, 8, 9]",
        )
        .unwrap();
        assert_eq!(legacy.name, "Old Ocean");
        assert_eq!(legacy.dark.accent, [7, 8, 9]);
        assert_eq!(legacy.light, Palette::paper());
        let theme = Theme::midnight();
        assert_eq!(
            ColorScheme::read_import(&theme.write().unwrap()).unwrap(),
            theme.scheme
        );
        assert!(ColorScheme::read_import("name = '../bad'\nbackground = [10, 20, 30]").is_err());
        assert!(
            ColorScheme::read_import("name = 'Missing mode'\n[dark]\nbackground = [10, 20, 30]")
                .is_err()
        );
    }
}
