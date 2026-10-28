//! Bounded native inspector. Feature names follow the REA inspector inventory;
//! exact source control placement and filter semantics remain reconstruction choices.
//! The one latest-only worker owns every file read, directory enumeration, decode,
//! text filtering and wrapping operation. Painting touches only visible rows.
//!
//! Limits: 256 KiB text input, 128 Ki displayed characters, 8192 visual rows,
//! 2048 immediate folder children/512 KiB names, 16 MiB image file, 64 MiB decoded
//! pixels and a 1600-pixel preview edge. These are explicit preview limits, not
//! recursive folder totals or a claim of full-resolution image rendering.
use egui::{
    Color32, ColorImage, Context, FontId, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, TextureWrapMode, Ui, UiBuilder, Vec2, epaint::Vertex, pos2,
    vec2,
};
use foldex_ui::{Icon, Theme, icon, icon_button, motion::MotionProfile};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::{
    collections::HashMap,
    fs::{self, File, Metadata},
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TEXT_BYTES: usize = 256 * 1024;
const DISPLAY_CHARS: usize = 128 * 1024;
const DISPLAY_ROWS: usize = 8192;
const LINE_CHARS: usize = 2048;
const FOLDER_ITEMS: usize = 2048;
const FOLDER_NAME_BYTES: usize = 512 * 1024;
const IMAGE_FILE_BYTES: usize = 16 * 1024 * 1024;
const IMAGE_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const IMAGE_EDGE: u32 = 1600;
const LAYOUT_CHARS: usize = 8192;
const LAYOUT_ENTRIES: usize = 128;
const RETRY: Duration = Duration::from_millis(16);

fn inspector_control(response: egui::Response, ui: &Ui) -> egui::Response {
    foldex_ui::components::register_focus_target(ui, response.rect, response.id);
    crate::keyboard::register(
        ui,
        crate::keyboard::Region::Inspector,
        response.id,
        response.rect,
    );
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        ui.scroll_to_rect(response.rect.expand(3.0), None);
    }
    response
}

fn preview_cursor(current: usize, count: usize, key: egui::Key, page: usize) -> usize {
    let last = count.saturating_sub(1);
    match key {
        egui::Key::ArrowUp => current.saturating_sub(1),
        egui::Key::ArrowDown => current.saturating_add(1).min(last),
        egui::Key::PageUp => current.saturating_sub(page),
        egui::Key::PageDown => current.saturating_add(page).min(last),
        egui::Key::Home => 0,
        egui::Key::End => last,
        _ => current.min(last),
    }
    .min(last)
}

fn preview_scroll(ctx: &Context, mut offset: Vec2, line: Vec2, page: Vec2, max: Vec2) -> Vec2 {
    for (key, direction) in [
        (egui::Key::ArrowLeft, vec2(-1.0, 0.0)),
        (egui::Key::ArrowRight, vec2(1.0, 0.0)),
        (egui::Key::ArrowUp, vec2(0.0, -1.0)),
        (egui::Key::ArrowDown, vec2(0.0, 1.0)),
    ] {
        if crate::commands::take_key(ctx, egui::Modifiers::NONE, key) {
            offset += direction * line;
        }
    }
    if crate::commands::take_key(ctx, egui::Modifiers::NONE, egui::Key::PageUp) {
        offset.y -= page.y;
    }
    if crate::commands::take_key(ctx, egui::Modifiers::NONE, egui::Key::PageDown) {
        offset.y += page.y;
    }
    if crate::commands::take_key(ctx, egui::Modifiers::NONE, egui::Key::Home) {
        offset.y = 0.0;
    }
    if crate::commands::take_key(ctx, egui::Modifiers::NONE, egui::Key::End) {
        offset.y = max.y;
    }
    offset.clamp(Vec2::ZERO, max.max(Vec2::ZERO))
}

#[derive(Default)]
pub struct InspectorResponse {
    pub close: bool,
    /// Open through the associated app.
    pub open: Option<PathBuf>,
    /// Resolve this path in the active file panel.
    pub go_to: Option<PathBuf>,
    pub reveal: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextEncoding {
    #[default]
    Auto,
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}
impl TextEncoding {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Windows1252 => "Windows-1252",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Options {
    encoding: TextEncoding,
    filter: String,
    wrap_columns: Option<usize>,
    whitespace: bool,
    descending: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            encoding: TextEncoding::Auto,
            filter: String::new(),
            wrap_columns: Some(40),
            whitespace: false,
            descending: false,
        }
    }
}

struct Request {
    id: u64,
    path: Option<PathBuf>,
    options: Options,
    force: bool,
}
#[derive(Default)]
struct Mailbox {
    request: Option<Request>,
    reply: Option<Reply>,
}
struct Shared {
    mailbox: Mutex<Mailbox>,
    worker_thread: OnceLock<thread::Thread>,
    current: AtomicU64,
    image_ack: AtomicU64,
    clear: AtomicBool,
    closed: AtomicBool,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Shared {
    fn notify(&self) {
        if let Some(worker) = self.worker_thread.get() {
            worker.unpark();
        }
    }
}
struct Reply {
    id: u64,
    path: PathBuf,
    data: Data,
}

#[derive(Clone, Debug)]
struct FileInfo {
    availability: foldex_fs::capabilities::Availability,
    bytes: u64,
    directory: bool,
    link: bool,
    read_only: bool,
    modified: Option<String>,
    created: Option<String>,
    accessed: Option<String>,
}
impl FileInfo {
    fn new(metadata: &Metadata, caps: foldex_fs::capabilities::Capabilities) -> Self {
        Self {
            bytes: metadata.len(),
            directory: metadata.is_dir(),
            availability: caps.availability(),
            link: caps.redirects() || caps.unknown_reparse(),
            read_only: metadata.permissions().readonly(),
            modified: metadata.modified().ok().map(format_time),
            created: metadata.created().ok().map(format_time),
            accessed: metadata.accessed().ok().map(format_time),
        }
    }
}

struct TextRow {
    number: usize,
    continued: bool,
    text: String,
}
struct TextData {
    rows: Arc<[TextRow]>,
    total_lines: usize,
    matched_lines: usize,
    max_columns: usize,
    limited: bool,
    encoding: &'static str,
    replacements: bool,
}
struct FolderItem {
    name: String,
    raw_name: std::ffi::OsString,
    directory: bool,
    link: bool,
}
struct FolderData {
    items: Arc<[FolderItem]>,
    indices: Arc<[u32]>,
    limited: bool,
    skipped: usize,
}
struct ImagePixels {
    size: [usize; 2],
    rgba: Vec<u8>,
}

enum Data {
    Text {
        info: Arc<FileInfo>,
        model: TextData,
    },
    Folder {
        info: Arc<FileInfo>,
        model: FolderData,
    },
    Image {
        info: Arc<FileInfo>,
        original: [u32; 2],
        preview: [usize; 2],
        pixels: Option<Arc<ImagePixels>>,
    },
    Info {
        info: Option<Arc<FileInfo>>,
        message: String,
    },
    Binary {
        info: Arc<FileInfo>,
        message: String,
    },
}
impl Data {
    fn info(&self) -> Option<&FileInfo> {
        match self {
            Self::Text { info, .. }
            | Self::Folder { info, .. }
            | Self::Image { info, .. }
            | Self::Binary { info, .. } => Some(info),
            Self::Info { info, .. } => info.as_deref(),
        }
    }
}

enum Source {
    Text {
        info: Arc<FileInfo>,
        bytes: Vec<u8>,
        limited: bool,
    },
    Folder {
        info: Arc<FileInfo>,
        items: Arc<[FolderItem]>,
        limited: bool,
        skipped: usize,
    },
    Image {
        info: Arc<FileInfo>,
        original: [u32; 2],
        preview: [usize; 2],
        pixels: Option<Arc<ImagePixels>>,
        sent: u64,
    },
    Info {
        info: Option<Arc<FileInfo>>,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageScale {
    Fit,
    Original,
    Custom,
}
struct RowLayout {
    galley: Arc<egui::Galley>,
    number: Arc<egui::Galley>,
    chars: usize,
    touched: u64,
    first_column: usize,
}

pub struct Inspector {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    path: Option<PathBuf>,
    generation: u64,
    pending: Option<Request>,
    content: Option<Data>,
    texture: Option<TextureHandle>,
    checker: Option<TextureHandle>,
    checker_style: ([u8; 3], [u8; 3]),
    options: Options,
    loading: bool,
    details: bool,
    line_numbers: bool,
    focus_filter: bool,
    encoding_menu: foldex_ui::components::MenuState,
    encoding_anchor: Rect,
    body_scroll_reset: bool,
    preview_opacity: f32,
    folder_motion: foldex_ui::motion::LayoutMotion,
    folder_arrivals: bool,
    font_size: f32,
    theme_font_size: f32,
    font_zoom_delta: f32,
    image_scale: ImageScale,
    image_zoom: f32,
    visual_zoom: f32,
    image_initial: bool,
    rotation: f32,
    visual_rotation: f32,
    image_grid: bool,
    image_grid_opacity: f32,
    image_minimap: bool,
    image_offset: Vec2,
    image_draw_size: Vec2,
    image_viewport: Vec2,
    image_recenter: bool,
    image_map_opacity: f32,
    layouts: HashMap<(usize, usize, usize), RowLayout>,
    layout_chars: usize,
    layout_tick: u64,
    layout_style: (u32, [u8; 3], u32, usize),
    focus_rect: Rect,
    folder_selected: usize,
    folder_reveal: bool,
    text_offset: Vec2,
    encoding_return: Option<egui::Id>,
    composing: bool,
    ime_frame: Option<u64>,
    ime_input: bool,
}

impl Inspector {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            worker_thread: OnceLock::new(),
            current: AtomicU64::new(0),
            image_ack: AtomicU64::new(0),
            clear: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            wake,
        });
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("foldex-inspector".into())
            .stack_size(384 * 1024)
            .spawn(move || worker_loop(state))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            path: None,
            generation: 0,
            pending: None,
            content: None,
            texture: None,
            checker: None,
            checker_style: ([0; 3], [0; 3]),
            options: Options::default(),
            loading: false,
            details: false,
            line_numbers: true,
            focus_filter: false,
            encoding_menu: Default::default(),
            encoding_anchor: Rect::NOTHING,
            body_scroll_reset: true,
            preview_opacity: 0.0,
            folder_motion: foldex_ui::motion::LayoutMotion::new(256),
            folder_arrivals: false,
            font_size: 40.0 / 3.0,
            theme_font_size: 40.0 / 3.0,
            font_zoom_delta: 0.0,
            image_scale: ImageScale::Fit,
            image_zoom: 1.0,
            visual_zoom: 1.0,
            image_initial: true,
            rotation: 0.0,
            visual_rotation: 0.0,
            image_grid: true,
            image_grid_opacity: 1.0,
            image_minimap: true,
            image_offset: Vec2::ZERO,
            image_draw_size: Vec2::ZERO,
            image_viewport: Vec2::ZERO,
            image_recenter: true,
            image_map_opacity: 0.0,
            layouts: HashMap::new(),
            layout_chars: 0,
            layout_tick: 0,
            layout_style: (0, [0; 3], 0, 0),
            focus_rect: Rect::NOTHING,
            folder_selected: 0,
            folder_reveal: false,
            text_offset: Vec2::ZERO,
            encoding_return: None,
            composing: false,
            ime_frame: None,
            ime_input: false,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn has_popup(&self) -> bool {
        self.encoding_menu.active()
    }
    pub fn close_popups(&mut self) {
        self.encoding_menu = Default::default();
    }
    pub fn owns_navigation(&self, ctx: &Context) -> bool {
        self.has_popup()
            || crate::keyboard::owns(ctx, crate::keyboard::Region::Inspector)
            || ctx
                .memory(|memory| memory.focused())
                .and_then(|id| ctx.read_response(id))
                .is_some_and(|response| self.focus_rect.contains(response.rect.center()))
    }

