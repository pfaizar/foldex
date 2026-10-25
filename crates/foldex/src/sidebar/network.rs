//! Opt-in connection discovery. Construction and service never enumerate a
//! network. Only an explicit expansion/refresh starts one cancellable request;
//! no timer, polling loop, share probe or credentials are owned by this service.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

#[cfg(windows)]
mod native;

pub const MAX_RESOURCES: usize = 64;
const MAX_NAME_UNITS: usize = 512;
const MAX_REPLY_BYTES: usize = 1024 * 1024;
const HELPER_ARGUMENT: &str = "--foldex-network-snapshot";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub path: PathBuf,
    pub label: String,
    pub connected: bool,
    local: Option<String>,
}
impl Resource {
    fn from_names(remote: &str, local: Option<&str>, connected: bool) -> Option<Self> {
        // WNet can return providers, printers or other virtual containers. Only
        // complete UNC shares become explorer paths; the Shell owns wider browsing.
        if remote.encode_utf16().count() > MAX_NAME_UNITS
            || remote.chars().any(char::is_control)
            || !remote.starts_with(r"\\")
            || remote.starts_with(r"\\?\")
            || remote.starts_with(r"\\.\")
        {
            return None;
        }
        let remote = remote.trim_end_matches('\\');
        let mut parts = remote[2..].split('\\');
        let server = parts.next()?;
        let share = parts.next()?;
        if server.is_empty()
            || share.is_empty()
            || parts
                .clone()
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || [server, share]
                .iter()
                .any(|part| matches!(*part, "." | ".."))
            || remote.contains('/')
        {
            return None;
        }
        // Only drive mappings get a local label. Never display provider-supplied
        // control text or interpret a local device name as a filesystem target.
        let local = local.filter(|name| {
            let bytes = name.as_bytes();
            bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        });
        let label = match local {
            Some(local) => format!("{share} on {server} ({local})"),
            None => format!("{share} on {server}"),
        };
        Some(Self {
            path: remote.into(),
            label,
            connected,
            local: local.map(str::to_owned),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    resources: Vec<Resource>,
    warning: Option<String>,
    truncated: bool,
}
impl Snapshot {
    fn insert(&mut self, resource: Resource) {
        if let Some(existing) = self.resources.iter_mut().find(|old| {
            old.path
                .to_string_lossy()
                .eq_ignore_ascii_case(&resource.path.to_string_lossy())
        }) {
            let connected = existing.connected || resource.connected;
            if (!existing.connected && resource.connected)
                || (existing.local.is_none() && resource.local.is_some())
            {
                *existing = resource;
            }
            existing.connected = connected;
        } else if self.resources.len() < MAX_RESOURCES {
            self.resources.push(resource);
        } else {
            self.truncated = true;
        }
    }
    fn normalize(self) -> Self {
        let mut normalized = Self {
            warning: self
                .warning
                .map(|message| message.chars().take(2048).collect()),
            truncated: self.truncated || self.resources.len() > MAX_RESOURCES,
            ..Default::default()
        };
        for resource in self.resources.into_iter().take(MAX_RESOURCES) {
            if let Some(resource) = Resource::from_names(
                &resource.path.to_string_lossy(),
                resource.local.as_deref(),
                resource.connected,
            ) {
                normalized.insert(resource);
            }
        }
        normalized
            .resources
            .sort_by_cached_key(|resource| (!resource.connected, resource.label.to_lowercase()));
        normalized
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAction {
    Connect,
    Browse,
    Manage,
    Refresh,
}
impl UserAction {
    pub const ALL: [Self; 4] = [Self::Connect, Self::Browse, Self::Manage, Self::Refresh];
    pub fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect network drive…",
            Self::Browse => "Browse Windows Network…",
            Self::Manage => "Manage mapped drives…",
            Self::Refresh => "Refresh connections",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
            Self::Connect => "Open the Windows Map Network Drive dialog. Windows handles sign-in.",
            Self::Browse => {
                "Browse computers and shares in Windows Network. Paste a share's UNC path in Foldex to open it here."
            }
            Self::Manage => {
                "Open This PC in Windows to manage or disconnect mapped network drives. Refresh this list after making changes."
            }
            Self::Refresh => {
                "Reload connected and remembered shares. No network-wide scan is performed."
            }
        }
    }
}

struct Cancellation {
    cancelled: AtomicBool,
    #[cfg(windows)]
    event: usize,
}
impl Cancellation {
    fn new() -> io::Result<Self> {
        Ok(Self {
            cancelled: AtomicBool::new(false),
            #[cfg(windows)]
            event: native::cancel_event()?,
        })
    }
    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            #[cfg(windows)]
            native::signal_cancel(self.event);
        }
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}
#[cfg(windows)]
impl Drop for Cancellation {
    fn drop(&mut self) {
        native::close_cancel(self.event);
    }
}
struct Flight {
    cancel: Arc<Cancellation>,
    result: Receiver<Result<Snapshot, String>>,
}

pub struct Data {
    wake: Arc<dyn Fn() + Send + Sync>,
    owner: isize,
    flight: Option<Flight>,
    action: Option<Receiver<Result<bool, String>>>,
    requested: bool,
    loaded: bool,
    snapshot: Snapshot,
    error: Option<String>,
    revision: u64,
}
impl Data {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            wake,
            owner: 0,
            flight: None,
            action: None,
            requested: false,
            loaded: false,
            snapshot: Snapshot::default(),
            error: None,
            revision: 0,
        }
    }
    pub fn set_owner_window(&mut self, owner: isize) {
        self.owner = owner;
    }
    pub fn resources(&self) -> &[Resource] {
        &self.snapshot.resources
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn loading(&self) -> bool {
        self.flight
            .as_ref()
            .is_some_and(|flight| !flight.cancel.is_cancelled())
            || self.requested
    }
    pub fn action_enabled(&self, action: UserAction) -> bool {
        if action == UserAction::Refresh {
            !self.loading()
        } else {
            self.action.is_none()
        }
    }
    pub fn status(&self) -> &'static str {
        if self.action.is_some() {
            "Opening Windows network controls…"
        } else if self.loading() {
            "Loading connections…"
        } else if self.error.is_some() {
            "Network unavailable · Retry below"
        } else if self.snapshot.warning.is_some() {
            "Some connections unavailable · Retry below"
        } else if !self.loaded {
            "Refresh to load network connections"
        } else if self.snapshot.resources.is_empty() {
            "No connected or saved shares"
        } else if self.snapshot.truncated {
            "Showing the first 64 connections"
        } else {
            "Connected and saved network shares"
        }
    }
    pub fn detail(&self) -> &str {
        self.error.as_deref().or(self.snapshot.warning.as_deref()).unwrap_or(
            "Saved shares may be offline. Open a share to browse in Foldex, or use Windows Network to find other computers. You can also enter \\\\server\\share in the address bar.",
        )
    }
    pub fn has_error(&self) -> bool {
        self.error.is_some() || self.snapshot.warning.is_some()
    }
    /// Called by a user expansion, never by an idle frame or persisted state.
    pub fn expanded(&mut self) {
        if !self.loaded && !self.loading() {
            self.perform(UserAction::Refresh);
        }
    }
    pub fn perform(&mut self, action: UserAction) {
        if !self.action_enabled(action) {
            return;
        }
        self.error = None;
        if action == UserAction::Refresh {
            self.requested = true;
            self.start_requested();
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let wake = self.wake.clone();
        let owner = self.owner;
        match thread::Builder::new()
            .name("foldex-network-dialog".into())
            .spawn(move || {
                #[cfg(windows)]
                let result = native::action(action, owner);
                #[cfg(not(windows))]
                let result = {
                    let _ = (action, owner);
                    Err("Windows network controls are unavailable on this platform".into())
                };
                if sender.send(result).is_ok() {
                    wake();
                }
            }) {
            Ok(_) => self.action = Some(receiver),
            Err(error) => {
                self.error = Some(format!("Could not open Windows network controls: {error}"))
            }
        }
    }
    fn start_requested(&mut self) {
        if !self.requested || self.flight.is_some() {
            return;
        }
        self.requested = false;
        if let Err(error) = self.start(discover) {
            self.error = Some(error);
        }
    }
    fn start(
        &mut self,
        discover: impl FnOnce(&Cancellation) -> Result<Snapshot, String> + Send + 'static,
    ) -> Result<(), String> {
        let cancel = Arc::new(Cancellation::new().map_err(|error| error.to_string())?);
        let worker_cancel = cancel.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let wake = self.wake.clone();
        thread::Builder::new()
            .name("foldex-network-snapshot".into())
            .spawn(move || {
                let snapshot = discover(&worker_cancel);
                if sender.send(snapshot).is_ok() {
                    // Even cancellation needs one completion wake, so a user refresh
                    // queued while the old helper exits can start without polling.
                    wake();
                }
            })
            .map_err(|error| format!("Could not load network connections: {error}"))?;
        self.flight = Some(Flight { cancel, result });
        Ok(())
    }
    /// Nonblocking mailbox drain only. Hiding/collapsing cancels the helper and
    /// discards its late publication. There is no automatic retry or idle wake.
    pub fn service(&mut self, active: bool) {
        if !active {
            self.requested = false;
            if let Some(flight) = &self.flight {
                flight.cancel.cancel();
            }
        }
        if let Some(flight) = &self.flight {
            let outcome = match flight.result.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err(
                    "Network helper stopped. Retry to reload connections.".into(),
                )),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(outcome) = outcome {
                let cancelled = flight.cancel.is_cancelled();
                self.flight = None;
                if !cancelled {
                    match outcome {
                        Ok(snapshot) => {
                            let snapshot = snapshot.normalize();
                            if snapshot != self.snapshot {
                                self.snapshot = snapshot;
                                self.revision = self.revision.wrapping_add(1);
                            }
                            self.loaded = true;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
        }
        if let Some(receiver) = &self.action {
            let outcome = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Windows network controls closed unexpectedly".into()))
                }
                Err(TryRecvError::Empty) => None,
            };
            if let Some(outcome) = outcome {
                self.action = None;
                match outcome {
                    Ok(refresh) => self.requested |= refresh && active,
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if active {
            self.start_requested();
        }
    }
    /// Deterministic native-capture hook. It starts no worker, timers or I/O.
    /// Live requests are cancelled and cached rows are replaced by offline data.
    pub(crate) fn benchmark_snapshot(
        &mut self,
        resources: &[(&str, Option<&str>, bool)],
        error: Option<&str>,
    ) {
        if let Some(flight) = &self.flight {
            flight.cancel.cancel();
        }
        self.requested = false;
        self.loaded = true;
        self.snapshot = Snapshot {
            resources: resources
                .iter()
                .filter_map(|(remote, local, connected)| {
                    Resource::from_names(remote, *local, *connected)
                })
                .collect(),
            ..Default::default()
        }
        .normalize();
        self.error = error.map(str::to_owned);
        self.revision = self.revision.wrapping_add(1);
    }
}
impl Drop for Data {
    fn drop(&mut self) {
        if let Some(flight) = &self.flight {
            flight.cancel.cancel();
        }
    }
}

fn discover(cancel: &Cancellation) -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        native::discover(cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = cancel;
        Err("Windows network connections are unavailable on this platform".into())
    }
}

/// Must run before launcher instance activation, config reads or UI startup.
/// The exact private argument is emitted only by the explicit discovery worker.
pub fn run_helper_if_requested(args: &[OsString]) -> bool {
    if args.len() != 1 || args[0] != HELPER_ARGUMENT {
        return false;
    }
    #[cfg(windows)]
    native::helper_main();
    true
}

fn decode_reply(bytes: &[u8]) -> Result<Snapshot, String> {
    if bytes.len() > MAX_REPLY_BYTES {
        return Err(
            "Network provider returned too much data. Use Windows Network to browse.".into(),
        );
    }
    // Each complete line is one bounded snapshot, so timeout/cancellation can
    // retain the connected list even if remembered-resource enumeration stalls.
    bytes
        .split(|byte| *byte == b'\n')
        .rev()
        .filter(|line| !line.is_empty())
        .find_map(|line| serde_json::from_slice::<Result<Snapshot, String>>(line).ok())
        .unwrap_or_else(|| {
            Err("No network connections were returned. Retry or browse Windows Network.".into())
        })
}

#[cfg(test)]
mod tests;
