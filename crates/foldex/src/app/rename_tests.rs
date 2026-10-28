//! Inline rename and new-item editors, driven through real frames, files
//! and jobs.
use super::tests::{key_event, pump_events, ready_fixture};
use super::*;
use crate::commands::Command;
use std::path::Path;

struct Run {
    app: Foldex,
    ctx: egui::Context,
    frame: usize,
}

impl Run {
    fn new() -> Self {
        let (mut app, ctx, frame) = ready_fixture();
        app.theme.reduced_motion = true;
        app.theme.apply(&ctx);
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(app.pane().id));
        let mut run = Self { app, ctx, frame };
        run.idle(2);
        run
    }
    fn root(&self) -> PathBuf {
        self.app.pane().path.clone()
    }
    fn send(&mut self, events: Vec<egui::Event>) {
        pump_events(&mut self.app, &self.ctx, self.frame, events);
        self.frame += 1;
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.send(vec![key_event(key, modifiers)]);
    }
    fn text(&mut self, text: &str) {
        self.send(vec![egui::Event::Text(text.into())]);
    }
    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.send(vec![]);
        }
    }
    fn command(&mut self, command: Command) {
        let ctx = self.ctx.clone();
        self.app.execute_command(&ctx, command);
    }
    /// Pumps frames, letting workers run, until `done` holds.
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.send(vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn editor_focused(&self) -> bool {
        self.app.pane().rename_has_focus(&self.ctx)
    }
    fn open_editor(&mut self) {
        self.key(Key::F2, Modifiers::NONE);
        self.until("the name editor", |run| run.editor_focused());
    }
    fn selection(&self) -> Option<(usize, usize)> {
        let state =
            egui::text_edit::TextEditState::load(&self.ctx, self.app.pane().rename_editor_id())?;
        let range = state.cursor.char_range()?;
        let (a, b) = (range.primary.index.0, range.secondary.index.0);
        Some((a.min(b), a.max(b)))
    }
    fn renaming(&self) -> Option<PathBuf> {
        self.app.pane().renaming_path().map(Path::to_path_buf)
    }
    fn selected_name(&self) -> Option<String> {
        let path = self.app.pane().primary_selected_path()?;
        Some(path.file_name()?.to_string_lossy().into_owned())
    }
    fn listed(&self, name: &str) -> bool {
        let pane = self.app.pane();
        !pane.loading
            && (0..pane.view_len()).any(|row| pane.path_at(row).is_some_and(|p| p.ends_with(name)))
    }
}

#[test]
fn f2_selects_the_stem_and_enter_renames_in_place() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    assert_eq!(run.app.pane().rename_text(), Some("Selected.txt"));
    assert_eq!(run.selection(), Some((0, "Selected".len())));
    run.text("Chosen");
    assert_eq!(run.app.pane().rename_text(), Some("Chosen.txt"));
    run.key(Key::Enter, Modifiers::NONE);
    assert!(run.app.rename_confirm.is_none());
    run.until("the rename", |run| {
        root.join("Chosen.txt").exists() && run.selected_name().as_deref() == Some("Chosen.txt")
    });
    assert!(!root.join("Selected.txt").exists());
    assert!(!run.app.renaming());
    assert!(crate::keyboard::listing_owns(&run.ctx, run.app.pane().id));
}

#[test]
fn escape_cancels_and_forbidden_characters_never_reach_the_name() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    run.text("a:b*c?");
    assert_eq!(run.app.pane().rename_text(), Some("abc.txt"));
    run.key(Key::Escape, Modifiers::NONE);
    assert!(!run.app.renaming());
    run.idle(3);
    assert!(root.join("Selected.txt").exists());
    assert!(!root.join("abc.txt").exists());
    assert!(crate::keyboard::listing_owns(&run.ctx, run.app.pane().id));
}

