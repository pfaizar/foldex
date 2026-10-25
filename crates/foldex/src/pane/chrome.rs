//! Shared pane bars for folder, Home, and Recycle Bin views.
use super::*;

pub(crate) struct PaneChrome {
    pub(crate) toolbar: Rect,
    pub(crate) footer: Rect,
    pub(crate) breadcrumbs: Rect,
    pub(super) recycle_controls: (Rect, Rect),
}

impl Pane {
    pub(crate) fn chrome(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        all: Rect,
        active: bool,
        inspector: bool,
        actions: &mut Vec<Action>,
    ) -> PaneChrome {
        let m = t.metrics();
        let scale = m.scale;
        let footer = Rect::from_min_max(
            pos2(all.left(), (all.bottom() - m.footer).max(all.top())),
            all.max,
        );
        let recycle_stacked = self.is_recycle_bin() && all.width() < 440.0 * scale;
        let toolbar = Rect::from_min_max(
            all.min,
            pos2(
                all.right(),
                (all.top()
                    + m.toolbar
                    + if recycle_stacked {
                        m.control + m.gap
                    } else {
                        0.0
                    })
                .min(footer.top()),
            ),
        );
        ui.painter()
            .rect_filled(toolbar, 0, Theme::color(t.background));
        use foldex_ui::components::icon_button_at;
        let button_y = toolbar.top() + m.toolbar * 0.5;
        let mut x = toolbar.left() + m.inset;
        let nav_count = if toolbar.width() < 320.0 * scale
            || self.is_recycle_bin() && toolbar.width() < 640.0 * scale
        {
            1
        } else {
            3
        };
        for (n, (glyph, hint)) in [
            (Icon::Back, "Back · Alt+Left"),
            (Icon::Forward, "Forward · Alt+Right"),
            (Icon::Up, "Parent folder · Alt+Up"),
        ]
        .into_iter()
        .take(nav_count)
        .enumerate()
        {
            let rect = Rect::from_center_size(
                pos2(x + m.control * 0.5, button_y),
                vec2(m.control, m.control),
            );
            let enabled = match n {
                0 => self.navigation.can_back(),
                1 => self.navigation.can_forward(),
                _ => !self.is_virtual() && self.path.parent().is_some(),
            };
            let response = foldex_ui::components::icon_button_at_enabled(
                ui,
                rect,
                Id::new((self.id, "navigation", n)),
                t,
                glyph,
                false,
                enabled,
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled() && enabled,
                    hint,
                )
            });
            if response
                .on_hover_text(hint)
                .on_disabled_hover_text(hint)
                .clicked()
            {
                match n {
                    0 => self.back(),
                    1 => self.forward(),
                    _ => self.up(),
                }
            }
            x += m.control;
        }
        let more = Rect::from_center_size(
            pos2(toolbar.right() - m.inset - m.control * 0.5, button_y),
            vec2(m.control, m.control),
        );
        self.more_anchor = more;
        if icon_button_at(
            ui,
            more,
            Id::new((self.id, "more")),
            t,
            Icon::More,
            self.more_menu.open,
        )
        .on_hover_text("Folder actions")
        .clicked()
        {
            self.more_menu.toggle(ui.ctx());
        }
        let inspect = more.translate(vec2(-m.control, 0.0));
        let inspect_available = inspector_available(ui.ctx(), t) && !self.is_recycle_bin();
        if !self.is_recycle_bin() {
            let response = foldex_ui::components::icon_button_at_enabled(
                ui,
                inspect,
                Id::new((self.id, "inspector")),
                t,
                Icon::Inspect,
                inspector && inspect_available,
                inspect_available,
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    ui.is_enabled() && inspect_available,
                    inspector && inspect_available,
                    "Inspector",
                )
            });
            if response
                .on_hover_text("Inspector · Space")
                .on_disabled_hover_text("Widen the window to show the inspector")
                .clicked()
            {
                actions.push(Action::ToggleInspector);
            }
        }
        let mut right = if self.is_recycle_bin() {
            more.left()
        } else {
            inspect.left()
        } - m.gap;
        if toolbar.width() > 440.0 * scale || self.is_recycle_bin() && !recycle_stacked {
            let bookmark = if self.is_recycle_bin() {
                inspect
            } else {
                inspect.translate(vec2(-m.control, 0.0))
            };
            if foldex_ui::components::icon_button_at_enabled(
                ui,
                bookmark,
                Id::new((self.id, "bookmark")),
                t,
                if self.bookmarked {
                    Icon::BookmarkFilled
                } else {
                    Icon::Bookmark
                },
                self.bookmarked,
                !self.is_virtual(),
            )
            .on_hover_text(if self.bookmarked {
                "Remove folder bookmark"
            } else {
                "Bookmark this folder"
            })
            .clicked()
            {
                actions.push(Action::Bookmark(self.path.clone()));
            }
            right = bookmark.left() - m.gap;
        }
        let mut recycle_controls = (Rect::NOTHING, Rect::NOTHING);
        if self.is_recycle_bin() {
            let controls = self.recycle_controls(
                ui,
                t,
                if recycle_stacked {
                    toolbar.right() - m.inset
                } else {
                    right
                },
                if recycle_stacked {
                    toolbar.bottom() - m.control * 0.5
                } else {
                    button_y
                },
                actions,
            );
            if !recycle_stacked {
                right = controls.0;
            }
            recycle_controls = (controls.1, controls.2);
        }
        let crumbs_rect = Rect::from_min_max(
            pos2((x + m.gap).min(right), toolbar.top() + 2.0 * scale),
            pos2(right, toolbar.top() + m.toolbar - 2.0 * scale),
        );
        if self.is_virtual() {
            if self.virtual_breadcrumb(ui, t, crumbs_rect) {
                actions.push(Action::Address);
            }
        } else {
            let count = self.breadcrumbs.len();
            let path = &self.breadcrumb_path;
            let breadcrumb = self.breadcrumb_bar.ui(
                ui,
                crumbs_rect,
                Id::new((self.id, "breadcrumbs")),
                t,
                &self.breadcrumbs,
                self.breadcrumb_current,
                self.breadcrumb_active
                    .or(self.breadcrumb_menu.active_trigger()),
                |index| {
                    path.ancestors()
                        .nth(count - 1 - index)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default()
                },
            );
            self.breadcrumb_anchor = breadcrumb.active_rect;
            self.breadcrumb_hidden = breadcrumb.hidden_count;
            let path_at = |index: usize| {
                self.breadcrumb_path
                    .ancestors()
                    .nth(count.checked_sub(index + 1)?)
                    .map(Path::to_path_buf)
            };
            if let Some((index, new_tab)) = breadcrumb.navigate
                && let Some(path) = path_at(index)
            {
                actions.push(if new_tab {
                    Action::NewTab(path)
                } else {
                    Action::Navigate(path)
                });
            }
            if let Some((index, anchor, trigger)) = breadcrumb.children
                && let Some(path) = path_at(index)
            {
                actions.push(Action::BreadcrumbChildren {
                    path,
                    current: path_at(index + 1),
                    anchor,
                    trigger,
                });
            }
            if let Some((end, anchor, trigger)) = breadcrumb.ancestors {
                actions.push(Action::BreadcrumbAncestors {
                    paths: (0..end).filter_map(path_at).collect(),
                    anchor,
                    trigger,
                });
            }
            if active
                && foldex_ui::components::can_focus(ui)
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::ALT, Key::ArrowDown))
                && let Some(anchor) = breadcrumb.current_rect
                && let Some(item) = self.breadcrumbs.get(self.breadcrumb_current)
            {
                ui.memory_mut(|memory| memory.request_focus(item.id.with("label")));
                actions.push(Action::BreadcrumbChildren {
                    path: self.path.clone(),
                    current: path_at(self.breadcrumb_current + 1),
                    anchor,
                    trigger: item.id,
                });
            }
            if breadcrumb.address {
                actions.push(Action::Address);
            }
            self.breadcrumb_context_ui(ui, t, breadcrumb.context, breadcrumb.active_rect, actions);
        }
        let items = pane_menu_items(
            self.recursive,
            self.split_orientation,
            self.query.show_hidden,
            self.path.parent().is_some(),
            self.view_len(),
            self.selected.len(),
            self.is_virtual(),
        );
        if let Some(item) = self.more_menu.show_sections(
            ui.ctx(),
            Id::new((self.id, "folder-menu")),
            t,
            more,
            &items,
            &PANE_MENU_SECTIONS,
        ) {
            if let Some(action) = self.more_menu_command(item) {
                actions.push(action);
            }
        }
        PaneChrome {
            toolbar,
            footer,
            breadcrumbs: crumbs_rect,
            recycle_controls,
        }
    }
    pub(super) fn virtual_breadcrumb(&mut self, ui: &Ui, t: &Theme, rect: Rect) -> bool {
        let (label, glyph, key, hint) = if self.is_home() {
            (
                "Home",
                Icon::Home,
                "home-address",
                "Home · Open a folder or address with Ctrl+L",
            )
        } else {
            (
                "Recycle Bin",
                Icon::Trash,
                "recycle-address",
                crate::navigation::RECYCLE_BIN_ADDRESS,
            )
        };
        let response = foldex_ui::components::button_response(ui, rect, Id::new((self.id, key)));
        crate::keyboard::register(
            ui,
            crate::keyboard::Region::Toolbar(self.id),
            response.id,
            rect,
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        });
        foldex_ui::components::focus_ring(ui, rect, response.id, t, response.has_focus());
        icon(
            ui.painter(),
            pos2(rect.left() + 12.0 * t.metrics().scale, rect.center().y),
            glyph,
            t.dim(),
            16.0 * t.metrics().scale,
        );
        foldex_ui::components::elided_label(
            ui.painter(),
            rect.shrink2(vec2(26.0 * t.metrics().scale, 0.0)),
            t,
            label,
            t.fg(),
            false,
        );
        response.on_hover_text(hint).clicked()
    }
}
