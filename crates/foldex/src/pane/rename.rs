//! Inline rename: one editor per pane, drawn over the item's own name the way
//! File Explorer does it. The editor follows its item by path, so watcher and
//! job refreshes that reorder the list don't close it; only the item vanishing
//! does. Enter, Tab and losing focus commit; Escape cancels.
use super::*;
use foldex_ui::inline_edit::{InlineEditState, InlineLayout, InlineOutcome};
use std::ffi::OsString;

/// How long a new name is shown before the refreshed listing confirms it.
const OPTIMISTIC_LIMIT: Duration = Duration::from_secs(15);
/// How long to wait for a shell New template's item to be listed.
const NEW_ITEM_LIMIT: Duration = Duration::from_secs(10);

/// The folder's names before a shell New template ran. The one name that
/// appears afterwards is the template's item.
pub(crate) struct NewItemWatch {
    directory: PathBuf,
    names: std::collections::HashSet<OsString>,
    since: Instant,
}

pub(crate) struct InlineRename {
    pub(super) target: PathBuf,
    pub(super) is_dir: bool,
    /// The name as displayed when editing began.
    pub(super) original: String,
    pub(super) text: String,
    pub(super) editor: InlineEditState,
    /// The view row last known to hold `target`, and the view it was found in.
    pub(super) row: Option<(usize, u64)>,
    /// Message and the time it was shown, drawn under the editor.
    balloon: Option<(String, f64)>,
    pub(super) drawn_frame: Option<u64>,
}

/// What the app should do after a commit attempt.
pub(crate) enum CommitOutcome {
    /// The editor closed; nothing changed on disk.
    Closed,
    /// The editor stays open, explaining why the name can't be used.
    Kept,
    Rename(Action),
}

impl InlineRename {
    fn new(target: PathBuf, is_dir: bool, text: Option<String>) -> Self {
        let original = target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let text = text.unwrap_or_else(|| original.clone());
        let select = if text == original {
            crate::naming::initial_selection(&text, is_dir)
        } else {
            (0, text.chars().count())
        };
        Self {
            target,
            is_dir,
            original,
            text,
            editor: InlineEditState::opening(select),
            row: None,
            balloon: None,
            drawn_frame: None,
        }
    }

    pub(crate) fn target(&self) -> &Path {
        &self.target
    }
}

pub(crate) fn editor_id(pane: u64) -> Id {
    Id::new((pane, "inline-rename"))
}

impl Pane {
    pub fn rename_editor_id(&self) -> Id {
        editor_id(self.id)
    }

