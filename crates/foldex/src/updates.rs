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
    status: String,
    next_check: Instant,
    last_attempt: Option<Instant>,
    notified_version: Option<String>,
    notify: bool,
    managed: bool,
    progress: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
}

impl Updates {
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
            status: if enabled {
                "Loading update preferences…"
            } else {
                "Update checks are unavailable in this window."
            }
            .into(),
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

    fn submit(&mut self, command: Command, status: &str) -> bool {
        if self.busy || self.exit_pending {
            return false;
        }
        if self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.try_send(command).is_ok())
        {
            self.busy = true;
            self.status = status.into();
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
        if self.submit(
            Command::Check(self.settings.include_prereleases),
            "Checking for updates…",
        ) {
            self.last_attempt = Some(Instant::now());
            self.prepared = false;
        }
    }

    fn download(&mut self) {
        if let Some(update) = self.available.clone() {
            self.cancelled.store(false, Ordering::Release);
            self.progress.store(0, Ordering::Relaxed);
            self.downloading = self.submit(Command::Download(update), "Downloading update…");
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
                        Ok(settings) => {
                            self.settings = settings;
                            self.status = "Ready to check for updates.".into();
                        }
                        Err(error) => {
                            // A broken preferences file must not silently enable network activity.
                            self.settings.check_automatically = false;
                            self.status =
                                "Automatic checks are off. You can still check manually.".into();
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
                        self.status = "Update preferences were not changed.".into();
                    } else {
                        self.settings_before_save = None;
                        self.status = "Update preferences saved.".into();
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
                            self.status = if let Some(update) = &self.available {
                                let version = update.version();
                                if self.notified_version.as_ref() != Some(&version) {
                                    self.notify = true;
                                    self.notified_version = Some(version.clone());
                                }
                                format!("Foldex {version} is available.")
                            } else {
                                "You're up to date.".into()
                            };
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
                            self.status =
                                "Update downloaded and verified. Ready to install.".into();
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Event::Prepared(result) => match result {
                    Ok(downloaded) => {
                        self.ready = Some(downloaded);
                        self.prepared = true;
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.open = true;
                    }
                },
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
        self.status = "Saving your workspace before installing…".into();
    }

    pub fn notification(&mut self, ctx: &egui::Context, theme: &foldex_ui::Theme) {
        if !self.notify || self.open || self.exit_pending {
            return;
        }
        let Some(update) = &self.available else {
            return;
        };
        let mut visible = true;
        egui::Window::new("Foldex update available")
            .id(egui::Id::new("update-notification"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
            .resizable(false)
            .collapsible(false)
            .open(&mut visible)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Foldex {} {}",
                    update.version(),
                    if self.ready.is_some() {
                        "is ready to install."
                    } else {
                        "is available."
                    }
                ));
                if foldex_ui::components::text_button(ui, theme, "View update", true).clicked() {
                    self.open = true;
                }
            });
        self.notify = visible;
    }

