//! The UI owns state only; a single sleeping worker owns network and disk I/O.
use foldex_update::{Downloaded, Settings, Update};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const MANUAL_COOLDOWN: Duration = Duration::from_secs(60);
static HANDOFF: Mutex<Option<Downloaded>> = Mutex::new(None);

enum Command {
    Check(bool),
    Save(Settings),
    Download(Update),
    Prepare(Downloaded, bool),
}
enum Event {
    Loaded(Result<Settings, String>),
    Checked(Result<Option<Update>, String>),
    Saved(Result<(), String>),
    Downloaded(Result<Downloaded, String>),
    Prepared(Result<Downloaded, String>),
}

pub struct Updates {
    pub open: bool,
    pub prepared: bool,
    pub exit_pending: bool,
    settings: Settings,
    settings_before_save: Option<Settings>,
    commands: Option<SyncSender<Command>>,
    events: Option<Receiver<Event>>,
    ready: Option<Downloaded>,
    available: Option<Update>,
    busy: bool,
    downloading: bool,
    loaded: bool,
    error: Option<String>,
    checking: bool,
    verifying: bool,
    up_to_date: bool,
    next_check: Instant,
    last_attempt: Option<Instant>,
    notified_version: Option<String>,
    notify: bool,
    managed: bool,
    progress: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub enum CatalogPhase {
    Unavailable,
    Checking,
    Current,
    Available { managed: bool },
    Downloading,
    Error,
    Installing,
    Notification,
}

impl Updates {
    /// Presentation states for the offscreen catalog. The worker stays absent,
    /// so no state here can reach the network or the installer.
    #[cfg(test)]
    pub fn catalog(&mut self, phase: CatalogPhase) {
        let version = "0.2.0";
        let name = format!("Foldex-{version}-windows-x64-setup.exe");
        let release = serde_json::json!([{
            "tag_name": format!("v{version}"),
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": name,
                "browser_download_url": format!(
                    "{}/releases/download/v{version}/{name}",
                    foldex_update::REPOSITORY
                ),
                "size": 18_400_000,
                "state": "uploaded",
                "digest": format!("sha256:{}", "ab".repeat(32)),
            }],
        }]);
        let update = foldex_update::select_update(
            &serde_json::to_vec(&release).unwrap(),
            env!("CARGO_PKG_VERSION"),
            false,
        )
        .unwrap();
        self.open = true;
        self.loaded = true;
        self.busy = false;
        self.managed = true;
        self.error = None;
        self.available = None;
        self.downloading = false;
        self.exit_pending = false;
        self.checking = false;
        self.up_to_date = false;
        match phase {
            CatalogPhase::Unavailable => self.loaded = false,
            CatalogPhase::Checking => {
                self.busy = true;
                self.checking = true;
            }
            CatalogPhase::Current => self.up_to_date = true,
            CatalogPhase::Available { managed } => {
                self.managed = managed;
                self.available = update;
            }
            CatalogPhase::Downloading => {
                self.available = update;
                self.busy = true;
                self.downloading = true;
                self.progress.store(7_200_000, Ordering::Relaxed);
            }
            CatalogPhase::Error => {
                self.error = Some("Couldn't reach GitHub. Check your connection and try again. (timed out after 10 s)".into());
            }
            CatalogPhase::Installing => {
                self.available = update;
                self.begin_exit();
            }
            CatalogPhase::Notification => {
                self.open = false;
                self.available = update;
                self.notify = true;
            }
        }
    }

    pub fn new(
        state_dir: PathBuf,
        managed: bool,
        enabled: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let mut service = Self {
            open: false,
            prepared: false,
            exit_pending: false,
            settings: Settings::default(),
            settings_before_save: None,
            commands: None,
            events: None,
            ready: None,
            available: None,
            busy: enabled,
            downloading: false,
            loaded: false,
            error: None,
            checking: false,
            verifying: false,
            up_to_date: false,
            next_check: Instant::now() + Duration::from_secs(15),
            last_attempt: None,
            notified_version: None,
            notify: false,
            managed,
            progress: Arc::new(AtomicU64::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        if enabled {
            let (commands, requests) = mpsc::sync_channel(2);
            let (events, replies) = mpsc::sync_channel(4);
            let progress = service.progress.clone();
            let cancelled = service.cancelled.clone();
            match std::thread::Builder::new().name("foldex-updates".into()).spawn(move || {
                let settings_path = state_dir.join("foldex-updates.json");
                let loaded = foldex_update::load_settings(&settings_path).map_err(|error| error.to_string());
                if events.send(Event::Loaded(loaded)).is_err() { return; }
                wake();
                clean_staging(&state_dir.join("updates"));
                while let Ok(command) = requests.recv() {
                    let event = match command {
                        Command::Check(previews) => Event::Checked(foldex_update::check(env!("CARGO_PKG_VERSION"), previews)),
                        Command::Save(settings) => Event::Saved(foldex_update::save_settings(&settings_path, &settings).map_err(|error| error.to_string())),
                        Command::Download(update) => {
                            let mut painted = Instant::now();
                            Event::Downloaded(foldex_update::download(update, &state_dir.join("updates"), &cancelled, |bytes| {
                                progress.store(bytes, Ordering::Relaxed);
                                if painted.elapsed() >= Duration::from_millis(250) { wake(); painted = Instant::now(); }
                            }))
                        }
                        Command::Prepare(downloaded, previews) => {
                            let result = foldex_update::check(env!("CARGO_PKG_VERSION"), previews).and_then(|current| {
                                crate::installation::ensure_only_process()?;
                                if !current.as_ref().is_some_and(|current| downloaded.matches(current)) {
                                    return Err("This update has changed or is no longer available. Check for updates and download it again.".into());
                                }
                                downloaded.verify()?;
                                Ok(downloaded)
                            });
                            Event::Prepared(result)
                        }
                    };
                    if events.send(event).is_err() { return; }
                    wake();
                }
            }) {
                Ok(_) => { service.commands = Some(commands); service.events = Some(replies); }
                Err(error) => { service.error = Some(error.to_string()); service.busy = false; }
            }
        }
        service
    }

    fn submit(&mut self, command: Command) -> bool {
        if self.busy || self.exit_pending {
            return false;
        }
        if self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.try_send(command).is_ok())
        {
            self.busy = true;
            self.error = None;
            true
        } else {
            self.error =
                Some("The update worker is unavailable. Restart Foldex and try again.".into());
            false
        }
    }

    fn check(&mut self) {
        if let Some(last) = self.last_attempt
            && last.elapsed() < MANUAL_COOLDOWN
        {
            self.next_check = last + MANUAL_COOLDOWN;
            return;
        }
        self.next_check = Instant::now() + CHECK_INTERVAL;
        if self.submit(Command::Check(self.settings.include_prereleases)) {
            self.checking = true;
            self.last_attempt = Some(Instant::now());
            self.prepared = false;
        }
    }

    fn download(&mut self) {
        if let Some(update) = self.available.clone() {
            self.cancelled.store(false, Ordering::Release);
            self.progress.store(0, Ordering::Relaxed);
            self.downloading = self.submit(Command::Download(update));
        }
    }

    pub fn pump(&mut self, ctx: &egui::Context) {
        loop {
            let event = match self.events.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => Some(event),
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.commands = None;
                    self.events = None;
                    self.busy = false;
                    self.checking = false;
                    self.verifying = false;
                    self.loaded = false;
                    self.error =
                        Some("The update worker stopped. Restart Foldex and try again.".into());
                    None
                }
                _ => None,
            };
            let Some(event) = event else { break };
            self.busy = false;
            match event {
                Event::Loaded(result) => {
                    self.loaded = true;
                    match result {
                        Ok(settings) => self.settings = settings,
                        Err(error) => {
                            // A broken preferences file must not silently enable network activity.
                            self.settings.check_automatically = false;
                            self.error =
                                Some(format!("Could not load update preferences: {error}"));
                        }
                    }
                }
                Event::Saved(result) => {
                    if let Err(error) = result {
                        if let Some(previous) = self.settings_before_save.take() {
                            self.settings = previous;
                        }
                        self.error = Some(format!("Could not save update preferences: {error}"));
                    } else {
                        self.settings_before_save = None;
                        if self.available.is_some()
                            && self.settings.download_automatically
                            && self.managed
                            && self.ready.is_none()
                        {
                            self.download();
                        }
                    }
                }
                Event::Checked(result) => {
                    self.checking = false;
                    self.next_check = Instant::now() + CHECK_INTERVAL;
                    match result {
                        Ok(update) => {
                            self.available = update;
                            if !self.ready.as_ref().is_some_and(|ready| {
                                self.available
                                    .as_ref()
                                    .is_some_and(|update| ready.matches(update))
                            }) {
                                self.ready = None;
                            }
                            if let Some(update) = &self.available {
                                let version = update.version();
                                if self.notified_version.as_ref() != Some(&version) {
                                    self.notify = true;
                                    self.notified_version = Some(version);
                                }
                            }
                            self.up_to_date = self.available.is_none();
                            if self.available.is_some()
                                && self.settings.download_automatically
                                && self.managed
                                && self.ready.is_none()
                            {
                                self.download();
                            }
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Event::Downloaded(result) => {
                    self.downloading = false;
                    match result {
                        Ok(downloaded) => {
                            self.ready = Some(downloaded);
                            self.notify = true;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Event::Prepared(result) => {
                    self.verifying = false;
                    match result {
                        Ok(downloaded) => {
                            self.ready = Some(downloaded);
                            self.prepared = true;
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.open = true;
                        }
                    }
                }
            }
        }
        if self.loaded && self.settings.check_automatically && !self.exit_pending && !self.prepared
        {
            if !self.busy && Instant::now() >= self.next_check {
                self.check();
            }
            if !self.busy {
                ctx.request_repaint_after(
                    self.next_check.saturating_duration_since(Instant::now()),
                );
            }
        }
    }

    pub fn arm_handoff(&mut self) -> bool {
        if !self.prepared {
            return false;
        }
        let Ok(mut handoff) = HANDOFF.lock() else {
            return false;
        };
        *handoff = self.ready.take();
        self.prepared = false;
        handoff.is_some()
    }

    pub fn cancel_exit(&mut self, error: &str) {
        self.exit_pending = false;
        self.prepared = false;
        self.error = Some(error.into());
        self.open = true;
    }
    pub fn begin_exit(&mut self) {
        self.exit_pending = true;
        self.open = true;
    }

    fn phase(&self) -> Phase {
        if self.exit_pending {
            Phase::Installing
        } else if self.downloading {
            Phase::Downloading
        } else if self.verifying {
            Phase::Verifying
        } else if self.ready.is_some() {
            Phase::Ready
        } else if self.available.is_some() {
            Phase::Available
        } else if self.checking {
            Phase::Checking
        } else if self.busy && !self.loaded {
            Phase::Loading
        } else if !self.loaded {
            Phase::Unavailable
        } else if self.up_to_date {
            Phase::Current
        } else {
            Phase::Idle
        }
    }

    /// A small card in the window corner; it never takes focus from the files.
    pub fn notification(&mut self, ctx: &egui::Context, theme: &foldex_ui::Theme) {
        use foldex_ui::dialog::{Kind, Text, button_at, button_width, text};
        if !self.notify || self.open || self.exit_pending {
            return;
        }
        let Some(update) = &self.available else {
            return;
        };
        let t = theme;
        let s = t.metrics().scale;
        let id = egui::Id::new("update-notification");
        let width = 300.0 * s;
        let screen = ctx.content_rect();
        let body = format!(
            "Foldex {} is {}.",
            update.version(),
            if self.ready.is_some() {
                "ready to install"
            } else {
                "available"
            }
        );
        let height = ctx
            .memory(|memory| memory.area_rect(id))
            .map_or(120.0 * s, |rect| rect.height());
        let origin = screen.right_bottom() - egui::vec2(width + 18.0 * s, height + 18.0 * s);
        let mut view = false;
        let mut dismiss = false;
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .show(ctx, |ui| {
                let pad = 16.0 * s;
                let frame = egui::Frame::NONE
                    .fill(foldex_ui::Theme::color(t.surface))
                    .stroke(egui::Stroke::new(1.0, t.line()))
                    .corner_radius(t.radius.saturating_add(3))
                    .inner_margin(egui::Margin::same(pad as i8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(
                            if t.resolved_mode == foldex_ui::ResolvedMode::Dark {
                                100
                            } else {
                                32
                            },
                        ),
                    });
                frame.show(ui, |ui| {
                    ui.set_width(width - 2.0 * pad);
                    ui.spacing_mut().item_spacing.y = 4.0 * s;
                    text(ui, t, Text::Heading, "Update available");
                    text(ui, t, Text::Secondary, &body);
                    ui.add_space(10.0 * s);
                    let (row, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), t.metrics().control),
                        egui::Sense::hover(),
                    );
                    let view_width = button_width(ui, t, "View update");
                    let later_width = button_width(ui, t, "Later");
                    let view_rect = egui::Rect::from_min_max(
                        egui::pos2(row.right() - view_width, row.top()),
                        row.max,
                    );
                    let later_rect = egui::Rect::from_min_max(
                        egui::pos2(view_rect.left() - 8.0 * s - later_width, row.top()),
                        egui::pos2(view_rect.left() - 8.0 * s, row.bottom()),
                    );
                    dismiss = button_at(ui, later_rect, id.with("later"), t, "Later", Kind::Ghost)
                        .clicked();
                    view = button_at(
                        ui,
                        view_rect,
                        id.with("view"),
                        t,
                        "View update",
                        Kind::Primary,
                    )
                    .clicked();
                });
            });
        if view {
            self.open = true;
        }
        if view || dismiss {
            self.notify = false;
        }
    }

    /// Installation needs an explicit action. The app then verifies idle state
    /// again, saves preferences, exits its tray, and starts Setup.
    pub fn ui(&mut self, ctx: &egui::Context, theme: &foldex_ui::Theme, can_install: bool) {
        use foldex_ui::dialog::{
            Action, Dialog, Footer, Glyph, Kind, Size, Text, Tone, callout, checkbox, glyph, inset,
            progress, section, spinner, text, text_at, text_colored,
        };
        if !self.open {
            return;
        }
        let t = theme;
        let s = t.metrics().scale;
        let id = egui::Id::new("updates-dialog");
        let phase = self.phase();
        let version = self
            .available
            .as_ref()
            .map(Update::version)
            .unwrap_or_default();
        let current = env!("CARGO_PKG_VERSION");
        let cooling_down = self
            .last_attempt
            .is_some_and(|last| last.elapsed() < MANUAL_COOLDOWN);
        let can_check = self.loaded && !self.busy && !cooling_down && !self.exit_pending;
        let (headline, detail) = match phase {
            Phase::Unavailable => (
                "Updates aren't available here".to_owned(),
                "This window can't check for new versions.".to_owned(),
            ),
            Phase::Loading => ("Loading update preferences…".to_owned(), String::new()),
            Phase::Checking => ("Checking for updates…".to_owned(), String::new()),
            Phase::Idle => (
                "Check for a new version".to_owned(),
                "Foldex looks for updates on GitHub.".to_owned(),
            ),
            Phase::Current => (
                "You're up to date".to_owned(),
                format!("Foldex {current} is the latest version."),
            ),
            Phase::Available if self.managed => (
                format!("Foldex {version} is available"),
                format!(
                    "{} download. Foldex restarts to install it.",
                    self.available
                        .as_ref()
                        .map_or_else(String::new, |update| foldex_ui::format_size(update.size()))
                ),
            ),
            Phase::Available => (
                format!("Foldex {version} is available"),
                "This is a portable copy. Download the installer to update.".to_owned(),
            ),
            Phase::Downloading => (format!("Downloading Foldex {version}…"), String::new()),
            Phase::Verifying => ("Verifying the update…".to_owned(), String::new()),
            Phase::Ready => (
                format!("Foldex {version} is ready to install"),
                "Foldex closes, installs the update and opens again.".to_owned(),
            ),
            Phase::Installing => (
                "Saving your workspace…".to_owned(),
                "Foldex closes and installs the update next.".to_owned(),
            ),
        };
        let mut actions = Vec::new();
        let check_label = if phase == Phase::Current {
            "Check again"
        } else {
            "Check for updates"
        };
        match phase {
            Phase::Unavailable | Phase::Loading | Phase::Checking => {}
            Phase::Idle | Phase::Current => actions.push(
                Action::new(
                    id.with("check"),
                    check_label,
                    if phase == Phase::Idle || self.error.is_some() {
                        Kind::Primary
                    } else {
                        Kind::Secondary
                    },
                )
                .enabled(can_check),
            ),
            Phase::Available | Phase::Ready | Phase::Verifying => {
                actions.push(Action::new(id.with("notes"), "Release notes", Kind::Ghost));
                let label = match (phase, self.managed) {
                    (Phase::Available, true) => "Download update",
                    (Phase::Available, false) => "Download installer",
                    _ => "Install and restart",
                };
                actions.push(
                    Action::new(id.with("primary"), label, Kind::Primary)
                        .enabled(!self.busy && (phase == Phase::Available || can_install)),
                );
            }
            Phase::Downloading => actions.push(Action::new(
                id.with("cancel-download"),
                "Cancel download",
                Kind::Secondary,
            )),
            Phase::Installing => actions.push(Action::new(
                id.with("cancel-install"),
                "Cancel installation",
                Kind::Secondary,
            )),
        }
        let subtitle = format!("Foldex {current}");
        let mut open = true;
        let previous = self.settings.clone();
        let shown = Dialog::new(id, "Updates", Size::Medium.width(t))
            .subtitle(&subtitle)
            .footer(Footer::new(actions))
            .show(ctx, t, &mut open, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0 * s;
                // An error replaces the idle prompt rather than stacking under it.
                let status = !(phase == Phase::Idle && self.error.is_some());
                if status {
                    let size = 18.0 * s;
                    let (row, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 24.0 * s),
                        egui::Sense::hover(),
                    );
                    let center = egui::pos2(row.left() + size * 0.5, row.center().y);
                    match phase {
                        Phase::Loading
                        | Phase::Checking
                        | Phase::Downloading
                        | Phase::Verifying
                        | Phase::Installing => spinner(ui, center, size, t.accent()),
                        Phase::Available | Phase::Ready => {
                            glyph(ui, center, size, Glyph::Info, t.accent())
                        }
                        Phase::Current => glyph(ui, center, size, Glyph::Done, t.dim()),
                        Phase::Idle | Phase::Unavailable => {
                            glyph(ui, center, size, Glyph::Info, t.dim())
                        }
                    }
                    let indent = size + 12.0 * s;
                    text_at(
                        ui,
                        egui::Rect::from_min_max(
                            egui::pos2(row.left() + indent, row.top()),
                            row.max,
                        ),
                        t,
                        Text::Heading,
                        &headline,
                        t.fg(),
                        egui::Align2::LEFT_CENTER,
                    );
                    inset(ui, indent, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0 * s;
                        if !detail.is_empty() {
                            text(ui, t, Text::Secondary, &detail);
                        }
                        if phase == Phase::Downloading {
                            let total = self.available.as_ref().map_or(1, Update::size).max(1);
                            let done = self.progress.load(Ordering::Relaxed).min(total);
                            ui.add_space(4.0 * s);
                            progress(ui, t, Some(done as f32 / total as f32));
                            text_colored(
                                ui,
                                t,
                                Text::Caption,
                                &format!(
                                    "{} of {}",
                                    foldex_ui::format_size(done),
                                    foldex_ui::format_size(total)
                                ),
                                t.dim(),
                            );
                        }
                    });
                }
                if let Some(error) = &self.error {
                    if status {
                        ui.add_space(6.0 * s);
                    }
                    callout(
                        ui,
                        t,
                        Tone::Danger,
                        &crate::friendly::present("Couldn't complete the update", error),
                    );
                }
                if phase == Phase::Ready && !can_install {
                    ui.add_space(6.0 * s);
                    callout(
                        ui,
                        t,
                        Tone::Neutral,
                        "Finish file operations and close open dialogs to install.",
                    );
                }
                if phase == Phase::Unavailable {
                    return;
                }
                section(ui, t, "Preferences");
                ui.add_enabled_ui(self.loaded && !self.busy && !self.exit_pending, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    checkbox(
                        ui,
                        t,
                        &mut self.settings.check_automatically,
                        "Check automatically every 6 hours",
                    );
                    if self.managed {
                        checkbox(
                            ui,
                            t,
                            &mut self.settings.download_automatically,
                            "Download updates in the background",
                        );
                    }
                    checkbox(
                        ui,
                        t,
                        &mut self.settings.include_prereleases,
                        "Include preview releases",
                    );
                });
            });
        if previous != self.settings {
            if previous.include_prereleases != self.settings.include_prereleases {
                self.ready = None;
                self.available = None;
                self.prepared = false;
                self.up_to_date = false;
                self.next_check = Instant::now();
            }
            if self.submit(Command::Save(self.settings.clone())) {
                self.settings_before_save = Some(previous);
            } else {
                self.settings = previous;
            }
        }
        let mut cancel = false;
        if let Some(index) = shown.clicked {
            let action = shown.actions[index].id;
            if action == id.with("check") {
                self.check();
            } else if action == id.with("notes") {
                if let Some(update) = &self.available {
                    ctx.open_url(egui::OpenUrl::new_tab(update.release_url()));
                }
            } else if action == id.with("primary") {
                match (phase, self.managed) {
                    (Phase::Available, true) => self.download(),
                    (Phase::Available, false) => {
                        if let Some(update) = &self.available {
                            ctx.open_url(egui::OpenUrl::new_tab(update.release_url()));
                        }
                    }
                    _ => {
                        if let Some(downloaded) = self.ready.take() {
                            self.verifying = self.submit(Command::Prepare(
                                downloaded,
                                self.settings.include_prereleases,
                            ));
                        }
                    }
                }
            } else if action == id.with("cancel-download") {
                self.cancelled.store(true, Ordering::Release);
            } else if action == id.with("cancel-install") {
                cancel = true;
            }
        }
        if let Some(remaining) = cooling_down_for(self.last_attempt) {
            ctx.request_repaint_after(remaining);
        }
        self.open = open && !cancel;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Unavailable,
    Loading,
    Checking,
    Idle,
    Current,
    Available,
    Downloading,
    Verifying,
    Ready,
    Installing,
}