    pub fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }

    pub fn renaming_path(&self) -> Option<&Path> {
        self.rename.as_ref().map(InlineRename::target)
    }

    /// The text in the open name editor.
    pub fn rename_text(&self) -> Option<&str> {
        self.rename.as_ref().map(|rename| rename.text.as_str())
    }

    /// Where the name editor was last drawn.
    #[cfg(test)]
    pub(crate) fn rename_rect(&self) -> Option<Rect> {
        self.rename_rect
    }

    /// The editor was just drawn and is still waiting for keyboard focus.
    /// Keys pressed meanwhile are meant for it, not for the listing.
    pub(crate) fn rename_opening(&self, ctx: &egui::Context) -> bool {
        let frame = ctx.cumulative_frame_nr();
        self.rename.as_ref().is_some_and(|rename| {
            rename.editor.focus
                && !rename.editor.had_focus
                && rename.drawn_frame.is_some_and(|drawn| drawn + 1 >= frame)
        })
    }

    pub(crate) fn rename_has_focus(&self, ctx: &egui::Context) -> bool {
        self.rename.is_some() && ctx.memory(|memory| memory.has_focus(self.rename_editor_id()))
    }

    /// Opens the editor on `path`. In a folder view the item becomes the only
    /// selection and scrolls into view; when it isn't listed yet, the editor
    /// opens as soon as it arrives.
    pub fn begin_rename(&mut self, path: PathBuf) {
        self.begin_rename_with(path, None);
    }

    /// Reopens the editor with text the user already typed, after they decline
    /// a confirmation.
    pub fn resume_rename(&mut self, path: PathBuf, text: String) {
        self.begin_rename_with(path, Some(text));
    }

    fn begin_rename_with(&mut self, path: PathBuf, text: Option<String>) {
        if self.is_recycle_bin() {
            return;
        }
        self.slow_click = None;
        if self.is_home() {
            let is_dir = path.is_dir();
            self.home_selected = Some(path.clone());
            self.rename = Some(InlineRename::new(path, is_dir, text));
            return;
        }
        match self.find_row(&path) {
            Some(row) => {
                let is_dir = self.entry_at(row).is_some_and(|entry| entry.is_dir);
                self.keyboard.cancel_range();
                self.select_all_pending = false;
                self.invert_pending = false;
                self.selected.clear();
                self.selected.insert(row as u32);
                self.anchor = Some(row);
                self.focused = Some(row);
                self.keyboard.reveal = Some(row);
                self.last_selection = Some(path.clone());
                self.sync_selection();
                let mut rename = InlineRename::new(path, is_dir, text);
                rename.row = Some((row, self.render_epoch));
                self.rename = Some(rename);
            }
            None if !self.recursive => {
                self.pending_selection = path.file_name().map(OsString::from);
                self.pending_rename = Some((path, text));
                self.refilter();
            }
            None => {}
        }
    }

    /// The row holding `path` in the current view, if it is loaded.
    fn find_row(&self, path: &Path) -> Option<usize> {
        let snapshot = self.snapshot.as_ref()?;
        if self.recursive {
            let entries = snapshot.entries.len();
            return (0..entries)
                .find(|&index| snapshot.entries[index].path(snapshot) == path)
                .map(|index| self.window_start + index);
        }
        if path.parent() != Some(snapshot.path.as_path()) {
            return None;
        }
        let name = path.file_name()?;
        self.indices.iter().position(|&index| {
            snapshot
                .entries
                .get(index as usize)
                .is_some_and(|entry| entry.raw_name == name)
        })
    }

    /// Re-resolves the edited item's row after the list changed underneath it.
    pub(crate) fn rename_row(&mut self) -> Option<usize> {
        let rename = self.rename.as_ref()?;
        if let Some((row, epoch)) = rename.row
            && epoch == self.render_epoch
        {
            return Some(row);
        }
        let target = rename.target.clone();
        let cached = rename.row.map(|(row, _)| row);
        let row = cached
            .filter(|&row| {
                self.snapshot
                    .as_ref()
                    .zip(self.entry_at(row))
                    .is_some_and(|(snapshot, entry)| entry.path(snapshot) == target)
            })
            .or_else(|| self.find_row(&target));
        if let Some(rename) = &mut self.rename {
            rename.row = row.map(|row| (row, self.render_epoch));
        }
        row
    }

    /// True once the listing is settled and the edited item is no longer in it.
    pub(super) fn rename_target_gone(&self) -> bool {
        !self.loading
            && !self.scanning
            && self.displayed_view_is_current()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.complete)
    }

    pub fn cancel_rename(&mut self, ctx: &egui::Context) {
        if let Some(rename) = self.rename.take() {
            self.return_focus_from_rename(ctx, &rename);
        }
    }

    /// Closes the editor without touching focus, when the user has moved on.
    pub(crate) fn drop_rename(&mut self) {
        self.rename = None;
        self.pending_rename = None;
    }

    fn return_focus_from_rename(&self, ctx: &egui::Context, rename: &InlineRename) {
        let id = editor_id(self.id);
        if rename.drawn_frame.is_some()
            && ctx.memory(|memory| memory.has_focus(id) || memory.focused().is_none())
        {
            crate::keyboard::request_id(ctx, self.listing_focus_id());
        }
    }

    /// Applies the editor's text. Trailing spaces and dots are dropped; an
    /// empty or unchanged name closes the editor quietly; a name Windows can't
    /// use keeps it open with an explanation. `confirmed` skips the
    /// extension-change question, which the app asks before renaming.
    pub fn commit_rename(&mut self, ctx: &egui::Context) -> CommitOutcome {
        let Some(rename) = self.rename.as_mut() else {
            return CommitOutcome::Closed;
        };
        match crate::naming::prepare_commit(&rename.original, &rename.text) {
            Ok(crate::naming::Commit::Unchanged) => {
                self.cancel_rename(ctx);
                CommitOutcome::Closed
            }
            Err(problem) => {
                let now = ctx.input(|input| input.time);
                rename.balloon = Some((problem.message(), now));
                rename.editor.focus = true;
                CommitOutcome::Kept
            }
            Ok(crate::naming::Commit::Rename(name)) => {
                let rename = self.rename.take().expect("checked above");
                self.return_focus_from_rename(ctx, &rename);
                CommitOutcome::Rename(self.rename_action(rename, name))
            }
        }
    }

    fn rename_action(&mut self, rename: InlineRename, name: String) -> Action {
        if crate::naming::extension_changed(&rename.original, &name, rename.is_dir) {
            Action::ConfirmRenameExtension {
                origin: self.id,
                path: rename.target,
                name,
            }
        } else {
            self.show_renamed(rename.target.clone(), name.clone());
            Action::RenameInline {
                origin: self.id,
                path: rename.target,
                name,
            }
        }
    }

    /// Commits from outside the listing (navigation, refresh, closing). The
    /// resulting action reaches the app through `drain_action`. An unusable
    /// name is dropped: the edit can't continue once the item is left behind.
    pub(crate) fn flush_rename(&mut self) {
        self.pending_rename = None;
        let Some(rename) = self.rename.take() else {
            return;
        };
        if let Ok(crate::naming::Commit::Rename(name)) =
            crate::naming::prepare_commit(&rename.original, &rename.text)
        {
            let action = self.rename_action(rename, name);
            self.queued_actions.push(action);
        }
    }

    /// Shows `name` on the renamed item until the refreshed listing has it.
    pub(crate) fn show_renamed(&mut self, from: PathBuf, name: String) {
        self.renamed.retain(|(path, _, _)| *path != from);
        if self.renamed.len() >= 64 {
            self.renamed.remove(0);
        }
        self.renamed.push((from, name, Instant::now()));
        self.name_cache.clear();
    }

    /// The rename job finished. On success the new item is selected once the
    /// refreshed listing contains it.
    pub(crate) fn finish_rename(&mut self, from: &Path, to: Option<&Path>) {
        let Some(to) = to else {
            self.renamed.retain(|(path, _, _)| path != from);
            self.name_cache.clear();
            return;
        };
        if self.recursive {
            // Search results come from the index, which only learns the new
            // name on its rescan; keep showing it until that row is replaced.
            let name = to.file_name().unwrap_or_default().to_string_lossy();
            self.show_renamed(from.to_owned(), name.into_owned());
            return;
        }
        self.renamed.retain(|(path, _, _)| path != from);
        self.name_cache.clear();
        if self.is_home() {
            if self.home_selected.as_deref() == Some(from) {
                self.home_selected = Some(to.to_owned());
            }
            return;
        }
        if to.parent() == Some(self.path.as_path()) {
            self.pending_selection = to.file_name().map(OsString::from);
        }
    }

    /// A new item was created for this pane: select it and open its editor.
    pub(crate) fn created_for_rename(&mut self, path: PathBuf) {
        if self.is_home() || self.recursive || path.parent() != Some(self.path.as_path()) {
            return;
        }
        self.pending_selection = path.file_name().map(OsString::from);
        self.pending_rename = Some((path, None));
    }

    /// Called after a view publishes: opens a waiting editor, drops confirmed
    /// optimistic names.
    /// A shell New template is about to create an item in `directory`. Opens
    /// its name editor once it is listed, like File Explorer does.
    pub fn watch_for_new_item(&mut self, directory: &Path) {
        self.new_item_watch = None;
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        if self.recursive || !crate::bookmarks::path_eq(&snapshot.path, directory) {
            return;
        }
        let names = (0..snapshot.entries.len())
            .map(|index| snapshot.entries[index].raw_name.clone())
            .collect();
        self.new_item_watch = Some(NewItemWatch {
            directory: snapshot.path.clone(),
            names,
            since: Instant::now(),
        });
    }

    fn take_new_item(&mut self) -> Option<PathBuf> {
        let watch = self.new_item_watch.as_ref()?;
        let snapshot = self.snapshot.as_ref()?;
        if watch.since.elapsed() > NEW_ITEM_LIMIT || snapshot.path != watch.directory {
            self.new_item_watch = None;
            return None;
        }
        let mut added = (0..snapshot.entries.len())
            .map(|index| &snapshot.entries[index].raw_name)
            .filter(|name| !watch.names.contains(*name));
        let first = added.next()?.clone();
        let path = watch.directory.join(&first);
        let single = added.next().is_none();
        self.new_item_watch = None;
        // Something else also arrived; it isn't clear which item is new.
        single.then_some(path)
    }

    pub(super) fn after_view_published(&mut self, focused: Option<usize>) {
        if self.new_item_watch.is_some()
            && self.rename.is_none()
            && let Some(path) = self.take_new_item()
        {
            self.begin_rename(path);
        }
        if let Some((path, text)) = self.pending_rename.take() {
            match focused.filter(|&row| {
                self.snapshot
                    .as_ref()
                    .zip(self.entry_at(row))
                    .is_some_and(|(snapshot, entry)| entry.path(snapshot) == path)
            }) {
                Some(_) => self.begin_rename_with(path, text),
                None if self.rename_target_gone() => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    self.error = Some(format!("Created “{name}”, but the current filter hides it"));
                }
                None => self.pending_rename = Some((path, text)),
            }
        }
        self.prune_renamed();
    }

    pub(super) fn prune_renamed(&mut self) {
        if self.renamed.is_empty() {
            return;
        }
        let settled = self.rename_target_gone();
        let mut kept = std::mem::take(&mut self.renamed);
        kept.retain(|(path, _, at)| {
            at.elapsed() < OPTIMISTIC_LIMIT && (!settled || self.find_row(path).is_some())
        });
        self.renamed = kept;
        self.name_cache.clear();
    }

    /// The name to draw for `path` while its rename is in flight.
    pub(crate) fn renamed_label(&self, path: impl FnOnce() -> PathBuf) -> Option<&str> {
        if self.renamed.is_empty() {
            return None;
        }
        let path = path();
        self.renamed
            .iter()
            .rev()
            .find(|(from, _, _)| *from == path)
            .map(|(_, name, _)| name.as_str())
    }

    /// Draws the editor over `anchor`, the renamed row's name, when that row
    /// was painted this frame. Otherwise follows the item: reveals it when it
    /// is listed off screen, commits when the user scrolled it away, and
    /// cancels when it is gone.
    pub(crate) fn place_rename_editor(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        anchor: Option<(Rect, InlineLayout)>,
        rename_row: Option<usize>,
        actions: &mut Vec<Action>,
    ) {
        if let Some((anchor, layout)) = anchor {
            let view_len = self.view_len();
            self.rename_editor_ui(
                ui,
                t,
                anchor,
                layout,
                |pane, backwards| {
                    let (row, _) = pane.rename.as_ref()?.row?;
                    let next = if backwards {
                        row.checked_sub(1)?
                    } else {
                        row + 1
                    };
                    (next < view_len).then(|| pane.path_at(next)).flatten()
                },
                actions,
            );
            return;
        }
        if self.rename.is_none() {
            return;
        }
        match rename_row {
            // Still listed but off screen: open there first, and end the
            // edit if the user scrolls it away afterwards.
            Some(row) => {
                if self
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.editor.had_focus)
                {
                    if let CommitOutcome::Rename(action) = self.commit_rename(ui.ctx()) {
                        actions.push(action);
                    }
                } else {
                    self.keyboard.reveal = Some(row);
                    ui.ctx().request_repaint();
                }
            }
            // Deleted or renamed by someone else: Explorer cancels.
            None if self.rename_target_gone() => self.rename = None,
            None => {}
        }
    }

    /// Home's version of `place_rename_editor`. `order` lists the drawn items
    /// of the kind being edited, for Tab; `listed` says whether the item is
    /// still in Home's history at all.
    pub(crate) fn place_home_rename_editor(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        anchor: Option<(Rect, InlineLayout)>,
        order: Vec<PathBuf>,
        listed: bool,
        actions: &mut Vec<Action>,
    ) {
        if let Some((anchor, layout)) = anchor {
            self.rename_editor_ui(
                ui,
                t,
                anchor,
                layout,
                |pane, backwards| {
                    let target = pane.renaming_path()?;
                    let at = order
                        .iter()
                        .position(|path| crate::bookmarks::path_eq(path, target))?;
                    let next = if backwards {
                        at.checked_sub(1)?
                    } else {
                        at + 1
                    };
                    order.get(next).cloned()
                },
                actions,
            );
        } else if !listed {
            self.rename = None;
        } else if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.editor.had_focus)
        {
            // Scrolled out of view after editing began.
            if let CommitOutcome::Rename(action) = self.commit_rename(ui.ctx()) {
                actions.push(action);
            }
        }
    }

    /// Draws the editor over `anchor` and acts on what the user did. Returns
    /// an action for the app, if any. `next` is the item Tab moves to.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rename_editor_ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        anchor: Rect,
        layout: InlineLayout,
        next: impl Fn(&Self, bool) -> Option<PathBuf>,
        actions: &mut Vec<Action>,
    ) {
        let ctx = ui.ctx().clone();
        let id = self.rename_editor_id();
        let composing = crate::keyboard::composing(&ctx);
        let frame = ctx.cumulative_frame_nr();
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        if rename.drawn_frame.is_some_and(|drawn| drawn + 1 < frame) {
            // Hidden for a while (scrolled, reloading): focus was dropped, so
            // ask for it again instead of treating its loss as a commit.
            rename.editor.had_focus = false;
            rename.editor.focus = true;
        }
        rename.drawn_frame = Some(frame);
        let out = foldex_ui::inline_edit::inline_name_editor(
            ui,
            anchor,
            id,
            t,
            &mut rename.text,
            layout,
            &mut rename.editor,
            composing,
            &crate::naming::forbidden,
            crate::naming::MAX_NAME_UNITS,
        );
        foldex_ui::components::label_control(&out.response, "Rename");
        let now = ctx.input(|input| input.time);
        if out.rejected {
            rename.balloon = Some((crate::naming::FORBIDDEN_MESSAGE.into(), now));
        } else if out.response.changed() {
            rename.balloon = None;
        }
        if let Some((message, shown)) = &rename.balloon {
            if now - shown < 5.0 {
                foldex_ui::inline_edit::name_balloon(&ctx, out.rect, id, t, message);
                ctx.request_repaint_after(Duration::from_millis(250));
            } else {
                rename.balloon = None;
            }
        }
        self.rename_rect = Some(out.rect);
        let window_left = rename.editor.had_focus && !ctx.input(|input| input.focused);
        let outcome = if window_left {
            InlineOutcome::FocusLost
        } else {
            out.outcome
        };
        match outcome {
            InlineOutcome::Editing => {}
            InlineOutcome::Cancel => self.cancel_rename(&ctx),
            InlineOutcome::Commit | InlineOutcome::FocusLost => {
                if let CommitOutcome::Rename(action) = self.commit_rename(&ctx) {
                    actions.push(action);
                } else if outcome == InlineOutcome::FocusLost && self.rename.is_some() {
                    // Focus moved elsewhere with an unusable name: revert it.
                    self.rename = None;
                }
            }
            InlineOutcome::Move { backwards } => {
                let next = next(self, backwards);
                match self.commit_rename(&ctx) {
                    CommitOutcome::Kept => {}
                    CommitOutcome::Closed => {
                        if let Some(next) = next {
                            self.begin_rename(next);
                        }
                    }
                    CommitOutcome::Rename(action) => {
                        let confirm = matches!(action, Action::ConfirmRenameExtension { .. });
                        actions.push(action);
                        if !confirm && let Some(next) = next {
                            self.begin_rename(next);
                        }
                    }
                }
            }
        }
    }
}