    pub fn open_path(&mut self, path: PathBuf) -> io::Result<bool> {
        validate_path(&path)?;
        if self.path.as_ref() == Some(&path) {
            return Ok(false);
        }
        self.encoding_menu = Default::default();
        self.body_scroll_reset = true;
        self.preview_opacity = 0.0;
        self.folder_motion.clear();
        self.path = Some(path);
        self.content = None;
        self.texture = None;
        self.clear_layouts();
        self.rotation = 0.0;
        self.visual_rotation = 0.0;
        self.image_grid_opacity = if self.image_grid { 1.0 } else { 0.0 };
        self.image_scale = ImageScale::Fit;
        self.image_offset = Vec2::ZERO;
        self.image_draw_size = Vec2::ZERO;
        self.image_viewport = Vec2::ZERO;
        self.image_recenter = true;
        self.image_map_opacity = 0.0;
        self.visual_zoom = 1.0;
        self.image_initial = true;
        self.folder_selected = 0;
        self.folder_reveal = false;
        self.text_offset = Vec2::ZERO;
        self.schedule(true);
        Ok(true)
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "inspector worker closed",
            ));
        }
        if self.path.is_some() {
            self.schedule(true);
        }
        Ok(())
    }

    pub fn close(&mut self) {
        self.focus_rect = Rect::NOTHING;
        self.encoding_menu = Default::default();
        if self.path.is_none() && self.content.is_none() && self.pending.is_none() {
            return;
        }
        self.path = None;
        self.content = None;
        self.texture = None;
        self.checker = None;
        self.loading = false;
        self.clear_layouts();
        self.shared.clear.store(true, Ordering::Release);
        self.schedule(false);
        self.shared.notify();
    }

    pub fn set_details(&mut self, details: bool) {
        self.details = details;
    }
    pub fn toggle_details(&mut self) {
        self.details = !self.details;
    }
    pub fn focus_filter(&mut self) {
        self.focus_filter = true;
    }
    pub fn toggle_wrap(&mut self) {
        self.options.wrap_columns = if self.options.wrap_columns.is_some() {
            None
        } else {
            Some(40)
        };
        self.rebuild_text();
    }
    pub fn toggle_line_numbers(&mut self) {
        self.line_numbers = !self.line_numbers;
    }
    pub fn toggle_whitespace(&mut self) {
        self.options.whitespace = !self.options.whitespace;
        self.rebuild_text();
    }
    pub fn toggle_image_grid(&mut self) {
        self.image_grid = !self.image_grid;
    }
    pub fn toggle_image_minimap(&mut self) {
        self.image_minimap = !self.image_minimap;
    }
    pub fn toggle_image_scale(&mut self) {
        self.image_scale = if self.image_scale == ImageScale::Fit {
            ImageScale::Original
        } else {
            ImageScale::Fit
        };
    }
    pub fn change_contents_size(&mut self, increase: bool) {
        if matches!(self.content, Some(Data::Image { .. })) {
            let zoom = match self.image_scale {
                ImageScale::Fit => self.visual_zoom,
                ImageScale::Original => 1.0,
                ImageScale::Custom => self.image_zoom,
            };
            self.image_zoom = (zoom * if increase { 1.25 } else { 0.8 }).clamp(0.02, 16.0);
            self.image_scale = ImageScale::Custom;
        } else {
            let next = (self.font_size + if increase { 1.0 } else { -1.0 }).clamp(8.0, 28.0);
            if next != self.font_size {
                self.font_zoom_delta = next - self.theme_font_size;
            }
            self.font_size = next;
            self.clear_layouts();
        }
    }

    fn rebuild_text(&mut self) {
        if matches!(self.content, Some(Data::Text { .. } | Data::Binary { .. })) {
            self.schedule(false);
        }
    }
    fn clear_layouts(&mut self) {
        self.layouts.clear();
        self.layout_chars = 0;
    }
    fn sync_theme_font(&mut self, theme: &Theme) {
        if self.theme_font_size.to_bits() != theme.inspector_font_size.to_bits() {
            self.theme_font_size = theme.inspector_font_size;
            self.font_size = (self.theme_font_size + self.font_zoom_delta).clamp(8.0, 28.0);
            self.clear_layouts();
        }
    }
    fn trim_layouts(&mut self, count: usize) {
        while self.layouts.len() >= LAYOUT_ENTRIES
            || self.layout_chars.saturating_add(count) > LAYOUT_CHARS
        {
            let old = self
                .layouts
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| *key);
            let Some(old) = old else {
                break;
            };
            self.layout_chars -= self.layouts.remove(&old).unwrap().chars;
        }
    }
    fn schedule(&mut self, force: bool) {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.shared
            .current
            .store(self.generation, Ordering::Release);
        self.pending = Some(Request {
            id: self.generation,
            path: self.path.clone(),
            options: self.options.clone(),
            force,
        });
        self.loading = self.path.is_some();
        self.flush_request();
    }
    fn flush_request(&mut self) {
        if self.pending.is_none() {
            return;
        }
        if let Ok(mut mailbox) = self.shared.mailbox.try_lock() {
            mailbox.request = self.pending.take();
            drop(mailbox);
            self.shared.notify();
        }
    }
    fn poll(&mut self, ctx: &Context) {
        self.flush_request();
        if self.pending.is_some() {
            ctx.request_repaint_after(RETRY);
        }
        let reply = self
            .shared
            .mailbox
            .try_lock()
            .ok()
            .and_then(|mut mailbox| mailbox.reply.take());
        let Some(mut reply) = reply else {
            return;
        };
        if reply.id != self.generation || self.path.as_ref() != Some(&reply.path) {
            return;
        }
        if let Data::Image {
            pixels, preview, ..
        } = &mut reply.data
        {
            if let Some(image) = pixels.take() {
                let bytes = preview[0]
                    .checked_mul(preview[1])
                    .and_then(|size| size.checked_mul(4));
                if bytes == Some(image.rgba.len())
                    && image.size == *preview
                    && preview
                        .iter()
                        .all(|edge| *edge > 0 && *edge <= IMAGE_EDGE as usize)
                {
                    self.texture = Some(ctx.load_texture(
                        "inspector-image",
                        ColorImage::from_rgba_unmultiplied(*preview, &image.rgba),
                        TextureOptions::LINEAR,
                    ));
                }
                // The worker retains the final pixel Arc until this explicit ack.
                // Its final 10 MiB pixel-buffer destruction happens on that worker.
                drop(image);
                self.shared.image_ack.store(reply.id, Ordering::Release);
                self.shared.notify();
            }
        } else {
            self.texture = None;
        }
        self.content = Some(reply.data);
        self.folder_arrivals = matches!(self.content, Some(Data::Folder { .. }));
        self.loading = false;
        self.clear_layouts();
    }

    pub fn ui(&mut self, ui: &mut Ui, rect: Rect, theme: &Theme) -> InspectorResponse {
        self.sync_theme_font(theme);
        self.poll(ui.ctx());
        let mut response = InspectorResponse::default();
        self.focus_rect = rect;
        let frame = ui.ctx().cumulative_frame_nr();
        if self.ime_frame != Some(frame) {
            self.ime_frame = Some(frame);
            self.ime_input = self.composing;
            ui.input(|input| {
                for event in &input.events {
                    if let egui::Event::Ime(event) = event {
                        self.ime_input = true;
                        match event {
                            egui::ImeEvent::Preedit { text, .. } => {
                                self.composing = !text.is_empty()
                            }
                            egui::ImeEvent::Commit(_) => self.composing = false,
                            _ => {}
                        }
                    }
                }
            });
        }
        if !self.has_popup() && !self.ime_input && self.owns_navigation(ui.ctx()) {
            let focused = ui.memory(|memory| memory.focused());
            if focused != Some(egui::Id::new("inspector-image-map"))
                && !ui.ctx().text_edit_focused()
                && crate::commands::take_key(ui.ctx(), egui::Modifiers::NONE, egui::Key::Escape)
            {
                response.close = true;
            }
        }
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0, Theme::color(theme.sidebar));
        painter.line_segment(
            [rect.left_top(), rect.left_bottom()],
            Stroke::new(1.0, theme.line()),
        );
        if rect.width() < 80.0 || rect.height() < 80.0 {
            return response;
        }
        let m = theme.metrics();
        if foldex_ui::motion::MotionGains::new(ui.input(|input| input.stable_dt), theme.duration())
            .step_normalized(
                &mut self.preview_opacity,
                if self.content.is_some() { 1.0 } else { 0.0 },
                MotionProfile::Standard,
            )
        {
            ui.ctx().request_repaint();
        }
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), m.toolbar));
        let right = header.right() - m.inset - 3.0 * m.control;
        for (n, glyph, hint) in [
            (2, Icon::Refresh, "Reload preview"),
            (1, Icon::More, "Extra file details"),
            (0, Icon::Close, "Close inspector"),
        ] {
            let control = Rect::from_center_size(
                pos2(right + (2.5 - n as f32) * m.control, header.center().y),
                vec2(m.control, m.control),
            );
            let hit = inspector_control(
                foldex_ui::components::icon_button_at(
                    ui,
                    control,
                    egui::Id::new(("inspector-header", n)),
                    theme,
                    glyph,
                    n == 1 && self.details || n == 2 && self.loading,
                ),
                ui,
            )
            .on_hover_text(if n == 2 && self.loading {
                "Updating preview…"
            } else {
                hint
            });
            foldex_ui::components::label_control(&hit, hint);
            if hit.clicked() {
                match n {
                    0 => response.close = true,
                    1 => self.toggle_details(),
                    _ => {
                        let _ = self.refresh();
                    }
                }
            }
        }
        foldex_ui::components::elided_label(
            &painter,
            Rect::from_min_max(
                pos2(header.left() + m.inset, header.top()),
                pos2(
                    (right - m.gap).max(header.left() + m.inset),
                    header.bottom(),
                ),
            ),
            theme,
            "Inspector",
            theme.dim(),
            false,
        );
        let body = Rect::from_min_max(
            pos2(rect.left() + m.inset, header.bottom() + m.gap),
            pos2(rect.right() - m.inset, rect.bottom() - m.inset),
        );
        if body.height() < 1.0 {
            return response;
        }
        ui.scope_builder(UiBuilder::new().id_salt("inspector").max_rect(body), |ui| {
            ui.set_clip_rect(body.intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing = vec2(m.gap, m.gap);
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("inspector-body-scroll")
                .auto_shrink([false, false])
                .max_height(body.height());
            if self.body_scroll_reset {
                scroll = scroll.vertical_scroll_offset(0.0);
                self.body_scroll_reset = false;
            }
            scroll.show(ui, |ui| {
                let content_origin = ui.cursor().top();
                let Some(path) = self.path.clone() else {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("Select an item to inspect it").color(theme.dim()),
                    );
                    return;
                };
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy();
                foldex_ui::fonts::request_text(ui.ctx(), &name);
                ui.add_space(m.gap);
                let title = ui.add(
                    egui::Label::new(egui::RichText::new(name).strong().color(theme.fg()))
                        .truncate(),
                );
                if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
                    title.on_hover_text(path.to_string_lossy());
                }
                ui.horizontal_wrapped(|ui| {
                    if inspector_control(
                        foldex_ui::components::text_button(ui, theme, "Open", false),
                        ui,
                    )
                    .on_hover_text("Open with the associated app")
                    .clicked()
                    {
                        response.open = Some(path.clone());
                    }
                    if inspector_control(
                        foldex_ui::components::text_button(ui, theme, "Show in panel", false),
                        ui,
                    )
                    .clicked()
                    {
                        response.go_to = Some(path.clone());
                    }
                    if inspector_control(
                        foldex_ui::components::text_button(ui, theme, "Reveal", false),
                        ui,
                    )
                    .on_hover_text("Show in Windows File Explorer")
                    .clicked()
                    {
                        response.reveal = Some(path.clone());
                    }
                    if self.loading && self.content.is_none() {
                        ui.label(egui::RichText::new("Loading…").small().color(theme.dim()));
                    }
                });
                if let Some(info) = self.content.as_ref().and_then(Data::info) {
                    ui.label(
                        egui::RichText::new(if info.directory {
                            "Folder".into()
                        } else {
                            foldex_ui::format_size(info.bytes)
                        })
                        .small()
                        .color(theme.dim()),
                    );
                    foldex_ui::components::reveal(
                        ui,
                        egui::Id::new("inspector-details"),
                        theme,
                        self.details,
                        |ui| metadata_ui(ui, info, theme),
                    );
                }
                ui.add_space(5.0);
                ui.separator();
                let text = matches!(self.content, Some(Data::Text { .. }));
                let folder = matches!(self.content, Some(Data::Folder { .. }));
                let image = matches!(self.content, Some(Data::Image { .. }));
                let binary = matches!(self.content, Some(Data::Binary { .. }));
                if text || folder {
                    self.filter_ui(ui, theme, text);
                }
                if text || binary {
                    self.text_controls(ui, theme);
                }
                if image {
                    self.image_controls(ui, theme);
                }
                ui.add_space(4.0);
                let used = ui.cursor().top() - content_origin;
                let height = (body.height() - used - 1.0).max(120.0 * m.scale);
                let (content_rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                let mut preview = ui.new_child(
                    UiBuilder::new()
                        .id_salt("inspector-preview")
                        .max_rect(content_rect),
                );
                preview.multiply_opacity(self.preview_opacity);
                preview.set_clip_rect(content_rect.intersect(ui.clip_rect()));
                if text {
                    self.text_ui(&mut preview, content_rect, theme);
                } else if folder {
                    self.folder_ui(&mut preview, content_rect, theme, &path, &mut response);
                } else if image {
                    self.image_ui(&mut preview, content_rect, theme);
                } else if let Some(Data::Info { message, .. } | Data::Binary { message, .. }) =
                    &self.content
                {
                    let text = match message.strip_prefix("Preview unavailable: ") {
                        Some(detail) => crate::friendly::present("Preview unavailable", detail),
                        None if message.starts_with("This location can't be previewed: ") => {
                            crate::friendly::with_detail(
                                "This location can't be previewed.",
                                message,
                            )
                        }
                        None => crate::friendly::present_authored("Preview unavailable", message),
                    };
                    preview.label(egui::RichText::new(text).color(theme.dim()));
                }
            });
        });
        if response.close {
            self.encoding_menu = Default::default();
        }
        let encodings = [
            TextEncoding::Auto,
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Windows1252,
        ];
        let items: [foldex_ui::components::MenuItem<'_>; 5] =
            std::array::from_fn(|i| foldex_ui::components::MenuItem {
                id: i as u32,
                label: encodings[i].label(),
                shortcut: "",
                glyph: Icon::Empty,
                checked: Some(self.options.encoding == encodings[i]),
                enabled: true,
            });
        if let Some(choice) = self.encoding_menu.show_width(
            ui.ctx(),
            egui::Id::new("inspector-encoding-menu"),
            theme,
            self.encoding_anchor,
            &items,
            200.0 * m.scale,
        ) {
            self.options.encoding = encodings[choice as usize];
            self.schedule(false);
        }
        if !self.encoding_menu.active()
            && let Some(id) = self.encoding_return.take()
        {
            crate::keyboard::request_id(ui.ctx(), id);
        }
        response
    }

    fn filter_ui(&mut self, ui: &mut Ui, theme: &Theme, text: bool) {
        let hint = if text {
            "Filter text lines…"
        } else {
            "Filter folder contents…"
        };
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), theme.metrics().control),
            Sense::hover(),
        );
        let filter_id = egui::Id::new("inspector-filter");
        let leaving_filter = !self.ime_input && ui.memory(|memory| memory.has_focus(filter_id))
            && ui.input(|input| input.events.iter().any(|event| matches!(event,
                egui::Event::Key { key: egui::Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none())));
        let edit = foldex_ui::components::search_input_clearable(
            ui,
            rect,
            egui::Id::new("inspector-filter"),
            theme,
            &mut self.options.filter,
            hint,
            256,
        )
        .response;
        foldex_ui::components::label_control(&edit, hint);
        crate::keyboard::register(ui, crate::keyboard::Region::Inspector, edit.id, edit.rect);
        let clear_id = egui::Id::new("inspector-filter").with("clear");
        if let Some(clear) = ui.ctx().read_response(clear_id) {
            crate::keyboard::register(ui, crate::keyboard::Region::Inspector, clear.id, clear.rect);
        }
        if self.focus_filter && foldex_ui::components::can_focus(ui) {
            edit.request_focus();
            self.focus_filter = ui.is_sizing_pass() || !edit.has_focus();
        }
        if edit.changed() {
            self.schedule(false);
        }
        if leaving_filter {
            crate::keyboard::request_id(
                ui.ctx(),
                egui::Id::new(if text {
                    "inspector-text-focus"
                } else {
                    "inspector-folder-focus"
                }),
            );
        }
    }

    fn text_controls(&mut self, ui: &mut Ui, theme: &Theme) {
        let mut rebuild = false;
        ui.horizontal_wrapped(|ui| {
            let mut wrap = self.options.wrap_columns.is_some();
            if inspector_control(
                foldex_ui::components::text_button(ui, theme, "Wrap", wrap),
                ui,
            )
            .on_hover_text("Word wrap")
            .clicked()
            {
                wrap = !wrap;
                self.options.wrap_columns = if wrap { Some(40) } else { None };
                rebuild = true;
            }
            if inspector_control(
                foldex_ui::components::text_button(ui, theme, "123", self.line_numbers),
                ui,
            )
            .on_hover_text("Line numbers")
            .clicked()
            {
                self.line_numbers = !self.line_numbers;
            }
            if inspector_control(
                foldex_ui::components::text_button(ui, theme, "· →", self.options.whitespace),
                ui,
            )
            .on_hover_text("Show whitespace")
            .clicked()
            {
                self.options.whitespace = !self.options.whitespace;
                rebuild = true;
            }
            for (larger, glyph, label) in [
                (false, Icon::Minus, "Smaller text"),
                (true, Icon::Plus, "Larger text"),
            ] {
                let m = theme.metrics();
                let (rect, _) = ui.allocate_exact_size(vec2(m.control, m.control), Sense::hover());
                let response = foldex_ui::components::icon_button_at(
                    ui,
                    rect,
                    egui::Id::new(("inspector-text-size", larger)),
                    theme,
                    glyph,
                    false,
                )
                .on_hover_text(label);
                crate::keyboard::register(
                    ui,
                    crate::keyboard::Region::Inspector,
                    response.id,
                    response.rect,
                );
                let enabled = foldex_ui::components::can_focus(ui);
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
                });
                if response.clicked() {
                    self.change_contents_size(larger);
                }
            }
            let encoding = foldex_ui::components::select_button(
                ui,
                theme,
                self.options.encoding.label(),
                120.0 * theme.metrics().scale,
                self.encoding_menu.open,
            )
            .on_hover_text("Text encoding");
            foldex_ui::components::label_control(&encoding, "Text encoding");
            crate::keyboard::register(
                ui,
                crate::keyboard::Region::Inspector,
                encoding.id,
                encoding.rect,
            );
            self.encoding_anchor = encoding.rect;
            if !ui.is_rect_visible(encoding.rect) {
                self.encoding_menu.close();
            }
            if encoding.clicked() {
                self.encoding_return = Some(encoding.id);
                self.encoding_menu.toggle(ui.ctx());
                self.encoding_menu.select(
                    [
                        TextEncoding::Auto,
                        TextEncoding::Utf8,
                        TextEncoding::Utf16Le,
                        TextEncoding::Utf16Be,
                        TextEncoding::Windows1252,
                    ]
                    .iter()
                    .position(|value| *value == self.options.encoding)
                    .unwrap_or(0),
                );
            }
        });
        if rebuild {
            self.schedule(false);
        }
    }

    fn text_ui(&mut self, ui: &mut Ui, rect: Rect, theme: &Theme) {
        let Some(Data::Text { model, .. }) = &self.content else {
            return;
        };
        let rows = model.rows.clone();
        let digits = model.total_lines.max(1).ilog10() as f32 + 1.0;
        let font = FontId::monospace(self.font_size);
        let char_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'M')).max(1.0);
        let gutter = if self.line_numbers {
            (digits * char_width + 12.0).max(28.0)
        } else {
            0.0
        };
        let columns = ((rect.width() - gutter - 14.0).max(1.0) / char_width)
            .floor()
            .clamp(4.0, 512.0) as usize;
        if self.options.wrap_columns.is_some_and(|old| old != columns) {
            self.options.wrap_columns = Some(columns);
            self.schedule(false);
        }
        let Some(Data::Text { model, .. }) = &self.content else {
            return;
        };
        let status = format!(
            "{} · {} / {} lines{}{}",
            model.encoding,
            model.matched_lines,
            model.total_lines,
            if model.replacements {
                " · replacements"
            } else {
                ""
            },
            if model.limited {
                " · limited preview"
            } else {
                ""
            }
        );
        let max_columns = model.max_columns;
        ui.label(
            egui::RichText::new(status)
                .size((theme.font_size - 1.0).max(10.0))
                .color(theme.dim()),
        );
        let style = (
            self.font_size.to_bits(),
            theme.text,
            ui.ctx().pixels_per_point().to_bits(),
            foldex_ui::fonts::revision(ui.ctx()),
        );
        if self.layout_style != style {
            self.clear_layouts();
            self.layout_style = style;
        }
        let height = (self.font_size + 5.0).ceil();
        ui.spacing_mut().item_spacing.y = 0.0;
        let viewport_rect = Rect::from_min_max(ui.cursor().min, rect.max).intersect(ui.clip_rect());
        let text_focus = crate::keyboard::composite(
            ui,
            crate::keyboard::Region::Inspector,
            egui::Id::new("inspector-text-focus"),
            viewport_rect,
            "Text preview",
        );
        if text_focus.has_focus() && !self.ime_input {
            self.text_offset = preview_scroll(
                ui.ctx(),
                self.text_offset,
                vec2(char_width * 4.0, height),
                viewport_rect.size() * 0.9,
                vec2(
                    max_columns as f32 * char_width + gutter + 8.0,
                    rows.len() as f32 * height,
                ) - viewport_rect.size(),
            );
        }
        foldex_ui::components::focus_ring(
            ui,
            viewport_rect,
            text_focus.id,
            theme,
            text_focus.has_focus(),
        );
        let output = egui::ScrollArea::both()
            .id_salt("inspector-text")
            .auto_shrink([false, false])
            .scroll_offset(self.text_offset)
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.min_rect().min;
                ui.set_min_size(vec2(
                    (max_columns as f32 * char_width + gutter + 8.0).max(viewport.width()),
                    rows.len() as f32 * height,
                ));
                let first = ((viewport.top() / height).floor().max(0.0) as usize).min(rows.len());
                let end =
                    ((viewport.bottom() / height).ceil().max(0.0) as usize + 1).min(rows.len());
                let first_column = (viewport.left() / char_width).floor().max(0.0) as usize;
                let visible_columns = ((viewport.width() - gutter).max(1.0) / char_width)
                    .ceil()
                    .clamp(1.0, 512.0) as usize
                    + 2;
                let clip = ui.clip_rect();
                let text_clip =
                    Rect::from_min_max(pos2(clip.left() + gutter, clip.top()), clip.max);
                for row in first..end {
                    let data = &rows[row];
                    let key = (row, first_column, visible_columns);
                    self.layout_tick = self.layout_tick.wrapping_add(1);
                    if !self.layouts.contains_key(&key) {
                        let (slice, prefix) =
                            column_slice(&data.text, first_column, visible_columns);
                        let count = slice.chars().count();
                        self.trim_layouts(count);
                        foldex_ui::fonts::request_text(ui.ctx(), &slice);
                        let galley = ui.painter().layout_no_wrap(slice, font.clone(), theme.fg());
                        let number = ui.painter().layout_no_wrap(
                            if data.continued {
                                String::new()
                            } else {
                                data.number.to_string()
                            },
                            FontId::monospace(self.font_size - 1.0),
                            theme.dim(),
                        );
                        self.layout_chars += count;
                        self.layouts.insert(
                            key,
                            RowLayout {
                                galley,
                                number,
                                chars: count,
                                touched: self.layout_tick,
                                first_column: prefix,
                            },
                        );
                    }
                    let layout = self.layouts.get_mut(&key).unwrap();
                    layout.touched = self.layout_tick;
                    let at = pos2(
                        origin.x + gutter + layout.first_column as f32 * char_width,
                        origin.y + row as f32 * height,
                    );
                    let label_rect =
                        Rect::from_min_size(at, layout.galley.size().max(vec2(1.0, height)));
                    ui.scope_builder(
                        UiBuilder::new()
                            .id_salt(("inspector-text-row", row))
                            .max_rect(label_rect),
                        |ui| {
                            ui.set_clip_rect(text_clip);
                            // Text remains selectable with the mouse and Copy,
                            // while the preview contributes one keyboard stop.
                            ui.add(
                                egui::Label::new(layout.galley.clone())
                                    .selectable(true)
                                    .sense(Sense::CLICK | Sense::DRAG),
                            );
                        },
                    );
                    if self.line_numbers {
                        let gutter_rect =
                            Rect::from_min_size(pos2(clip.left(), at.y), vec2(gutter, height));
                        ui.painter().rect_filled(
                            gutter_rect.intersect(clip),
                            0,
                            Theme::color(theme.sidebar),
                        );
                        ui.painter()
                            .with_clip_rect(gutter_rect.intersect(clip))
                            .galley(
                                pos2(
                                    clip.left() + gutter - layout.number.size().x - 7.0,
                                    at.y + 0.5,
                                ),
                                layout.number.clone(),
                                theme.dim(),
                            );
                    }
                }
            });
        self.text_offset = output.state.offset;
    }

    fn folder_ui(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        theme: &Theme,
        path: &Path,
        response: &mut InspectorResponse,
    ) {
        let preview_scale = (self.font_size / 14.0).clamp(0.6, 2.0);
        let preview_icon = 16.0 * preview_scale;
        self.folder_motion
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        self.folder_motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ui.ctx(), theme.duration()));
        let Some(Data::Folder { model, .. }) = &self.content else {
            return;
        };
        let arrivals = std::mem::take(&mut self.folder_arrivals);
        let items = model.items.clone();
        let indices = model.indices.clone();
        let limited = model.limited;
        let skipped = model.skipped;
        let style = (
            self.font_size.to_bits(),
            theme.text,
            ui.ctx().pixels_per_point().to_bits(),
            foldex_ui::fonts::revision(ui.ctx()),
        );
        if self.layout_style != style {
            self.clear_layouts();
            self.layout_style = style;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{}{}{}",
                    foldex_ui::item_count(indices.len()),
                    if limited { " · limited preview" } else { "" },
                    if skipped > 0 {
                        " · unreadable entries skipped"
                    } else {
                        ""
                    }
                ))
                .small()
                .color(theme.dim()),
            );
            if inspector_control(
                foldex_ui::components::text_button(
                    ui,
                    theme,
                    if self.options.descending {
                        "Z–A"
                    } else {
                        "A–Z"
                    },
                    false,
                ),
                ui,
            )
            .on_hover_text("Reverse folder preview order")
            .clicked()
            {
                self.options.descending = !self.options.descending;
                self.schedule(false);
            }
        });
        ui.spacing_mut().item_spacing.y = 0.0;
        let viewport_rect = Rect::from_min_max(ui.cursor().min, rect.max).intersect(ui.clip_rect());
        let list = crate::keyboard::composite(
            ui,
            crate::keyboard::Region::Inspector,
            egui::Id::new("inspector-folder-focus"),
            viewport_rect,
            "Folder contents",
        );
        if indices.is_empty() {
            let remaining = Rect::from_min_max(ui.cursor().min, rect.max);
            let preview_theme = Theme {
                font_size: self.font_size,
                ..theme.clone()
            };
            let message = if !self.options.filter.is_empty() {
                "No matching items"
            } else if skipped > 0 {
                "No readable items in this preview"
            } else {
                "This folder is empty"
            };
            foldex_ui::components::message_at(
                ui,
                remaining,
                egui::Id::new("inspector-folder-empty"),
                &preview_theme,
                message,
                theme.dim(),
            );
            return;
        }
        let row_height = (self.font_size + 11.0).max(23.0);
        self.folder_selected = self.folder_selected.min(indices.len().saturating_sub(1));
        if list.has_focus() && !self.ime_input {
            let page = (viewport_rect.height() / row_height).floor().max(1.0) as usize;
            for key in [
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::PageUp,
                egui::Key::PageDown,
                egui::Key::Home,
                egui::Key::End,
            ] {
                if crate::commands::take_key(ui.ctx(), egui::Modifiers::NONE, key) {
                    self.folder_selected =
                        preview_cursor(self.folder_selected, indices.len(), key, page);
                    self.folder_reveal = true;
                }
            }
            if crate::commands::take_key(ui.ctx(), egui::Modifiers::NONE, egui::Key::Enter) {
                response.go_to =
                    Some(path.join(&items[indices[self.folder_selected] as usize].raw_name));
            }
        }
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("inspector-folder")
            .auto_shrink([false, false]);
        if self.folder_reveal || list.gained_focus() {
            scroll = scroll.vertical_scroll_offset(
                (self.folder_selected as f32 * row_height - viewport_rect.height() * 0.5
                    + row_height * 0.5)
                    .max(0.0),
            );
            if !ui.is_sizing_pass() {
                self.folder_reveal = false;
            }
        }
        scroll.show_rows(ui, row_height, indices.len(), |ui, range| {
            let origin = ui.cursor().min.to_vec2() - vec2(0.0, range.start as f32 * row_height);
            let viewport = ui.clip_rect().translate(-origin);
            for row in range {
                let entry = &items[indices[row] as usize];
                let (target, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
                let identity = egui::Id::new(("inspector-folder-entry", path, &entry.raw_name));
                let placement = self.folder_motion.place_in_view(
                    identity,
                    target.translate(-origin),
                    arrivals,
                    viewport,
                );
                let rect = placement.rect.translate(origin);
                let clipped = rect.intersect(ui.clip_rect());
                let hit = ui.interact(
                    Rect::from_min_max(clipped.min, clipped.max.max(clipped.min)),
                    identity.with("hit"),
                    Sense::CLICK,
                );
                hit.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        ui.is_enabled(),
                        self.folder_selected == row,
                        &entry.name,
                    )
                });
                if hit.clicked() {
                    self.folder_selected = row;
                    list.request_focus();
                }
                let mut painter = ui.painter().clone();
                painter.multiply_opacity(placement.opacity);
                painter.rect_filled(rect, 0, Theme::color(theme.sidebar));
                let hover = foldex_ui::animated_state(
                    ui.ctx(),
                    hit.id.with("hover"),
                    hit.hovered(),
                    theme,
                    MotionProfile::Fast,
                );
                painter.rect_filled(
                    rect,
                    theme.radius,
                    Theme::color(theme.hover).gamma_multiply(hover),
                );
                if self.folder_selected == row {
                    painter.rect_filled(rect, theme.radius, Theme::color(theme.selection));
                    if list.has_focus() && foldex_ui::focus::keyboard_visible(ui.ctx()) {
                        painter.rect_stroke(
                            rect.shrink(0.5),
                            theme.radius,
                            Stroke::new(1.0, theme.accent()),
                            StrokeKind::Inside,
                        );
                    }
                }
                icon(
                    &painter,
                    pos2(rect.left() + 10.0 * preview_scale, rect.center().y),
                    foldex_ui::file_icon(&entry.name, entry.directory),
                    if entry.directory {
                        Theme::color(theme.folder)
                    } else {
                        theme.dim()
                    },
                    preview_icon,
                );
                let text_rect = Rect::from_min_max(
                    pos2(rect.left() + 25.0 * preview_scale, rect.top()),
                    pos2(
                        (rect.right() - 4.0 * preview_scale)
                            .max(rect.left() + 25.0 * preview_scale),
                        rect.bottom(),
                    ),
                );
                let key = (row, usize::MAX, text_rect.width().to_bits() as usize);
                self.layout_tick = self.layout_tick.wrapping_add(1);
                if !self.layouts.contains_key(&key) {
                    let count = entry.name.chars().count();
                    foldex_ui::fonts::request_text(ui.ctx(), &entry.name);
                    self.trim_layouts(count);
                    self.layout_chars += count;
                    let mut job = egui::text::LayoutJob::simple(
                        entry.name.clone(),
                        FontId::proportional(self.font_size),
                        theme.fg(),
                        text_rect.width().max(1.0),
                    );
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    self.layouts.insert(
                        key,
                        RowLayout {
                            galley: ui.painter().layout_job(job),
                            number: ui.painter().layout_no_wrap(
                                String::new(),
                                FontId::monospace(self.font_size),
                                theme.dim(),
                            ),
                            chars: count,
                            touched: self.layout_tick,
                            first_column: 0,
                        },
                    );
                }
                let layout = self.layouts.get_mut(&key).unwrap();
                layout.touched = self.layout_tick;
                let galley = layout.galley.clone();
                painter.with_clip_rect(text_rect).galley(
                    pos2(text_rect.left(), rect.center().y - galley.size().y * 0.5),
                    galley,
                    theme.fg(),
                );
                if hit.double_clicked() {
                    response.go_to = Some(path.join(&entry.raw_name));
                }
                if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
                    hit.on_hover_ui(|ui| {
                        ui.label(&entry.name);
                        ui.label(
                            egui::RichText::new(if entry.link {
                                "Link · Double-click to show in panel"
                            } else {
                                "Double-click to show in panel"
                            })
                            .small()
                            .color(theme.dim()),
                        );
                    });
                }
            }
        });
        if self.folder_motion.is_moving() {
            ui.ctx().request_repaint();
        }
    }

    fn image_controls(&mut self, ui: &mut Ui, theme: &Theme) {
        ui.horizontal_wrapped(|ui| {
            if inspector_control(
                foldex_ui::components::text_button(
                    ui,
                    theme,
                    "Fit",
                    self.image_scale == ImageScale::Fit,
                ),
                ui,
            )
            .clicked()
            {
                self.image_scale = ImageScale::Fit;
                self.image_recenter = true;
            }
            if inspector_control(
                foldex_ui::components::text_button(
                    ui,
                    theme,
                    "1:1",
                    self.image_scale == ImageScale::Original,
                ),
                ui,
            )
            .on_hover_text("One source pixel per screen pixel; preview may be downsampled")
            .clicked()
            {
                self.image_scale = ImageScale::Original;
                self.image_recenter = true;
            }
            if inspector_control(icon_button(ui, theme, Icon::Minus, "Zoom out", false), ui)
                .clicked()
            {
                self.change_contents_size(false);
            }
            if inspector_control(icon_button(ui, theme, Icon::Plus, "Zoom in", false), ui).clicked()
            {
                self.change_contents_size(true);
            }
            if inspector_control(
                icon_button(ui, theme, Icon::Refresh, "Rotate clockwise", false),
                ui,
            )
            .clicked()
            {
                self.rotation += 1.0;
                if self.rotation >= 8.0 {
                    self.rotation -= 4.0;
                    self.visual_rotation -= 4.0;
                }
                self.image_recenter = true;
            }
            let mut percent = match self.image_scale {
                ImageScale::Fit => self.visual_zoom * 100.0,
                ImageScale::Original => 100.0,
                ImageScale::Custom => self.image_zoom * 100.0,
            };
            if inspector_control(
                ui.add_sized(
                    [64.0 * theme.metrics().scale, theme.metrics().control],
                    egui::DragValue::new(&mut percent)
                        .speed(1.0)
                        .range(2.0..=1600.0)
                        .clamp_existing_to_range(false)
                        .fixed_decimals(0)
                        .suffix("%"),
                ),
                ui,
            )
            .on_hover_text("Custom image scale · double-click to type a percentage")
            .changed()
            {
                self.image_zoom = (percent / 100.0).clamp(0.02, 16.0);
                self.image_scale = ImageScale::Custom;
            }
            if inspector_control(
                foldex_ui::components::text_button(ui, theme, "Grid", self.image_grid),
                ui,
            )
            .on_hover_text("Transparency checkerboard")
            .clicked()
            {
                self.image_grid = !self.image_grid;
            }
            if inspector_control(
                foldex_ui::components::text_button(ui, theme, "Map", self.image_minimap),
                ui,
            )
            .on_hover_text("Image minimap while zoomed")
            .clicked()
            {
                self.image_minimap = !self.image_minimap;
            }
        });
    }

    fn image_ui(&mut self, ui: &mut Ui, _rect: Rect, theme: &Theme) {
        let Some(Data::Image {
            original, preview, ..
        }) = &self.content
        else {
            return;
        };
        let original = *original;
        let preview = *preview;
        let Some(texture) = self.texture.as_ref().map(TextureHandle::id) else {
            return;
        };
        let gains = foldex_ui::motion::layout_gains(ui.ctx(), theme.duration());
        let previous_rotation = self.visual_rotation;
        if gains.step(
            &mut self.visual_rotation,
            self.rotation,
            MotionProfile::Standard,
            0.0001,
        ) {
            ui.ctx().request_repaint();
        }
        let rotating = previous_rotation != self.visual_rotation;
        let angle = self.visual_rotation * std::f32::consts::FRAC_PI_2;
        let ppp = ui.ctx().pixels_per_point().max(0.25);
        let source = vec2(original[0] as f32 / ppp, original[1] as f32 / ppp);
        let (sin, cos) = angle.sin_cos();
        let extent = vec2(
            cos.abs() * source.x + sin.abs() * source.y,
            sin.abs() * source.x + cos.abs() * source.y,
        );
        let caption_theme = Theme {
            font_size: (theme.font_size - 1.0).max(10.0),
            ..theme.clone()
        };
        let caption_height = ui.fonts_mut(|fonts| fonts.row_height(&caption_theme.font()));
        let (caption_rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), caption_height), Sense::hover());
        let available = ui.available_size().max(vec2(1.0, 1.0));
        let fit = (available.x / extent.x)
            .min(available.y / extent.y)
            .max(0.0001);
        let target = match self.image_scale {
            ImageScale::Fit => fit,
            ImageScale::Original => 1.0,
            ImageScale::Custom => self.image_zoom,
        };
        if self.image_initial || self.image_scale == ImageScale::Fit && rotating {
            self.visual_zoom = target;
            self.image_initial = false;
            ui.ctx().request_repaint(); // Refresh the scale control after its first fitted layout.
        }
        if gains.step(
            &mut self.visual_zoom,
            target,
            MotionProfile::Standard,
            0.0001,
        ) {
            ui.ctx().request_repaint();
        }
        let zoom = self.visual_zoom.max(0.0001);
        let caption = format!(
            "{} × {}{}",
            original[0],
            original[1],
            if preview[0] as u32 != original[0] || preview[1] as u32 != original[1] {
                " · downsampled preview"
            } else {
                ""
            }
        );
        foldex_ui::components::elided_label(
            ui.painter(),
            caption_rect,
            &caption_theme,
            &caption,
            theme.dim(),
            false,
        );
        if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
            ui.interact(caption_rect, ui.id().with("image-caption"), Sense::hover())
                .on_hover_text(caption);
        }
        if self.checker_style != (theme.background, theme.hover) {
            self.checker = None;
            self.checker_style = (theme.background, theme.hover);
        }
        let fade_gains = foldex_ui::motion::MotionGains::new(
            ui.input(|input| input.stable_dt),
            theme.duration(),
        );
        if fade_gains.step_normalized(
            &mut self.image_grid_opacity,
            if self.image_grid { 1.0 } else { 0.0 },
            MotionProfile::Standard,
        ) {
            ui.ctx().request_repaint();
        }
        if self.image_grid_opacity > 0.0 && self.checker.is_none() {
            let mut pixels = vec![Color32::TRANSPARENT; 16 * 16];
            for y in 0..16 {
                for x in 0..16 {
                    pixels[y * 16 + x] = if (x / 8 + y / 8) % 2 == 0 {
                        Theme::color(theme.background)
                    } else {
                        Theme::color(theme.hover)
                    };
                }
            }
            let mut options = TextureOptions::NEAREST;
            options.wrap_mode = TextureWrapMode::Repeat;
            self.checker = Some(ui.ctx().load_texture(
                "inspector-checker",
                ColorImage::new([16, 16], pixels),
                options,
            ));
        }
        let grid = self.checker.as_ref().map(TextureHandle::id);
        let draw_size = extent * zoom;
        let image_size = source * zoom;
        let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        let viewport = ui.available_size().max(vec2(1.0, 1.0));
        let image_focus = crate::keyboard::composite(
            ui,
            crate::keyboard::Region::Inspector,
            egui::Id::new("inspector-image-focus"),
            Rect::from_min_size(ui.cursor().min, viewport).intersect(ui.clip_rect()),
            "Image preview",
        );
        // Retain the visible image center during zoom and responsive resizing.
        // The pointer continues to own direct panning; there is no second easing loop.
        if self.image_recenter
            || draw_size != self.image_draw_size
            || viewport != self.image_viewport
        {
            let center = if self.image_recenter || self.image_draw_size.min_elem() <= 0.0 {
                vec2(0.5, 0.5)
            } else {
                let old_centering =
                    (self.image_draw_size.max(self.image_viewport) - self.image_draw_size) * 0.5;
                ((self.image_offset + self.image_viewport * 0.5 - old_centering)
                    / self.image_draw_size)
                    .clamp(Vec2::ZERO, Vec2::splat(1.0))
            };
            let content = draw_size.max(viewport);
            self.image_offset = (center * draw_size + (content - draw_size) * 0.5 - viewport * 0.5)
                .clamp(Vec2::ZERO, (content - viewport).max(Vec2::ZERO));
            self.image_recenter = false;
        }
        if image_focus.has_focus() && !self.ime_input {
            self.image_offset = preview_scroll(
                ui.ctx(),
                self.image_offset,
                viewport * 0.1,
                viewport * 0.9,
                draw_size.max(viewport) - viewport,
            );
        }
        let output = egui::ScrollArea::both()
            .id_salt("inspector-image")
            .auto_shrink([false, false])
            .scroll_source(egui::scroll_area::ScrollSource::ALL)
            .scroll_offset(self.image_offset)
            .show_viewport(ui, |ui, viewport| {
                let content = draw_size.max(viewport.size());
                ui.set_min_size(content);
                let bounds =
                    Rect::from_min_size(ui.min_rect().min + (content - draw_size) * 0.5, draw_size);
                if self.image_grid_opacity > 0.0 {
                    if let Some(grid) = grid {
                        let mut painter = ui.painter().clone();
                        painter.multiply_opacity(self.image_grid_opacity);
                        paint_rotated(
                            &painter,
                            grid,
                            bounds.center(),
                            image_size,
                            angle,
                            Rect::from_min_size(Pos2::ZERO, image_size / 16.0),
                        );
                    }
                }
                paint_rotated(
                    ui.painter(),
                    texture,
                    bounds.center(),
                    image_size,
                    angle,
                    uv,
                );
                ((content - draw_size) * 0.5, viewport)
            });
        self.image_offset = output.state.offset;
        self.image_draw_size = draw_size;
        self.image_viewport = output.inner_rect.size();
        foldex_ui::components::focus_ring(
            ui,
            output.inner_rect,
            image_focus.id,
            theme,
            image_focus.has_focus(),
        );
        let map_id = egui::Id::new("inspector-image-map");
        let map_active = self.image_minimap
            && (draw_size.x > output.inner_rect.width() + 1.0
                || draw_size.y > output.inner_rect.height() + 1.0);
        let map_gains = foldex_ui::motion::MotionGains::new(
            ui.input(|input| input.stable_dt),
            theme.duration(),
        );
        if map_gains.step_normalized(
            &mut self.image_map_opacity,
            if map_active { 1.0 } else { 0.0 },
            MotionProfile::Standard,
        ) {
            ui.ctx().request_repaint();
        }
        if !map_active && ui.memory(|memory| memory.has_focus(map_id)) {
            image_focus.request_focus();
        }
        if self.image_map_opacity > 0.0
            && output.inner_rect.width() > 24.0
            && output.inner_rect.height() > 24.0
        {
            let scale = theme.metrics().scale;
            let mini_size = fit_size(
                draw_size,
                vec2(
                    (112.0 * scale).min(output.inner_rect.width() * 0.45),
                    (82.0 * scale).min(output.inner_rect.height() * 0.3),
                ),
            );
            let map = Rect::from_min_size(
                output.inner_rect.right_bottom() - mini_size - vec2(8.0, 8.0) * scale
                    + vec2(0.0, 4.0 * scale * (1.0 - self.image_map_opacity)),
                mini_size,
            );
            let outer = map.expand(3.0 * scale).intersect(output.inner_rect);
            if map_active {
                crate::keyboard::register(ui, crate::keyboard::Region::Inspector, map_id, outer);
            }
            let hit = ui.interact(
                outer,
                map_id,
                if map_active {
                    Sense::click_and_drag()
                } else {
                    Sense::hover()
                },
            );
            let max_offset = (draw_size - output.inner_rect.size()).max(Vec2::ZERO);
            if map_active {
                hit.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Other,
                        ui.is_enabled(),
                        "Image minimap · drag to pan, arrow keys to move",
                    )
                });
                if let Some(point) = foldex_ui::components::drag_position(ui, outer, &hit) {
                    hit.request_focus();
                    let center =
                        ((point - map.min) / map.size()).clamp(Vec2::ZERO, Vec2::splat(1.0));
                    self.image_offset = (center * draw_size - output.inner_rect.size() * 0.5)
                        .clamp(Vec2::ZERO, max_offset);
                    ui.ctx().request_repaint();
                }
                if hit.has_focus() && !self.ime_input {
                    ui.memory_mut(|memory| {
                        memory.set_focus_lock_filter(
                            map_id,
                            egui::EventFilter {
                                tab: false,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: false,
                            },
                        )
                    });
                    for (key, direction) in [
                        (egui::Key::ArrowLeft, vec2(-1.0, 0.0)),
                        (egui::Key::ArrowRight, vec2(1.0, 0.0)),
                        (egui::Key::ArrowUp, vec2(0.0, -1.0)),
                        (egui::Key::ArrowDown, vec2(0.0, 1.0)),
                    ] {
                        let step = {
                            if crate::commands::take_key(ui.ctx(), egui::Modifiers::SHIFT, key) {
                                0.5
                            } else if crate::commands::take_key(
                                ui.ctx(),
                                egui::Modifiers::NONE,
                                key,
                            ) {
                                0.1
                            } else {
                                0.0
                            }
                        };
                        if step > 0.0 {
                            self.image_offset = (self.image_offset
                                + direction * output.inner_rect.size() * step)
                                .clamp(Vec2::ZERO, max_offset);
                            ui.ctx().request_repaint();
                        }
                    }
                    if crate::commands::take_key(ui.ctx(), egui::Modifiers::NONE, egui::Key::Escape)
                    {
                        image_focus.request_focus();
                    }
                }
            }
            let hover = foldex_ui::animated_state(
                ui.ctx(),
                map_id.with("hover"),
                map_active && (hit.hovered() || hit.dragged()),
                theme,
                MotionProfile::Fast,
            );
            let mut painter = ui.painter().with_clip_rect(output.inner_rect);
            painter.multiply_opacity(self.image_map_opacity);
            painter.rect_filled(outer, theme.radius, Theme::color(theme.surface));
            painter.rect_stroke(
                outer,
                theme.radius,
                Stroke::new(1.0, theme.line().lerp_to_gamma(theme.accent(), hover * 0.7)),
                StrokeKind::Inside,
            );
            paint_rotated(
                &painter,
                texture,
                map.center(),
                image_size * (mini_size.x / draw_size.x),
                angle,
                uv,
            );
            let centering = output.inner.0;
            let offset = (self.image_offset - centering).max(Vec2::ZERO);
            let ratio = mini_size / draw_size;
            let shown =
                Rect::from_min_size(map.min + offset * ratio, output.inner_rect.size() * ratio)
                    .intersect(map);
            painter.rect_stroke(
                shown,
                0,
                Stroke::new(1.0, theme.accent()),
                StrokeKind::Inside,
            );
            foldex_ui::components::focus_ring(
                ui,
                outer,
                map_id,
                theme,
                map_active && hit.has_focus(),
            );
        }
    }
}