fn cooling_down_for(last: Option<Instant>) -> Option<Duration> {
    last.and_then(|last| MANUAL_COOLDOWN.checked_sub(last.elapsed()))
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Called after the native event loop has returned. Setup waits for this process
/// to terminate before checking app mutexes or replacing the loaded executable.
pub fn finish_handoff() -> Result<(), String> {
    let downloaded = HANDOFF.lock().map_err(|error| error.to_string())?.take();
    let Some(downloaded) = downloaded else {
        return Ok(());
    };
    downloaded.verify()?;
    let path = downloaded.path();
    let directory = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .ok_or("Could not find the Foldex installation directory")?;
    std::process::Command::new(&path)
        .arg("/SP-")
        .arg("/SILENT")
        .arg("/NORESTART")
        .arg("/NOCLOSEAPPLICATIONS")
        .arg("/NORESTARTAPPLICATIONS")
        .arg("/UPDATE=1")
        .arg("/RESTARTAPP=1")
        .arg(format!("/WAITPID={}", std::process::id()))
        .arg(format!("/DIR={}", directory.display()))
        .spawn()
        .map_err(|error| format!("Could not start the update installer: {error}"))?;
    downloaded.keep();
    Ok(())
}

fn clean_staging(cache: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("installer-")
        {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(7 * 24 * 60 * 60));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with(event: Event) -> (Updates, SyncSender<Event>) {
        let mut updates = Updates::new(PathBuf::new(), false, false, Arc::new(|| {}));
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(event).unwrap();
        updates.events = Some(receiver);
        (updates, sender)
    }

    #[test]
    fn corrupt_preferences_disable_automatic_requests() {
        let (mut updates, _sender) = service_with(Event::Loaded(Err("invalid JSON".into())));
        updates.pump(&egui::Context::default());
        assert!(!updates.settings.check_automatically);
        assert!(updates.error.as_ref().unwrap().contains("invalid JSON"));
    }

    #[test]
    fn a_failed_settings_save_restores_the_previous_preferences() {
        let (mut updates, _sender) = service_with(Event::Saved(Err("disk full".into())));
        let previous = Settings::default();
        updates.settings.check_automatically = false;
        updates.settings_before_save = Some(previous.clone());
        updates.pump(&egui::Context::default());
        assert_eq!(updates.settings, previous);
        assert!(updates.error.as_ref().unwrap().contains("disk full"));
    }

    #[test]
    fn worker_failure_does_not_leave_the_ui_waiting_forever() {
        let mut updates = Updates::new(PathBuf::new(), false, false, Arc::new(|| {}));
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        updates.events = Some(receiver);
        updates.busy = true;
        updates.pump(&egui::Context::default());
        assert!(!updates.busy);
        assert!(updates.error.as_ref().unwrap().contains("stopped"));
    }
}
