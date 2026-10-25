//! Path-specific menus and retained breadcrumb preferences. No filesystem work.
use super::*;
use foldex_ui::components::{MenuItem, MenuSection, MenuState};

#[derive(Default)]
pub(super) struct BreadcrumbMenu {
    pub menu: MenuState,
    target: Option<(PathBuf, Id, Rect)>,
}
impl BreadcrumbMenu {
    pub fn active_trigger(&self) -> Option<Id> {
        self.menu
            .active()
            .then(|| self.target.as_ref().map(|(_, trigger, _)| *trigger))
            .flatten()
    }
}

pub(super) fn make_breadcrumbs(
    pane: u64,
    path: &Path,
    compact: bool,
) -> Vec<foldex_ui::breadcrumb::Item> {
    let mut paths: Vec<_> = path.ancestors().take(48).collect();
    if compact {
        // Windows exposes these roots through the environment. Cache their
        // lexical identities; navigating or painting never queries the disk.
        static ROOTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let roots = ROOTS.get_or_init(|| {
            [
                "USERPROFILE",
                "PUBLIC",
                "OneDrive",
                "OneDriveConsumer",
                "OneDriveCommercial",
            ]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|path| path.to_string_lossy().replace('/', "\\"))
            .collect()
        });
        if let Some(first) = paths.iter().rposition(|path| {
            let path = friendly_path(path).replace('/', "\\");
            roots.iter().any(|root| root.eq_ignore_ascii_case(&path))
        }) {
            paths.truncate(first + 1);
        }
    }
    paths
        .into_iter()
        .rev()
        .map(|path| foldex_ui::breadcrumb::Item {
            id: Id::new((pane, "breadcrumb", path)),
            label: path_label(path),
        })
        .collect()
}

impl Pane {
    pub fn set_breadcrumb_options(&mut self, compact: bool, history: bool) {
        if (compact, history) == (self.compact_paths, self.breadcrumb_history) {
            return;
        }
        self.compact_paths = compact;
        self.breadcrumb_history = history;
        if !history {
            self.breadcrumb_path = self.path.clone();
        }
        self.breadcrumbs = make_breadcrumbs(self.id, &self.breadcrumb_path, compact);
        let distance = self
            .breadcrumb_path
            .ancestors()
            .position(|path| path == self.path.as_path())
            .unwrap_or(usize::MAX);
        if distance >= self.breadcrumbs.len() {
            self.breadcrumb_path = self.path.clone();
            self.breadcrumbs = make_breadcrumbs(self.id, &self.breadcrumb_path, compact);
            self.breadcrumb_current = self.breadcrumbs.len().saturating_sub(1);
        } else {
            self.breadcrumb_current = self.breadcrumbs.len() - 1 - distance;
        }
        self.breadcrumb_active = None;
        self.breadcrumb_anchor = None;
        self.breadcrumb_hidden = 0;
    }

