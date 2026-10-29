use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
}

impl Foldex {
    /// File Explorer's warning before a rename changes a file's extension.
    /// No reopens the name editor with the typed text, like Explorer does.
    pub(super) fn rename_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.rename_confirm else {
            return;
        };
        let origin = confirm.origin;
        if self.pane_by_id_mut(origin).is_none() {
            self.rename_confirm = None;
            return;
        }
        let composing = crate::keyboard::composing(ctx);
        let mut answer = None;
        if !composing {
            if crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape) {
                answer = Some(Answer::No);
            } else if crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter) {
                answer = Some(Answer::Yes);
            }
        }
        let t = &self.theme;
        let id = Id::new("rename-extension-confirm");
        let mut open = true;
        use foldex_ui::dialog::{Action, Dialog, Footer, Kind, Size, Text};
        let shown = Dialog::new(id, "Change the file extension?", Size::Small.width(t))
            .footer(Footer::new(vec![
                Action::new(id.with("no"), "Keep editing", Kind::Secondary),
                Action::new(id.with("yes"), "Change extension", Kind::Primary),
            ]))
            .show(ctx, t, &mut open, |ui| {
                foldex_ui::dialog::text(
                    ui,
                    t,
                    Text::Secondary,
                    "Apps may no longer recognize the file, and it might become unusable.",
                );
            });
        match shown.clicked {
            Some(0) => answer = Some(Answer::No),
            Some(_) => answer = Some(Answer::Yes),
            None => {}
        }
        if !open && answer.is_none() {
            answer = Some(Answer::No);
        }
        let Some(answer) = answer else {
            return;
        };
        foldex_ui::components::close_dialog(ctx, id);
        let RenameConfirm { origin, path, name } = self.rename_confirm.take().unwrap();
        match answer {
            Answer::Yes => {
                if let Some(pane) = self.pane_by_id_mut(origin) {
                    pane.show_renamed(path.clone(), name.clone());
                }
                self.job(Job::Rename { path, name, origin });
            }
            Answer::No => {
                if let Some(pane) = self.pane_by_id_mut(origin) {
                    pane.resume_rename(path, name);
                }
            }
        }
    }
}