impl Drop for Inspector {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.notify();
        // Dropping JoinHandle detaches. A blocked provider must never block UI exit.
        self.worker.take();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    let _ = shared.worker_thread.set(thread::current());
    let mut cached: Option<(PathBuf, Source)> = None;
    loop {
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        if shared.clear.swap(false, Ordering::AcqRel) {
            cached = None;
            if let Ok(mut mailbox) = shared.mailbox.lock() {
                mailbox.reply = None;
            }
        }
        if let Some((_, Source::Image { pixels, sent, .. })) = &mut cached {
            if shared.image_ack.load(Ordering::Acquire) >= *sent {
                pixels.take();
            }
        }
        let request = {
            let Ok(mut mailbox) = shared.mailbox.lock() else {
                break;
            };
            mailbox.request.take()
        };
        let Some(request) = request else {
            if !shared.closed.load(Ordering::Acquire) {
                thread::park();
            }
            continue;
        };
        let Some(path) = request.path else {
            cached = None;
            continue;
        };
        let canceled = || {
            shared.closed.load(Ordering::Acquire)
                || shared.current.load(Ordering::Acquire) != request.id
        };
        if canceled() {
            continue;
        }
        if request.force || cached.as_ref().is_none_or(|(old, _)| old != &path) {
            let source = read_source(&path, &canceled).unwrap_or_else(|error| Source::Info {
                info: None,
                message: format!("Preview unavailable: {error}"),
            });
            if canceled() {
                continue;
            }
            cached = Some((path.clone(), source));
        }
        let Some((_, source)) = &mut cached else {
            continue;
        };
        let Some(data) = render_source(source, &request.options, request.id, &canceled) else {
            continue;
        };
        if canceled() {
            continue;
        }
        if let Ok(mut mailbox) = shared.mailbox.lock() {
            if !canceled() {
                mailbox.reply = Some(Reply {
                    id: request.id,
                    path,
                    data,
                });
            }
        }
        if !canceled() {
            (shared.wake)();
        }
    }
}

