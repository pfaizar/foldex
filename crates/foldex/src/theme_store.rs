//! Bounded, app-local color scheme library. Native dialogs and all file I/O run
//! on a sleeping worker; UI frames only drain replies and debounce preferences.
use foldex_ui::{ColorScheme, Theme};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
mod native;

pub(crate) const MAX_SCHEMES: usize = 128;
const MAX_LIBRARY_ENTRIES: usize = 512;
const SAVE_DELAY: Duration = Duration::from_millis(300);

enum Request {
    List,
    Save(ColorScheme),
    Import(isize),
    Export(isize, ColorScheme),
    Preferences(Vec<u8>),
}
enum Reply {
    Listed(Result<Vec<ColorScheme>, String>),
    Changed(Result<Option<ColorScheme>, String>, bool),
    Exported(Result<bool, String>),
    Preferences(Result<(), String>),
}
struct Worker {
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<Reply>,
}
pub(crate) struct Selection {
    pub scheme: ColorScheme,
    pub imported: bool,
}

#[derive(Default)]
pub(crate) struct Library {
    path: Option<PathBuf>,
    owner: isize,
    worker: Option<Worker>,
    pub schemes: Vec<ColorScheme>,
    pub busy: bool,
    pub status: Option<(String, bool)>,
    pending_preferences: Option<(Instant, Vec<u8>)>,
    last_preferences: Option<Vec<u8>>,
    preferences_error: Option<String>,
    preferences_inflight: usize,
}
impl Library {
    pub fn configure(&mut self, theme_path: PathBuf, owner: isize) {
        self.path = Some(theme_path);
        self.owner = owner;
    }
    pub fn start(&mut self, ctx: &egui::Context) {
        if self.worker.is_some() {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let directory = path.with_file_name("themes");
        let wake_ctx = ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || wake_ctx.request_repaint());
        let (tx, jobs) = mpsc::sync_channel(3);
        let (results, rx) = mpsc::sync_channel(4);
        match std::thread::Builder::new()
            .name("foldex-themes".into())
            .spawn(move || {
                while let Ok(request) = jobs.recv() {
                    let reply = match request {
                        Request::List => Reply::Listed(read_library(&directory)),
                        Request::Save(scheme) => {
                            Reply::Changed(save_scheme(&directory, scheme).map(Some), false)
                        }
                        Request::Import(owner) => Reply::Changed(
                            (|| {
                                let Some(source) = native::open(owner)? else {
                                    return Ok(None);
                                };
                                import_file(&directory, &source).map(Some)
                            })(),
                            true,
                        ),
                        Request::Export(owner, scheme) => {
                            Reply::Exported(export_scheme(owner, &scheme))
                        }
                        Request::Preferences(bytes) => Reply::Preferences(
                            crate::platform::atomic_save(&path, &bytes)
                                .map_err(|error| error.to_string()),
                        ),
                    };
                    if results.send(reply).is_err() {
                        break;
                    }
                    wake();
                }
            }) {
            Ok(_) => {
                self.worker = Some(Worker { tx, rx });
                self.busy = true;
                self.send(Request::List);
            }
            Err(error) => self.status = Some((error.to_string(), true)),
        }
    }
    fn send(&mut self, request: Request) -> bool {
        let Some(worker) = &self.worker else {
            self.status = Some(("The theme library is not available.".into(), true));
            self.busy = false;
            return false;
        };
        match worker.tx.try_send(request) {
            Ok(()) => true,
            Err(_) => {
                self.status = Some((
                    "The theme library is busy. Try again in a moment.".into(),
                    true,
                ));
                self.busy = false;
                false
            }
        }
    }
    pub fn save(&mut self, scheme: ColorScheme) {
        if self.busy {
            return;
        }
        self.status = Some(("Saving both color modes…".into(), false));
        self.busy = true;
        self.send(Request::Save(scheme));
    }
    pub fn import(&mut self) {
        if self.busy {
            return;
        }
        self.status = Some(("Choose a theme file…".into(), false));
        self.busy = true;
        self.send(Request::Import(self.owner));
    }
    pub fn export(&mut self, scheme: ColorScheme) {
        if self.busy {
            return;
        }
        self.status = Some(("Choose where to export the theme…".into(), false));
        self.busy = true;
        self.send(Request::Export(self.owner, scheme));
    }
    pub fn persist(&mut self, ctx: &egui::Context, theme: &Theme, immediate: bool) {
        if self.path.is_none() {
            return;
        }
        self.start(ctx);
        if self.worker.is_none() {
            self.preferences_error = Some("The appearance writer is unavailable.".into());
            self.pending_preferences = None;
            return;
        }
        match theme.write() {
            Ok(text) => {
                let bytes = text.into_bytes();
                if self.last_preferences.as_ref() == Some(&bytes) {
                    self.pending_preferences = None;
                    return;
                }
                self.pending_preferences = Some((
                    Instant::now()
                        + if immediate {
                            Duration::ZERO
                        } else {
                            SAVE_DELAY
                        },
                    bytes,
                ));
                ctx.request_repaint_after(if immediate {
                    Duration::ZERO
                } else {
                    SAVE_DELAY
                });
            }
            Err(error) => {
                self.preferences_error = Some(error.to_string());
                self.status = Some((error.to_string(), true));
            }
        }
    }
    /// Returns a scheme only after its atomic library save has succeeded.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Selection> {
        let mut selected = None;
        loop {
            let reply = match self.worker.as_ref().map(|worker| worker.rx.try_recv()) {
                Some(Ok(reply)) => reply,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.preferences_error = Some("The appearance writer stopped.".into());
                    self.preferences_inflight = 0;
                    self.pending_preferences = None;
                    self.worker = None;
                    self.busy = false;
                    break;
                }
                _ => break,
            };
            match reply {
                Reply::Listed(result) => {
                    self.busy = false;
                    match result {
                        Ok(schemes) => self.schemes = schemes,
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Reply::Changed(result, imported) => {
                    self.busy = false;
                    match result {
                        Ok(Some(scheme)) => {
                            self.schemes
                                .retain(|old| !same_name(&old.name, &scheme.name));
                            self.schemes.push(scheme.clone());
                            self.schemes
                                .sort_by_key(|scheme| scheme.name.to_lowercase());
                            self.status = Some((
                                format!("{} saved. Both color modes are included.", scheme.name),
                                false,
                            ));
                            selected = Some(Selection { scheme, imported });
                        }
                        Ok(None) => self.status = None,
                        Err(error) => self.status = Some((error, true)),
                    }
                }
                Reply::Exported(result) => {
                    self.busy = false;
                    self.status = match result {
                        Ok(true) => Some(("Theme exported with both color modes.".into(), false)),
                        Ok(false) => None,
                        Err(error) => Some((error, true)),
                    };
                }
                Reply::Preferences(result) => {
                    self.preferences_inflight = self.preferences_inflight.saturating_sub(1);
                    if let Err(error) = result {
                        self.last_preferences = None;
                        self.preferences_error = Some(error.clone());
                        self.status =
                            Some((format!("Appearance could not be saved: {error}"), true));
                    } else {
                        self.preferences_error = None;
                    }
                }
            }
        }
        if let Some((due, _)) = &self.pending_preferences {
            if *due <= Instant::now() && !self.busy {
                let (_, bytes) = self.pending_preferences.take().unwrap();
                if let Some(worker) = &self.worker {
                    match worker.tx.try_send(Request::Preferences(bytes.clone())) {
                        Ok(()) => {
                            self.last_preferences = Some(bytes);
                            self.preferences_inflight += 1;
                        }
                        Err(mpsc::TrySendError::Full(_)) => {
                            self.pending_preferences = Some((Instant::now() + SAVE_DELAY, bytes))
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => {
                            self.preferences_error =
                                Some("The appearance writer is unavailable.".into());
                            self.status =
                                Some(("The appearance writer is unavailable.".into(), true));
                            self.last_preferences = None;
                        }
                    }
                }
            }
            if let Some((due, _)) = &self.pending_preferences {
                // Dialog completion wakes the UI; no timer loop while a native
                // picker is open or an explicit library action is in progress.
                if !self.busy {
                    ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
                }
            }
        }
        selected
    }
    pub fn flush(&mut self, ctx: &egui::Context) {
        if let Some((due, _)) = &mut self.pending_preferences {
            let now = Instant::now();
            if *due > now {
                *due = now;
                ctx.request_repaint();
            }
        }
    }
    pub fn preferences_saved(&self) -> bool {
        self.pending_preferences.is_none() && self.preferences_inflight == 0 && !self.busy
    }
    pub fn preferences_error(&self) -> Option<&str> {
        if self.pending_preferences.is_none() && self.preferences_inflight == 0 {
            self.preferences_error.as_deref()
        } else {
            None
        }
    }
}

fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}
fn read_bounded(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > foldex_ui::theme::MAX_THEME_BYTES as u64 {
        return Err("Choose a theme file no larger than 64 KiB.".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(foldex_ui::theme::MAX_THEME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > foldex_ui::theme::MAX_THEME_BYTES {
        return Err("Theme file exceeds 64 KiB.".into());
    }
    String::from_utf8(bytes).map_err(|_| "Theme files must use UTF-8 text.".into())
}
fn read_library(directory: &Path) -> Result<Vec<ColorScheme>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut schemes = Vec::new();
    for entry in entries.take(MAX_LIBRARY_ENTRIES).flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("toml"))
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let Ok(text) = read_bounded(&path) else {
            continue;
        };
        let Ok(scheme) = ColorScheme::read(&text) else {
            continue;
        };
        if !same_name(&scheme.name, "Default")
            && !schemes
                .iter()
                .any(|old: &ColorScheme| same_name(&old.name, &scheme.name))
        {
            schemes.push(scheme);
            if schemes.len() == MAX_SCHEMES {
                break;
            }
        }
    }
    schemes.sort_by_key(|scheme| scheme.name.to_lowercase());
    Ok(schemes)
}
fn save_scheme(directory: &Path, mut scheme: ColorScheme) -> Result<ColorScheme, String> {
    scheme.name = scheme.name.trim().to_owned();
    scheme.validate()?;
    if same_name(&scheme.name, "Default") {
        return Err("Choose another name. The Default scheme is always available.".into());
    }
    let library = read_library(directory)?;
    if let Some(existing) = library
        .iter()
        .find(|old| same_name(&old.name, &scheme.name))
    {
        scheme.name = existing.name.clone();
    } else if library.len() >= MAX_SCHEMES {
        return Err(format!(
            "The theme library is full ({MAX_SCHEMES} schemes)."
        ));
    }
    let text = scheme.write()?;
    crate::platform::atomic_save(
        &directory.join(format!("{}.toml", scheme.name)),
        text.as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    Ok(scheme)
}
fn import_file(directory: &Path, source: &Path) -> Result<ColorScheme, String> {
    let scheme = ColorScheme::read_import(&read_bounded(source)?)?;
    let existing = read_library(directory)?;
    let scheme = import_name(scheme, &existing)?;
    save_scheme(directory, scheme)
}
fn export_scheme(owner: isize, scheme: &ColorScheme) -> Result<bool, String> {
    let Some(target) = native::save(owner, &scheme.name)? else {
        return Ok(false);
    };
    let text = scheme.write()?;
    crate::platform::atomic_save(&target, text.as_bytes()).map_err(|error| error.to_string())?;
    Ok(true)
}
fn import_name(mut scheme: ColorScheme, library: &[ColorScheme]) -> Result<ColorScheme, String> {
    if let Some(existing) = library
        .iter()
        .find(|old| same_name(&old.name, &scheme.name))
        && existing.dark == scheme.dark
        && existing.light == scheme.light
    {
        return Ok(existing.clone());
    }
    let base = if same_name(&scheme.name, "Default") {
        "Imported Default".to_owned()
    } else {
        scheme.name.clone()
    };
    for number in 1..=MAX_SCHEMES + 1 {
        let suffix = if number == 1 {
            String::new()
        } else {
            format!(" {number}")
        };
        let budget = foldex_ui::theme::MAX_SCHEME_NAME - suffix.chars().count();
        let mut stem: String = base.chars().take(budget).collect();
        while stem.len() + suffix.len() > 128 {
            stem.pop();
        }
        let name = format!("{}{suffix}", stem.trim_end());
        if !library.iter().any(|old| same_name(&old.name, &name)) {
            scheme.name = name;
            scheme.validate()?;
            return Ok(scheme);
        }
    }
    Err("The theme library has no room for another imported name.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_failed_preference_save_is_visible_until_a_successful_retry() {
        let fixture = Fixture::new();
        let blocker = fixture.0.join("blocked");
        fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("theme.toml");
        let ctx = egui::Context::default();
        let mut library = Library::default();
        library.configure(path.clone(), 0);
        library.persist(&ctx, &Theme::default(), true);
        let deadline = Instant::now() + Duration::from_secs(4);
        while library.preferences_error().is_none() {
            library.poll(&ctx);
            assert!(
                Instant::now() < deadline,
                "the failed save was not reported"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        fs::remove_file(&blocker).unwrap();
        fs::create_dir(&blocker).unwrap();
        library.persist(&ctx, &Theme::default(), true);
        while !library.preferences_saved() {
            library.poll(&ctx);
            assert!(Instant::now() < deadline, "the retry did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(library.preferences_error().is_none());
        assert!(path.is_file());
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "foldex-themes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn saved_library_roundtrips_both_modes_and_protects_default() {
        let fixture = Fixture::new();
        let mut scheme = Theme::default().saved_scheme("Ocean").unwrap();
        scheme.light.surface = [210, 220, 230];
        let saved = save_scheme(&fixture.0, scheme.clone()).unwrap();
        assert_eq!(read_library(&fixture.0).unwrap(), vec![saved.clone()]);
        assert_eq!(
            ColorScheme::read(&read_bounded(&fixture.0.join("Ocean.toml")).unwrap()).unwrap(),
            saved
        );
        assert!(save_scheme(&fixture.0, ColorScheme::default()).is_err());
        scheme.name = "ocean".into();
        save_scheme(&fixture.0, scheme).unwrap();
        assert_eq!(read_library(&fixture.0).unwrap().len(), 1);
    }
    #[test]
    fn imports_rename_conflicts_and_cannot_replace_default() {
        let existing = Theme::default().saved_scheme("Ocean").unwrap();
        let mut imported = existing.clone();
        imported.dark.accent = [1, 2, 3];
        let renamed = import_name(imported, &[existing.clone()]).unwrap();
        assert_eq!(renamed.name, "Ocean 2");
        assert_eq!(
            import_name(existing.clone(), &[existing.clone()]).unwrap(),
            existing
        );
        assert_eq!(
            import_name(ColorScheme::default(), &[]).unwrap().name,
            "Imported Default"
        );
    }
    #[test]
    fn invalid_and_oversized_library_files_are_ignored() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("invalid.toml"), "name = '../escape'").unwrap();
        let oversized = fixture.0.join("large.toml");
        fs::write(
            &oversized,
            vec![b'x'; foldex_ui::theme::MAX_THEME_BYTES + 1],
        )
        .unwrap();
        assert!(read_bounded(&oversized).is_err());
        assert!(read_library(&fixture.0).unwrap().is_empty());
    }

    #[test]
    fn importing_a_shared_file_saves_both_palettes_without_overwriting_a_conflict() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("library");
        let source = fixture.0.join("shared.toml");
        let original = Theme::default().saved_scheme("Ocean").unwrap();
        save_scheme(&directory, original.clone()).unwrap();
        let mut shared = original.clone();
        shared.light.accent = [19, 87, 140];
        shared.dark.surface = [11, 22, 33];
        fs::write(&source, shared.write().unwrap()).unwrap();
        let imported = import_file(&directory, &source).unwrap();
        assert_eq!(imported.name, "Ocean 2");
        assert_eq!(imported.dark, shared.dark);
        assert_eq!(imported.light, shared.light);
        assert_eq!(read_library(&directory).unwrap(), vec![original, imported]);
        fs::write(&source, "name = '../escape'\n[dark]\nbackground = [1,2,3]").unwrap();
        assert!(import_file(&directory, &source).is_err());
        assert_eq!(read_library(&directory).unwrap().len(), 2);
    }

    #[test]
    fn restoring_last_saved_preferences_discards_a_pending_different_palette() {
        let ctx = egui::Context::default();
        let (tx, _jobs) = mpsc::sync_channel(3);
        let (_results, rx) = mpsc::sync_channel(4);
        let original = Theme::default();
        let mut library = Library {
            path: Some(PathBuf::from("unused-theme.toml")),
            worker: Some(Worker { tx, rx }),
            last_preferences: Some(original.write().unwrap().into_bytes()),
            ..Default::default()
        };
        let mut edited = original.clone();
        edited.surface = [9, 8, 7];
        library.persist(&ctx, &edited, false);
        assert!(library.pending_preferences.is_some());
        library.persist(&ctx, &original, false);
        assert!(library.pending_preferences.is_none());
        assert!(library.preferences_saved());
    }
}