    /// Installation needs an explicit action. The app then verifies idle state
    /// again, saves preferences, exits its tray, and starts Setup.
    pub fn ui(&mut self, ctx: &egui::Context, theme: &foldex_ui::Theme, can_install: bool) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mut cancel = false;
        let width =
            (520.0 * theme.metrics().scale).min((ctx.content_rect().width() - 24.0).max(240.0));
        foldex_ui::components::dialog(
            ctx,
            egui::Id::new("updates-dialog"),
            theme,
            "Application Updates",
            width,
            &mut open,
            |ui| {
                ui.label(format!("Current version: {}", env!("CARGO_PKG_VERSION")));
                ui.label(&self.status);
                if let Some(error) = &self.error {
                    ui.colored_label(
                        foldex_ui::Theme::color(theme.danger),
                        crate::friendly::present("Couldn't complete the update", error),
                    );
                }
                ui.add_space(theme.metrics().gap);
                let previous = self.settings.clone();
                ui.add_enabled_ui(self.loaded && !self.busy && !self.exit_pending, |ui| {
                    foldex_ui::components::checkbox(
                        ui,
                        theme,
                        &mut self.settings.check_automatically,
                        "Check automatically (every 6 hours)",
                    );
                    ui.add_enabled_ui(self.managed, |ui| {
                        foldex_ui::components::checkbox(
                            ui,
                            theme,
                            &mut self.settings.download_automatically,
                            "Download updates automatically",
                        );
                    });
                    foldex_ui::components::checkbox(
                        ui,
                        theme,
                        &mut self.settings.include_prereleases,
                        "Include prerelease versions",
                    );
                });
                if previous != self.settings {
                    if previous.include_prereleases != self.settings.include_prereleases {
                        self.ready = None;
                        self.available = None;
                        self.prepared = false;
                        self.next_check = Instant::now();
                    }
                    if self.submit(
                        Command::Save(self.settings.clone()),
                        "Saving update preferences…",
                    ) {
                        self.settings_before_save = Some(previous);
                    } else {
                        self.settings = previous;
                    }
                }
                if !self.managed {
                    ui.label("Portable copy: in-app install is unavailable.");
                }
                if self.downloading {
                    let total = self.available.as_ref().map_or(1, Update::size);
                    let fraction = self.progress.load(Ordering::Relaxed) as f32 / total as f32;
                    ui.add(egui::ProgressBar::new(fraction).show_percentage());
                    if foldex_ui::components::text_button(ui, theme, "Cancel download", false)
                        .clicked()
                    {
                        self.cancelled.store(true, Ordering::Release);
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    let cooling_down = self
                        .last_attempt
                        .is_some_and(|last| last.elapsed() < MANUAL_COOLDOWN);
                    ui.add_enabled_ui(
                        self.loaded && !self.busy && !cooling_down && !self.exit_pending,
                        |ui| {
                            if foldex_ui::components::text_button(
                                ui,
                                theme,
                                "Check for updates",
                                false,
                            )
                            .clicked()
                            {
                                self.check();
                            }
                        },
                    );
                    if let Some(update) = self.available.clone() {
                        if foldex_ui::components::text_button(ui, theme, "Release notes", false)
                            .clicked()
                        {
                            ctx.open_url(egui::OpenUrl::new_tab(update.release_url()));
                        }
                        if self.managed {
                            ui.add_enabled_ui(!self.busy && !self.exit_pending, |ui| {
                                if self.ready.is_some() {
                                    ui.add_enabled_ui(can_install, |ui| {
                                        if foldex_ui::components::text_button(
                                            ui,
                                            theme,
                                            "Install and restart",
                                            true,
                                        )
                                        .clicked()
                                        {
                                            if let Some(downloaded) = self.ready.take() {
                                                self.submit(
                                                    Command::Prepare(
                                                        downloaded,
                                                        self.settings.include_prereleases,
                                                    ),
                                                    "Verifying update before installation…",
                                                );
                                            }
                                        }
                                    });
                                } else if foldex_ui::components::text_button(
                                    ui,
                                    theme,
                                    "Download update",
                                    true,
                                )
                                .clicked()
                                {
                                    self.download();
                                }
                            });
                        } else if foldex_ui::components::text_button(
                            ui,
                            theme,
                            "Download installer",
                            true,
                        )
                        .clicked()
                        {
                            ctx.open_url(egui::OpenUrl::new_tab(update.release_url()));
                        }
                    }
                });
                if !can_install && self.ready.is_some() {
                    ui.label("Finish file operations first.");
                }
                if let Some(remaining) = cooling_down_for(self.last_attempt) {
                    ctx.request_repaint_after(remaining);
                }
                if self.exit_pending
                    && foldex_ui::components::text_button(ui, theme, "Cancel installation", false)
                        .clicked()
                {
                    cancel = true;
                }
            },
        );
        self.open = open && !cancel;
    }
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