#[test]
fn changing_the_extension_asks_first_and_no_returns_to_the_typed_name() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    // As egui-winit reports Ctrl on Windows: both ctrl and command.
    run.key(Key::A, Modifiers::CTRL | Modifiers::COMMAND);
    assert_eq!(run.selection(), Some((0, "Selected.txt".len())));
    run.text("Notes.md");
    run.key(Key::Enter, Modifiers::NONE);
    run.until("the confirmation", |run| run.app.rename_confirm.is_some());
    assert!(!run.app.renaming());
    run.idle(2);
    run.key(Key::Escape, Modifiers::NONE);
    assert!(run.app.rename_confirm.is_none());
    run.until("the editor to reopen", |run| run.editor_focused());
    assert_eq!(run.app.pane().rename_text(), Some("Notes.md"));
    assert!(root.join("Selected.txt").exists());
    run.key(Key::Enter, Modifiers::NONE);
    run.until("the confirmation", |run| run.app.rename_confirm.is_some());
    run.idle(2);
    run.key(Key::Enter, Modifiers::NONE);
    run.until("the rename", |_| root.join("Notes.md").exists());
    assert!(!root.join("Selected.txt").exists());
}

#[test]
fn a_background_refresh_keeps_the_editor_and_its_text() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    run.text("Typed");
    std::fs::write(root.join("Arrived.txt"), b"x").unwrap();
    run.app.pane_mut().refresh();
    run.until("the refreshed listing", |run| run.listed("Arrived.txt"));
    run.idle(3);
    assert!(run.editor_focused());
    assert_eq!(run.app.pane().rename_text(), Some("Typed.txt"));
    assert_eq!(run.renaming(), Some(root.join("Selected.txt")));
}

#[test]
fn new_folder_is_created_numbered_and_opens_its_name_editor() {
    let mut run = Run::new();
    let root = run.root();
    run.command(Command::NewFolder);
    run.until("the first folder's editor", |run| {
        run.editor_focused() && run.renaming() == Some(root.join("New folder"))
    });
    assert!(root.join("New folder").is_dir());
    assert_eq!(run.app.pane().rename_text(), Some("New folder"));
    // Folders select the whole name.
    assert_eq!(run.selection(), Some((0, "New folder".len())));
    run.command(Command::NewFolder);
    run.until("the numbered folder's editor", |run| {
        run.editor_focused() && run.renaming() == Some(root.join("New folder (2)"))
    });
    run.text("Projects");
    run.key(Key::Enter, Modifiers::NONE);
    run.until("the rename", |_| root.join("Projects").is_dir());
    assert!(root.join("New folder").is_dir());
    assert!(!root.join("New folder (2)").exists());
}

#[test]
fn new_text_document_selects_its_stem() {
    let mut run = Run::new();
    let root = run.root();
    run.command(Command::NewFile);
    let path = root.join(crate::naming::DEFAULT_TEXT_FILE);
    run.until("the new file's editor", |run| {
        run.editor_focused() && run.renaming().as_ref() == Some(&path)
    });
    assert!(path.is_file());
    assert_eq!(run.selection(), Some((0, "New Text Document".len())));
}

#[test]
fn tab_commits_and_moves_the_editor_to_the_next_item() {
    let mut run = Run::new();
    let root = run.root();
    std::fs::write(root.join("Zeta.txt"), b"x").unwrap();
    run.app.pane_mut().refresh();
    run.until("the second file", |run| run.listed("Zeta.txt"));
    let first = run.app.pane().path_at(0).unwrap();
    let second = run.app.pane().path_at(1).unwrap();
    run.app.pane_mut().begin_rename(first.clone());
    run.until("the editor", |run| run.editor_focused());
    run.key(Key::Tab, Modifiers::NONE);
    run.until("the editor on the next item", |run| {
        run.editor_focused() && run.renaming().as_ref() == Some(&second)
    });
    assert!(first.exists());
    run.key(Key::Tab, Modifiers::SHIFT);
    run.until("the editor back on the first item", |run| {
        run.editor_focused() && run.renaming().as_ref() == Some(&first)
    });
}