    pub(super) fn breadcrumb_context_ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        request: Option<(usize, Rect, Id)>,
        anchor: Option<Rect>,
        actions: &mut Vec<Action>,
    ) {
        let mut request = request;
        // The shared bar owns geometry and its label/chevron tab stops. Tag
        // exactly those controls that were painted in this pass for F6 routing.
        let focused = ui.memory(|memory| memory.focused());
        for (index, item) in self.breadcrumbs.iter().enumerate() {
            if index < self.breadcrumb_hidden {
                continue;
            }
            let ids = [
                Some(item.id.with("label")),
                (index + 1 < self.breadcrumbs.len()).then(|| item.id.with("children")),
            ];
            for id in ids.into_iter().flatten() {
                if let Some(response) = ui.ctx().read_response(id)
                    && response.rect.is_positive()
                    && response.rect.intersects(ui.clip_rect())
                {
                    crate::keyboard::register(
                        ui,
                        crate::keyboard::Region::Toolbar(self.id),
                        id,
                        response.rect,
                    );
                    if request.is_none()
                        && focused == Some(id)
                        && Self::consume_navigation_key(ui.ctx(), egui::Modifiers::SHIFT, Key::F10)
                    {
                        request = Some((index, response.rect, item.id));
                    }
                }
            }
        }
        let hidden = Id::new((self.id, "breadcrumbs")).with("ancestors");
        for id in [hidden, hidden.with("arrow")]
            .into_iter()
            .filter(|_| self.breadcrumb_hidden > 0)
        {
            if let Some(response) = ui.ctx().read_response(id) {
                crate::keyboard::register(
                    ui,
                    crate::keyboard::Region::Toolbar(self.id),
                    id,
                    response.rect,
                );
                if focused == Some(id)
                    && Self::consume_navigation_key(ui.ctx(), egui::Modifiers::SHIFT, Key::F10)
                {
                    let count = self.breadcrumbs.len();
                    let paths = (0..self.breadcrumb_hidden)
                        .filter_map(|index| {
                            self.breadcrumb_path
                                .ancestors()
                                .nth(count - 1 - index)
                                .map(Path::to_path_buf)
                        })
                        .collect();
                    actions.push(Action::BreadcrumbAncestors {
                        paths,
                        anchor: response.rect,
                        trigger: hidden,
                    });
                }
            }
        }
        if let Some((index, anchor, trigger)) = request
            && let Some(path) = self
                .breadcrumb_path
                .ancestors()
                .nth(self.breadcrumbs.len() - 1 - index)
        {
            self.more_menu.close();
            self.zoom_open = false;
            self.breadcrumb_menu.target = Some((path.to_path_buf(), trigger, anchor));
            self.breadcrumb_menu.menu.open = false;
            self.breadcrumb_menu.menu.toggle(ui.ctx());
        } else if self.breadcrumb_menu.menu.active() {
            if let Some(anchor) = anchor {
                if let Some((_, _, previous)) = &mut self.breadcrumb_menu.target {
                    *previous = anchor;
                }
            } else {
                self.breadcrumb_menu.menu.close();
            }
        }
        let Some((_, _, anchor)) = &self.breadcrumb_menu.target else {
            return;
        };
        let items = [
            MenuItem {
                id: 0,
                label: "Open",
                shortcut: "",
                glyph: Icon::Folder,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 1,
                label: "Open in new tab",
                shortcut: "",
                glyph: Icon::Plus,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 2,
                label: "Open in new window",
                shortcut: "",
                glyph: Icon::Window,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 3,
                label: "Open in right split",
                shortcut: "",
                glyph: Icon::Split,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 4,
                label: "Open in bottom split",
                shortcut: "",
                glyph: Icon::SplitBelow,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 5,
                label: "Edit path",
                shortcut: "",
                glyph: Icon::Rename,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 6,
                label: "Copy path",
                shortcut: "",
                glyph: Icon::Copy,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 7,
                label: "Compact Paths",
                shortcut: "",
                glyph: Icon::Folder,
                checked: Some(self.compact_paths),
                enabled: true,
            },
            MenuItem {
                id: 8,
                label: "Path Breadcrumbs",
                shortcut: "",
                glyph: Icon::Clock,
                checked: Some(self.breadcrumb_history),
                enabled: true,
            },
        ];
        if let Some(item) = self.breadcrumb_menu.menu.show_sections_width(
            ui.ctx(),
            Id::new((self.id, "breadcrumb-actions")),
            t,
            *anchor,
            &items,
            &[MenuSection::separator(5), MenuSection::separator(7)],
            288.0 * t.metrics().scale,
        ) {
            let path = self.breadcrumb_menu.target.as_ref().unwrap().0.clone();
            match item {
                0 => actions.push(Action::Navigate(path)),
                1 => actions.push(Action::NewTab(path)),
                2 => actions.push(Action::NewWindow(path)),
                3 | 4 => actions.push(Action::SplitAt {
                    path,
                    vertical: item == 4,
                }),
                5 => actions.push(Action::EditPath(path)),
                6 => actions.push(Action::CopyPaths(vec![path])),
                7 => self.set_breadcrumb_options(!self.compact_paths, self.breadcrumb_history),
                8 => self.set_breadcrumb_options(self.compact_paths, !self.breadcrumb_history),
                _ => {}
            }
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    #[test]
    fn shift_f10_opens_actions_for_the_focused_breadcrumb() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(731, &engine, PathBuf::from("C:/virtual/child"));
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        t.apply(&ctx);
        let index = pane.breadcrumbs.len() - 1;
        let trigger = pane.breadcrumbs[index].id;
        let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let out = pane.breadcrumb_bar.ui(
                        ui,
                        Rect::from_min_size(pos2(20.0, 50.0), vec2(950.0, 28.0)),
                        Id::new((pane.id, "breadcrumbs")),
                        &t,
                        &pane.breadcrumbs,
                        pane.breadcrumb_current,
                        pane.breadcrumb_menu.active_trigger(),
                        |_| String::new(),
                    );
                    pane.breadcrumb_hidden = out.hidden_count;
                    pane.breadcrumb_context_ui(ui, &t, out.context, out.active_rect, &mut vec![]);
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0, vec![]);
        crate::keyboard::request_id(&ctx, trigger.with("label"));
        draw(&mut pane, 1, vec![]);
        draw(
            &mut pane,
            2,
            vec![egui::Event::Key {
                key: Key::F10,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            }],
        );
        assert!(pane.breadcrumb_menu.menu.open);
        assert_eq!(pane.breadcrumb_menu.target.as_ref().unwrap().0, pane.path);
    }
}