fn read_source(path: &Path, canceled: &dyn Fn() -> bool) -> io::Result<Source> {
    let metadata = fs::symlink_metadata(path)?;
    let caps = foldex_fs::capabilities::probe(path)?;
    let info = Arc::new(FileInfo::new(&metadata, caps));
    if let Err(error) = foldex_fs::capabilities::background_path(path) {
        return Ok(Source::Info {
            info: Some(info),
            message: format!("This location can't be previewed: {error}"),
        });
    }
    if (!caps.is_dir() && !caps.can_read_content()) || (caps.is_dir() && !caps.can_enumerate()) {
        return Ok(Source::Info {
            info: Some(info),
            message: "Online-only item. Open it explicitly to download or view it.".into(),
        });
    }
    if info.link || (!metadata.is_file() && !metadata.is_dir()) {
        return Ok(Source::Info {
            info: Some(info),
            message: "Link or special item. Open it with its associated app.".into(),
        });
    }
    if metadata.is_dir() {
        let mut items = Vec::new();
        let mut name_bytes = 0usize;
        let mut limited = false;
        let mut skipped = 0;
        for item in foldex_fs::ReadDir::open_no_follow(path)? {
            if canceled() {
                return Err(canceled_error());
            }
            let item = match item {
                Ok(item) => item,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let item_caps = item.capabilities();
            let raw_name = item.name;
            let weight = raw_name.len().saturating_mul(4);
            if items.len() == FOLDER_ITEMS || name_bytes.saturating_add(weight) > FOLDER_NAME_BYTES
            {
                limited = true;
                break;
            }
            name_bytes += weight;
            items.push(FolderItem {
                name: raw_name.to_string_lossy().into_owned(),
                raw_name,
                directory: item_caps.is_dir(),
                link: item_caps.redirects() || item_caps.unknown_reparse(),
            });
        }
        return Ok(Source::Folder {
            info,
            items: Arc::from(items),
            limited,
            skipped,
        });
    }
    let mut file = foldex_fs::capabilities::open_background_content(path)?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(TEXT_BYTES + 4));
    Read::by_ref(&mut file)
        .take((TEXT_BYTES + 4) as u64)
        .read_to_end(&mut bytes)?;
    if canceled() {
        return Err(canceled_error());
    }
    let format = image::guess_format(&bytes).ok();
    if matches!(
        format,
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        if metadata.len() > IMAGE_FILE_BYTES as u64 {
            return Ok(Source::Info {
                info: Some(info),
                message: "Image exceeds the 16 MiB preview limit. Open the original to view it."
                    .into(),
            });
        }
        bytes.reserve_exact((metadata.len() as usize).saturating_sub(bytes.len()));
        file.take((IMAGE_FILE_BYTES + 1 - bytes.len()) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > IMAGE_FILE_BYTES {
            return Err(io::Error::other("This image is too large to preview"));
        }
        let (original, pixels) = decode_image(&bytes, format.unwrap())?;
        let preview = pixels.size;
        return Ok(Source::Image {
            info,
            original,
            preview,
            pixels: Some(Arc::new(pixels)),
            sent: u64::MAX,
        });
    }
    let limited = metadata.len() > TEXT_BYTES as u64 || bytes.len() > TEXT_BYTES;
    bytes.truncate(TEXT_BYTES);
    Ok(Source::Text {
        info,
        bytes,
        limited,
    })
}

fn decode_image(bytes: &[u8], format: ImageFormat) -> io::Result<([u32; 2], ImagePixels)> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(IMAGE_DECODE_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(io::Error::other)?;
    if decoder.total_bytes() > IMAGE_DECODE_BYTES {
        return Err(io::Error::other(
            "decoded image exceeds the 64 MiB preview budget",
        ));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let (width, height) = decoder.dimensions();
    let mut image = DynamicImage::from_decoder(decoder).map_err(io::Error::other)?;
    image.apply_orientation(orientation);
    let original = if image.width() == width {
        [width, height]
    } else {
        [height, width]
    };
    let image = if image.width() > IMAGE_EDGE || image.height() > IMAGE_EDGE {
        image.thumbnail(IMAGE_EDGE, IMAGE_EDGE)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Ok((
        original,
        ImagePixels {
            size: [rgba.width() as usize, rgba.height() as usize],
            rgba: rgba.into_raw(),
        },
    ))
}

fn render_source(
    source: &mut Source,
    options: &Options,
    id: u64,
    canceled: &dyn Fn() -> bool,
) -> Option<Data> {
    match source {
        Source::Text {
            info,
            bytes,
            limited,
        } => match decode_text(bytes, options.encoding, *limited) {
            Ok((text, encoding, replacements)) => {
                let model = build_text(&text, options, *limited, encoding, replacements, canceled)?;
                Some(Data::Text {
                    info: info.clone(),
                    model,
                })
            }
            Err(message) => Some(Data::Binary {
                info: info.clone(),
                message,
            }),
        },
        Source::Folder {
            info,
            items,
            limited,
            skipped,
        } => {
            let filter = options.filter.to_lowercase();
            let mut indices = Vec::new();
            for (index, item) in items.iter().enumerate() {
                if index % 64 == 0 && canceled() {
                    return None;
                }
                if filter.is_empty() || item.name.to_lowercase().contains(&filter) {
                    indices.push(index as u32);
                }
            }
            indices.sort_unstable_by(|a, b| {
                let a = &items[*a as usize];
                let b = &items[*b as usize];
                b.directory
                    .cmp(&a.directory)
                    .then_with(|| {
                        let order = foldex_core::natural_cmp(&a.name, &b.name);
                        if options.descending {
                            order.reverse()
                        } else {
                            order
                        }
                    })
                    .then_with(|| a.raw_name.cmp(&b.raw_name))
            });
            Some(Data::Folder {
                info: info.clone(),
                model: FolderData {
                    items: items.clone(),
                    indices: Arc::from(indices),
                    limited: *limited,
                    skipped: *skipped,
                },
            })
        }
        Source::Image {
            info,
            original,
            preview,
            pixels,
            sent,
        } => {
            *sent = id;
            Some(Data::Image {
                info: info.clone(),
                original: *original,
                preview: *preview,
                pixels: pixels.clone(),
            })
        }
        Source::Info { info, message } => Some(Data::Info {
            info: info.clone(),
            message: message.clone(),
        }),
    }
}

fn decode_text(
    bytes: &[u8],
    requested: TextEncoding,
    truncated: bool,
) -> Result<(String, &'static str, bool), String> {
    let encoding = match requested {
        TextEncoding::Auto if bytes.starts_with(&[0xff, 0xfe]) => TextEncoding::Utf16Le,
        TextEncoding::Auto if bytes.starts_with(&[0xfe, 0xff]) => TextEncoding::Utf16Be,
        TextEncoding::Auto => TextEncoding::Utf8,
        encoding => encoding,
    };
    let (text, replacements) = match encoding {
        TextEncoding::Utf8 | TextEncoding::Auto => {
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            let bytes = match std::str::from_utf8(bytes) {
                Err(error) if truncated && error.error_len().is_none() => {
                    &bytes[..error.valid_up_to()]
                }
                _ => bytes,
            };
            let text = String::from_utf8_lossy(bytes);
            let replacements = matches!(text, std::borrow::Cow::Owned(_));
            if requested == TextEncoding::Auto && replacements {
                return Err(
                    "Binary file or unsupported encoding. Choose an encoding or open the original."
                        .into(),
                );
            }
            (text.into_owned(), replacements)
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let bytes = bytes
                .strip_prefix(&[0xff, 0xfe])
                .or_else(|| bytes.strip_prefix(&[0xfe, 0xff]))
                .unwrap_or(bytes);
            let mut words: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| {
                    if encoding == TextEncoding::Utf16Le {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            if truncated
                && words
                    .last()
                    .is_some_and(|word| (0xd800..=0xdbff).contains(word))
            {
                words.pop();
            }
            let replacements =
                String::from_utf16(&words).is_err() || (!truncated && bytes.len() % 2 != 0);
            (String::from_utf16_lossy(&words), replacements)
        }
        TextEncoding::Windows1252 => (bytes.iter().map(|byte| cp1252(*byte)).collect(), false),
    };
    if requested == TextEncoding::Auto {
        let controls = text
            .chars()
            .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
            .count();
        if text.contains('\0') || controls.saturating_mul(100) > text.len().max(1) {
            return Err("Binary file. Choose an encoding or open the original.".into());
        }
    }
    Ok((text, encoding.label(), replacements))
}

fn cp1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    if (0x80..=0x9f).contains(&byte) {
        HIGH[(byte - 0x80) as usize]
    } else {
        byte as char
    }
}

fn build_text(
    text: &str,
    options: &Options,
    input_limited: bool,
    encoding: &'static str,
    replacements: bool,
    canceled: &dyn Fn() -> bool,
) -> Option<TextData> {
    let mut rows = Vec::new();
    let mut displayed = 0usize;
    let mut limited = input_limited;
    let filter = options.filter.to_lowercase();
    let mut total_lines = 0usize;
    let mut matched_lines = 0usize;
    let mut max_columns = 0usize;
    // split preserves an empty final line, matching a text editor's line count.
    for (line_index, original) in text.split('\n').enumerate() {
        if line_index % 64 == 0 && canceled() {
            return None;
        }
        total_lines += 1;
        let original = original.strip_suffix('\r').unwrap_or(original);
        if !filter.is_empty() && !original.to_lowercase().contains(&filter) {
            continue;
        }
        matched_lines += 1;
        if displayed >= DISPLAY_CHARS || rows.len() >= DISPLAY_ROWS {
            limited = true;
            continue;
        }
        let mut line = String::new();
        let cap = if options.wrap_columns.is_some() {
            DISPLAY_CHARS - displayed
        } else {
            LINE_CHARS.min(DISPLAY_CHARS - displayed)
        };
        let mut chars = 0usize;
        let mut columns = 0usize;
        let mut line_limited = false;
        for character in original.chars() {
            let representation = if character == '\t' {
                if options.whitespace { "→   " } else { "    " }
            } else {
                ""
            };
            let count = if representation.is_empty() { 1 } else { 4 };
            if chars.saturating_add(count) > cap {
                limited = true;
                line_limited = true;
                break;
            }
            if !representation.is_empty() {
                line.push_str(representation);
                columns += 4;
            } else {
                let character = if character == ' ' && options.whitespace {
                    '·'
                } else if character.is_control() {
                    '�'
                } else {
                    character
                };
                line.push(character);
                columns += display_width(character);
            }
            chars += count;
        }
        if line_limited && options.wrap_columns.is_none() {
            if let Some(last) = line.pop() {
                columns -= display_width(last);
                chars -= 1;
            }
            line.push('…');
            columns += 1;
            chars += 1;
        }
        displayed += chars;
        if let Some(wrap) = options.wrap_columns {
            let segments = wrap_line(&line, wrap.max(4));
            for (index, segment) in segments.into_iter().enumerate() {
                if rows.len() == DISPLAY_ROWS {
                    limited = true;
                    break;
                }
                let width = segment.chars().map(display_width).sum();
                max_columns = max_columns.max(width);
                rows.push(TextRow {
                    number: line_index + 1,
                    continued: index > 0,
                    text: segment,
                });
            }
        } else {
            max_columns = max_columns.max(columns);
            rows.push(TextRow {
                number: line_index + 1,
                continued: false,
                text: line,
            });
        }
    }
    Some(TextData {
        rows: Arc::from(rows),
        total_lines,
        matched_lines,
        max_columns,
        limited,
        encoding,
        replacements,
    })
}

/// Greedy monospace-column wrapping; Unicode wide ranges occupy two columns.
/// Font fallback/grapheme shaping can vary; this never claims byte-exact source layout.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut columns = 0;
        let mut end = start;
        let mut last_space = None;
        for (offset, character) in line[start..].char_indices() {
            let next = columns + display_width(character);
            if next > width && end > start {
                break;
            }
            columns = next;
            end = start + offset + character.len_utf8();
            if character.is_whitespace() || character == '·' {
                last_space = Some(end);
            }
        }
        if end < line.len() {
            if let Some(space) = last_space.filter(|space| *space > start) {
                end = space;
            }
        }
        if end == start {
            break;
        }
        result.push(line[start..end].to_owned());
        start = end;
    }
    result
}

fn display_width(character: char) -> usize {
    match character as u32 {
        0x1100..=0x115f
        | 0x2329..=0x232a
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe19
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

fn column_slice(text: &str, first: usize, count: usize) -> (String, usize) {
    let mut columns = 0usize;
    let mut start = text.len();
    let mut end = text.len();
    let mut prefix = 0;
    for (offset, character) in text.char_indices() {
        let next = columns + display_width(character);
        if start == text.len() && next > first {
            start = offset;
            prefix = columns;
        }
        if columns >= first.saturating_add(count) {
            end = offset;
            break;
        }
        columns = next;
    }
    (text[start..end].to_owned(), prefix)
}

fn paint_rotated(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: Pos2,
    size: Vec2,
    angle: f32,
    uv: Rect,
) {
    let half = size * 0.5;
    let (sin, cos) = angle.sin_cos();
    let corners = [
        vec2(-half.x, -half.y),
        vec2(half.x, -half.y),
        vec2(half.x, half.y),
        vec2(-half.x, half.y),
    ];
    let uvs = [
        uv.left_top(),
        uv.right_top(),
        uv.right_bottom(),
        uv.left_bottom(),
    ];
    let mut mesh = Mesh::with_texture(texture);
    for index in 0..4 {
        mesh.vertices.push(Vertex {
            pos: center
                + vec2(
                    cos * corners[index].x - sin * corners[index].y,
                    sin * corners[index].x + cos * corners[index].y,
                ),
            uv: uvs[index],
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

fn fit_size(size: Vec2, bounds: Vec2) -> Vec2 {
    size * (bounds.x / size.x).min(bounds.y / size.y).min(1.0)
}

fn metadata_ui(ui: &mut Ui, info: &FileInfo, theme: &Theme) {
    let m = theme.metrics();
    let font = egui::FontId::proportional((theme.font_size - 1.0).max(10.0));
    let keys = [
        ("Availability", Some(info.availability.label())),
        ("Modified", info.modified.as_deref()),
        ("Created", info.created.as_deref()),
        ("Accessed", info.accessed.as_deref()),
        ("Read-only", Some(if info.read_only { "Yes" } else { "No" })),
    ];
    let key_width = keys
        .iter()
        .map(|(key, _)| {
            ui.painter()
                .layout_no_wrap((*key).into(), font.clone(), theme.dim())
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let value_width = keys
        .iter()
        .map(|(_, value)| {
            ui.painter()
                .layout_no_wrap(
                    value.unwrap_or("Unavailable").into(),
                    font.clone(),
                    theme.fg(),
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let stacked = key_width + 12.0 * m.scale + value_width > ui.available_width();
    let line_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let row_height = if stacked {
        line_height * 2.0 + m.gap
    } else {
        (theme.font_size + 8.0 * m.scale).max(22.0 * m.scale)
    };
    let caption = Theme {
        font_size: font.size,
        ..theme.clone()
    };
    for (key, value) in keys {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
        let split = (rect.left() + key_width + 12.0 * m.scale).min(rect.right());
        let (key_rect, value_rect) = if stacked {
            (
                Rect::from_min_size(rect.min, vec2(rect.width(), line_height)),
                Rect::from_min_max(
                    pos2(rect.left(), rect.top() + line_height + m.gap),
                    rect.max,
                ),
            )
        } else {
            (
                Rect::from_min_max(rect.min, pos2(split - m.gap, rect.bottom())),
                Rect::from_min_max(pos2(split, rect.top()), rect.max),
            )
        };
        foldex_ui::components::elided_label(
            ui.painter(),
            key_rect,
            &caption,
            key,
            theme.dim(),
            false,
        );
        foldex_ui::components::elided_label(
            ui.painter(),
            value_rect,
            &caption,
            value.unwrap_or("Unavailable"),
            theme.fg(),
            false,
        );
        let hit = ui.interact(rect, ui.id().with(key), Sense::hover());
        hit.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                ui.is_enabled(),
                format!("{key}: {}", value.unwrap_or("Unavailable")),
            )
        });
        if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
            hit.on_hover_text(format!("{key}: {}", value.unwrap_or("Unavailable")));
        }
    }
}

fn validate_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.as_os_str().len() > 32768
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inspector requires an absolute, NUL-free path within 64 KiB",
        ));
    }
    Ok(())
}
fn canceled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "inspector request superseded")
}

fn format_time(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => {
            let duration = error.duration();
            -(duration.as_secs().min((i64::MAX - 1) as u64) as i64)
                - i64::from(duration.subsec_nanos() > 0)
        }
    };
    #[cfg(windows)]
    {
        crate::pane::format_date(seconds)
    }
    #[cfg(not(windows))]
    {
        let days = seconds.div_euclid(86400);
        let part = seconds.rem_euclid(86400);
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let day = z - era * 146097;
        let year_of_era = (day - day / 1460 + day / 36524 - day / 146096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_part = (5 * day_of_year + 2) / 153;
        let date = day_of_year - (153 * month_part + 2) / 5 + 1;
        let month = month_part + if month_part < 10 { 3 } else { -9 };
        year += i64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{date:02} {:02}:{:02} UTC",
            part / 3600,
            part / 60 % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::AtomicUsize, time::Instant};

    #[test]
    fn folder_keyboard_cursor_pages_and_bounds_without_wrapping_or_overflow() {
        use egui::Key;
        assert_eq!(preview_cursor(0, 0, Key::End, 8), 0);
        assert_eq!(preview_cursor(0, 12, Key::ArrowUp, 8), 0);
        assert_eq!(preview_cursor(0, 12, Key::PageDown, 8), 8);
        assert_eq!(preview_cursor(8, 12, Key::PageDown, 8), 11);
        assert_eq!(preview_cursor(11, 12, Key::ArrowDown, 8), 11);
        assert_eq!(preview_cursor(11, 12, Key::PageUp, 8), 3);
        assert_eq!(
            preview_cursor(usize::MAX, 12, Key::ArrowDown, usize::MAX),
            11
        );
        assert_eq!(preview_cursor(11, 12, Key::Home, 8), 0);
    }

    #[test]
    fn preview_scroll_consumes_only_plain_navigation_and_clamps_to_content() {
        let ctx = Context::default();
        let key = |key, modifiers| egui::Event::Key {
            key,
            modifiers,
            physical_key: None,
            pressed: true,
            repeat: false,
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![
                    key(egui::Key::PageDown, egui::Modifiers::NONE),
                    key(egui::Key::ArrowLeft, egui::Modifiers::ALT),
                    key(egui::Key::ArrowDown, egui::Modifiers::SHIFT),
                ],
                ..Default::default()
            },
            |ui| {
                let offset = preview_scroll(
                    ui.ctx(),
                    vec2(10.0, 90.0),
                    vec2(5.0, 10.0),
                    vec2(80.0, 80.0),
                    vec2(100.0, 100.0),
                );
                assert_eq!(offset, vec2(10.0, 100.0));
                assert_eq!(ui.input(|input| input.events.len()), 2);
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn registered_inspector_buttons_are_owned_without_making_them_text_editors() {
        let ctx = Context::default();
        let id = egui::Id::new("inspector-header").with("focus-test");
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let hit = inspector_control(
                ui.interact(
                    Rect::from_min_size(ui.cursor().min, vec2(28.0, 28.0)),
                    id,
                    Sense::click(),
                ),
                ui,
            );
            hit.request_focus();
            assert!(crate::keyboard::owns(
                ui.ctx(),
                crate::keyboard::Region::Inspector
            ));
            assert!(!ui.ctx().text_edit_focused());
        });
        output.textures_delta.clear();
    }

    fn info() -> Arc<FileInfo> {
        Arc::new(FileInfo {
            availability: Default::default(),
            bytes: 4,
            directory: false,
            link: false,
            read_only: false,
            modified: None,
            created: None,
            accessed: None,
        })
    }

    struct Fixture {
        path: PathBuf,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/inspector-test-fixtures");
            fs::create_dir_all(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let path = root.join(format!(
                "owned-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path, root }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let Ok(path) = fs::canonicalize(&self.path) else {
                return;
            };
            // Never recursively remove anything except this owned generated fixture.
            assert_eq!(path.parent(), Some(self.root.as_path()));
            assert!(
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("owned-"))
            );
            let _ = fs::remove_dir_all(path);
        }
    }

    #[test]
    fn encoding_preserves_unicode_and_explicit_legacy_choice() {
        assert_eq!(
            decode_text(b"hello", TextEncoding::Auto, false).unwrap().0,
            "hello"
        );
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0x16, 0x4e], TextEncoding::Auto, false)
                .unwrap()
                .0,
            "世"
        );
        assert_eq!(
            decode_text(&[0xfe, 0xff, 0x4e, 0x16], TextEncoding::Auto, false)
                .unwrap()
                .0,
            "世"
        );
        assert_eq!(
            decode_text(&[0x80, 0x93, b'x', 0x94], TextEncoding::Windows1252, false)
                .unwrap()
                .0,
            "€“x”"
        );
        assert!(decode_text(b"MZ\0\x01", TextEncoding::Auto, false).is_err());
        assert!(decode_text(&[0x80], TextEncoding::Auto, false).is_err());
        assert!(decode_text(&[0x80], TextEncoding::Utf8, false).unwrap().2);
        assert_eq!(
            decode_text(&[b'a', 0xe2, 0x82], TextEncoding::Auto, true)
                .unwrap()
                .0,
            "a"
        );
    }

    #[test]
    fn text_filter_preserves_source_line_numbers_and_whitespace() {
        let options = Options {
            filter: "error".into(),
            whitespace: true,
            wrap_columns: None,
            ..Options::default()
        };
        let model = build_text(
            "ok\nERROR\t x\nno\nerror two\n",
            &options,
            false,
            "UTF-8",
            false,
            &|| false,
        )
        .unwrap();
        assert_eq!(model.total_lines, 5);
        assert_eq!(model.matched_lines, 2);
        assert_eq!(model.rows[0].number, 2);
        assert_eq!(model.rows[0].text, "ERROR→   ·x");
        assert_eq!(model.rows[1].number, 4);
    }

    #[test]
    fn text_state_and_unwrapped_unicode_lines_stay_bounded() {
        let options = Options {
            wrap_columns: None,
            ..Options::default()
        };
        let model = build_text(
            &"界".repeat(100_000),
            &options,
            false,
            "UTF-8",
            false,
            &|| false,
        )
        .unwrap();
        assert!(model.limited);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].text.chars().count(), LINE_CHARS);
        assert!(model.rows[0].text.ends_with('…'));
        let model = build_text(
            &"1234567890\n".repeat(50_000),
            &Options::default(),
            false,
            "UTF-8",
            false,
            &|| false,
        )
        .unwrap();
        assert!(model.limited);
        assert!(model.rows.len() <= DISPLAY_ROWS);
        assert!(
            model
                .rows
                .iter()
                .map(|row| row.text.chars().count())
                .sum::<usize>()
                <= DISPLAY_CHARS
        );
    }

    #[test]
    fn wrapping_and_horizontal_slices_respect_utf8_boundaries() {
        let text = "ab 世😀 cd";
        let segments = wrap_line(text, 4);
        assert_eq!(segments.concat(), text);
        assert!(
            segments
                .iter()
                .all(|row| row.chars().map(display_width).sum::<usize>() <= 4)
        );
        let (slice, first) = column_slice("世ab😀cd", 1, 4);
        assert_eq!(first, 0);
        assert_eq!(slice, "世ab😀");
        assert_eq!(column_slice("abc", 40, 10).0, "");
    }

    #[test]
    fn latest_text_cancellation_stops_before_building_rows() {
        assert!(
            build_text(
                &"line\n".repeat(100_000),
                &Options::default(),
                false,
                "UTF-8",
                false,
                &|| true
            )
            .is_none()
        );
    }

    #[test]
    fn folder_filter_and_natural_order_retain_exact_raw_names() {
        let mut source = Source::Folder {
            info: info(),
            items: Arc::from([
                FolderItem {
                    name: "item10".into(),
                    raw_name: "item10".into(),
                    directory: false,
                    link: false,
                },
                FolderItem {
                    name: "item2".into(),
                    raw_name: "item2".into(),
                    directory: false,
                    link: false,
                },
                FolderItem {
                    name: "zfolder".into(),
                    raw_name: "zfolder".into(),
                    directory: true,
                    link: false,
                },
            ]),
            limited: false,
            skipped: 0,
        };
        let Data::Folder { model, .. } =
            render_source(&mut source, &Options::default(), 1, &|| false).unwrap()
        else {
            panic!()
        };
        assert_eq!(&*model.indices, &[2, 1, 0]);
        let options = Options {
            filter: "ITEM".into(),
            descending: true,
            ..Options::default()
        };
        let Data::Folder { model, .. } =
            render_source(&mut source, &options, 2, &|| false).unwrap()
        else {
            panic!()
        };
        assert_eq!(&*model.indices, &[0, 1]);
        assert!(render_source(&mut source, &options, 3, &|| true).is_none());
    }

    #[test]
    fn generated_sources_are_nonrecursive_and_image_payload_is_bounded() {
        let fixture = Fixture::new();
        fs::write(fixture.path.join("plain.txt"), b"text").unwrap();
        fs::create_dir(fixture.path.join("folder")).unwrap();
        fs::write(
            fixture.path.join("folder/hidden-child.txt"),
            b"not enumerated",
        )
        .unwrap();
        let Source::Folder { items, limited, .. } = read_source(&fixture.path, &|| false).unwrap()
        else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert!(!limited);
        assert!(read_source(&fixture.path, &|| true).is_err());
        let image_path = fixture.path.join("preview.png");
        image::RgbaImage::from_pixel(2000, 1000, image::Rgba([10, 20, 30, 255]))
            .save(&image_path)
            .unwrap();
        let Source::Image {
            original,
            preview,
            pixels,
            ..
        } = read_source(&image_path, &|| false).unwrap()
        else {
            panic!()
        };
        assert_eq!(original, [2000, 1000]);
        assert_eq!(preview, [1600, 800]);
        assert_eq!(pixels.unwrap().rgba.len(), 1600 * 800 * 4);
        let huge = fixture.path.join("too-large.png");
        fs::write(&huge, b"\x89PNG\r\n\x1a\n").unwrap();
        File::options()
            .write(true)
            .open(&huge)
            .unwrap()
            .set_len(IMAGE_FILE_BYTES as u64 + 1)
            .unwrap();
        assert!(matches!(
            read_source(&huge, &|| false).unwrap(),
            Source::Info { .. }
        ));
    }

    #[test]
    fn worker_delivers_latest_generation_and_close_does_not_need_a_frame_retry() {
        let fixture = Fixture::new();
        let path = fixture.path.join("latest.txt");
        fs::write(&path, b"latest selection").unwrap();
        let ctx = Context::default();
        let mut inspector = Inspector::new(Arc::new(|| {})).unwrap();
        for index in 0..32 {
            inspector
                .open_path(fixture.path.join(format!("missing-{index}")))
                .unwrap();
        }
        inspector.open_path(path.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while inspector.loading {
            inspector.poll(&ctx);
            assert!(
                Instant::now() < deadline,
                "latest inspector result never arrived"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let Some(Data::Text { model, .. }) = &inspector.content else {
            panic!("latest text not delivered")
        };
        assert_eq!(model.rows[0].text, "latest selection");
        assert!(!inspector.open_path(path).unwrap());
        let shared = inspector.shared.clone();
        let guard = shared.mailbox.lock().unwrap();
        inspector.close(); // Forces nonblocking mailbox failure on this UI call.
        assert!(inspector.path().is_none());
        drop(guard);
        let weak = Arc::downgrade(&inspector.shared);
        drop(shared);
        drop(inspector);
        while weak.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "inspector worker did not release state after close/drop"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn image_pixels_are_released_on_worker_after_upload_ack() {
        let fixture = Fixture::new();
        let path = fixture.path.join("tiny.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let ctx = Context::default();
        let mut inspector = Inspector::new(Arc::new(|| {})).unwrap();
        inspector.open_path(path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let weak = loop {
            inspector.flush_request();
            let mailbox = inspector.shared.mailbox.lock().unwrap();
            if let Some(Reply {
                data:
                    Data::Image {
                        pixels: Some(image),
                        ..
                    },
                ..
            }) = &mailbox.reply
            {
                break Arc::downgrade(image);
            }
            drop(mailbox);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        inspector.poll(&ctx);
        assert!(inspector.texture.is_some());
        assert!(matches!(
            inspector.content,
            Some(Data::Image { pixels: None, .. })
        ));
        while weak.strong_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "decoded image stayed retained after upload acknowledgement"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn inspector_text_paints_only_viewport_and_settles_without_idle_timer() {
        let ctx = Context::default();
        let mut inspector = Inspector::new(Arc::new(|| {})).unwrap();
        inspector.path = Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/synthetic-preview.txt"),
        );
        inspector.options.wrap_columns = None;
        let model = build_text(
            &"abcdefghijklmnop\n".repeat(50_000),
            &inspector.options,
            false,
            "UTF-8",
            false,
            &|| false,
        )
        .unwrap();
        inspector.content = Some(Data::Text {
            info: info(),
            model,
        });
        for frame in 0..20 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(340.0, 680.0))),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    let rect = ui.max_rect();
                    inspector.ui(ui, rect, &Theme::default());
                },
            );
            output.textures_delta.clear();
            assert!(inspector.layouts.len() < 64);
            assert!(inspector.layout_chars <= LAYOUT_CHARS);
            if frame > 4 && !ctx.has_requested_repaint() {
                assert_eq!(
                    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                    Duration::MAX
                );
                return;
            }
        }
        panic!("settled inspector kept repainting");
    }

    #[test]
    fn contents_size_from_original_image_uses_one_to_one_baseline() {
        let mut inspector = Inspector::new(Arc::new(|| {})).unwrap();
        inspector.content = Some(Data::Image {
            info: info(),
            original: [100, 100],
            preview: [100, 100],
            pixels: None,
        });
        inspector.image_scale = ImageScale::Original;
        inspector.image_zoom = 4.0;
        inspector.change_contents_size(true);
        assert_eq!(inspector.image_zoom, 1.25);
        assert_eq!(inspector.image_scale, ImageScale::Custom);
    }

    #[test]
    fn theme_inspector_font_preserves_local_contents_zoom() {
        let mut inspector = Inspector::new(Arc::new(|| {})).unwrap();
        inspector.change_contents_size(true);
        let mut theme = Theme {
            inspector_font_size: 18.0,
            ..Theme::default()
        };
        inspector.sync_theme_font(&theme);
        assert!((inspector.font_size - 19.0).abs() < 0.001);
        inspector.sync_theme_font(&theme);
        assert!((inspector.font_size - 19.0).abs() < 0.001);
        theme.inspector_font_size = 28.0;
        inspector.sync_theme_font(&theme);
        assert_eq!(inspector.font_size, 28.0);
        theme.inspector_font_size = 18.0;
        inspector.sync_theme_font(&theme);
        assert!((inspector.font_size - 19.0).abs() < 0.001);
    }

    #[cfg(not(windows))]
    #[test]
    fn metadata_time_labels_are_explicit_utc_and_floor_before_epoch() {
        assert_eq!(format_time(UNIX_EPOCH), "1970-01-01 00:00 UTC");
        assert_eq!(
            format_time(UNIX_EPOCH - Duration::from_millis(1)),
            "1969-12-31 23:59 UTC"
        );
    }
}

#[cfg(all(test, windows))]
mod cloud_policy_tests {
    use super::*;
    #[test]
    fn offline_selection_exposes_metadata_without_reading_a_preview() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        let path =
            std::env::temp_dir().join(format!("foldex-offline-preview-{}.txt", std::process::id()));
        fs::write(&path, b"must require explicit open").unwrap();
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        let before = unsafe { GetFileAttributesW(wide.as_ptr()) };
        assert_ne!(
            unsafe { SetFileAttributesW(wide.as_ptr(), before | FILE_ATTRIBUTE_OFFLINE) },
            0
        );
        let result = read_source(&path, &|| false).unwrap();
        let preview = crate::platform::read_preview(&path).unwrap();
        assert_ne!(unsafe { SetFileAttributesW(wide.as_ptr(), before) }, 0);
        fs::remove_file(&path).unwrap();
        assert!(
            matches!(result, Source::Info { info: Some(ref info), .. } if info.availability == foldex_fs::capabilities::Availability::OnlineOnly)
        );
        assert!(matches!(
            preview,
            crate::platform::PreviewContent::Info { .. }
        ));
    }
}