#[test]
fn an_item_created_by_a_shell_template_opens_its_name_editor() {
    let mut run = Run::new();
    let root = run.root();
    run.app.pane_mut().watch_for_new_item(&root);
    let made = root.join("New Bitmap image.bmp");
    std::fs::write(&made, b"").unwrap();
    run.app.pane_mut().refresh();
    run.until("the template item's editor", |run| {
        run.editor_focused() && run.renaming().as_ref() == Some(&made)
    });
    assert_eq!(run.selection(), Some((0, "New Bitmap image".len())));
}

#[test]
fn a_reserved_name_keeps_the_editor_open_and_renames_nothing() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    run.key(Key::A, Modifiers::CTRL | Modifiers::COMMAND);
    run.text("CON");
    run.key(Key::Enter, Modifiers::NONE);
    run.idle(3);
    assert!(run.editor_focused());
    assert_eq!(run.app.pane().rename_text(), Some("CON"));
    assert!(root.join("Selected.txt").exists());
}

#[test]
fn refresh_commits_what_was_typed() {
    let mut run = Run::new();
    let root = run.root();
    run.open_editor();
    run.text("Fresh");
    run.command(Command::Refresh);
    assert!(!run.app.renaming());
    run.until("the rename", |_| root.join("Fresh.txt").exists());
}

#[test]
fn home_items_rename_in_place_and_history_follows_them() {
    let mut run = Run::new();
    let root = run.root();
    let path = root.join("Selected.txt");
    run.app
        .record_home_usage(path.clone(), crate::home_history::Access::Opened);
    run.app.navigate_home();
    run.until("Home's rows", |run| !run.app.home.rows.is_empty());
    run.app.pane_mut().begin_rename(path.clone());
    run.until("the Home editor", |run| run.editor_focused());
    assert_eq!(run.selection(), Some((0, "Selected".len())));
    run.text("Kept");
    run.key(Key::Enter, Modifiers::NONE);
    let renamed = root.join("Kept.txt");
    run.until("the rename", |run| {
        renamed.exists()
            && run
                .app
                .home_history
                .entries
                .iter()
                .any(|entry| crate::bookmarks::path_eq(&entry.path, &renamed))
    });
    assert!(!run.app.renaming());
    assert_eq!(run.app.pane().home_selected.as_ref(), Some(&renamed));
}

fn click_at(run: &mut Run, pos: egui::Pos2) {
    run.send(vec![egui::Event::PointerMoved(pos)]);
    for pressed in [true, false] {
        run.send(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }]);
    }
}

/// Finds the selected item's name on screen by opening the editor there.
fn name_point(run: &mut Run) -> egui::Pos2 {
    run.open_editor();
    let rect = run.app.pane().rename_rect().unwrap();
    // Escape in the editor's first focused frame still cancels it.
    run.key(Key::Escape, Modifiers::NONE);
    assert!(!run.app.renaming());
    assert!(
        run.selected_name().is_some(),
        "Escape must not reach the listing"
    );
    run.idle(2);
    egui::pos2(rect.left() + 14.0, rect.center().y)
}

#[test]
fn a_slow_second_click_on_the_selected_name_starts_renaming() {
    let mut run = Run::new();
    let point = name_point(&mut run);
    // Let any earlier click time expire, then click the already selected name.
    for _ in 0..60 {
        run.send(vec![]);
    }
    click_at(&mut run, point);
    assert!(
        !run.app.renaming(),
        "renaming waits out the double-click time"
    );
    run.until("the slow click's editor", |run| run.editor_focused());
    assert_eq!(run.selection(), Some((0, "Selected".len())));
}

#[test]
fn a_keypress_after_the_click_cancels_the_slow_click_rename() {
    let mut run = Run::new();
    let point = name_point(&mut run);
    for _ in 0..60 {
        run.send(vec![]);
    }
    click_at(&mut run, point);
    run.key(Key::Home, Modifiers::NONE);
    for _ in 0..90 {
        run.send(vec![]);
    }
    assert!(!run.app.renaming());
}
