//! This PC lists drives with their capacity, like File Explorer. Tiles read the
//! sidebar's cached drive samples; painting never queries a disk.
use super::home_ui::{paint_image, section_heading};
use super::*;
use crate::{
    assets::{AssetManager, AssetStamp, ViewportSignature, VisibleAsset},
    sidebar::{Usage, Volume},
};

#[derive(Clone, Default)]
struct ThisPcKeyboard {
    focused: Option<PathBuf>,
    was_focused: bool,
    reveal: bool,
}

impl Foldex {
    pub(crate) fn navigate_this_pc(&mut self) {
        if !self.pane().is_this_pc() {
            self.pane_mut().navigate_this_pc(true);
        }
        self.sidebar_data.refresh_now();
        self.session_dirty = true;
    }
}

/// A selected drive can be opened, inspected or have its path copied, but it
/// is never copied, moved, renamed or deleted from here.
pub(super) fn command_allowed(command: crate::commands::Command) -> bool {
    use crate::commands::Command;
    !matches!(
        command,
        Command::Copy
            | Command::Cut
            | Command::Rename
            | Command::Recycle
            | Command::DeletePermanent
    )
}

fn usage_summary(usage: Option<Usage>, pending: bool) -> String {
    match usage {
        Some(usage) => format!(
            "{} free of {}",
            foldex_ui::format_size(usage.free),
            foldex_ui::format_size(usage.total)
        ),
        None if pending => "Measuring…".into(),
        None => "Capacity unavailable".into(),
    }
}

fn this_pc_step(current: usize, count: usize, columns: usize, key: Key, page: usize) -> usize {
    let last = count.saturating_sub(1);
    let columns = columns.max(1);
    match key {
        Key::Home => 0,
        Key::End => last,
        Key::ArrowLeft => current.saturating_sub(1),
        Key::ArrowRight => (current + 1).min(last),
        Key::ArrowDown => current.saturating_add(columns).min(last),
        Key::ArrowUp => current.saturating_sub(columns),
        Key::PageDown => current.saturating_add(page.max(1)).min(last),
        Key::PageUp => current.saturating_sub(page.max(1)),
        _ => current,
    }
}

/// This PC shares the pane chrome and footer with Home. Drive tiles use the
/// same card treatment as Home's Recommended section.
pub(super) fn pane_ui(
    ui: &mut Ui,
    t: &Theme,
    pane: &mut Pane,
    data: &crate::sidebar::Data,
    assets: &mut AssetManager,
    active: bool,
    inspector: bool,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let all = ui.max_rect();
    let m = t.metrics();
    let scale = m.scale;
    let listing_id = crate::keyboard::listing_id(pane.id);
    let filter_id = Id::new((pane.id, "filter"));
    let state_id = listing_id.with("this-pc-keyboard");
    let mut keyboard = ui
        .ctx()
        .data(|data| data.get_temp::<ThisPcKeyboard>(state_id))
        .unwrap_or_default();
    if ui.memory(|memory| memory.has_focus(listing_id))
        && Pane::consume_navigation_key(ui.ctx(), egui::Modifiers::SHIFT, Key::Tab)
    {
        pane.focus_filter = true;
        crate::keyboard::request_id(ui.ctx(), filter_id);
    }
    let chrome = pane.chrome(ui, t, all, active, inspector, &mut actions);
    let toolbar = chrome.toolbar;
    let footer = chrome.footer;
    let address = chrome.breadcrumbs;
    let toolbar_painter = ui
        .painter()
        .with_clip_rect(toolbar.intersect(ui.clip_rect()));
    if !pane.is_this_pc() {
        return actions;
    }

    let body_top = (toolbar.bottom() + 3.0 * scale).min(footer.top());
    let body = Rect::from_min_max(
        pos2(all.left(), body_top),
        pos2(all.right(), (footer.top() - 2.0 * scale).max(body_top)),
    );
    let padding = (20.0 * scale).min(body.width() * 0.06);
    let scroll_width = ui.spacing().scroll.allocated_width();
    let width = (body.width() - 2.0 * padding - scroll_width).max(1.0);
    let gap = 8.0 * scale;
    let columns = ((width + gap) / (248.0 * scale + gap))
        .floor()
        .clamp(1.0, 6.0) as usize;
    let tile_width =
        ((width - gap * (columns - 1) as f32) / columns as f32).clamp(1.0, 380.0 * scale);
    let tile_height = (2.0 * t.font_size + 46.0 * scale).max(72.0 * scale);
    let icon_size = (40.0 * scale).min((tile_height - 16.0 * scale).max(0.0));

    let query = pane.filter.to_lowercase();
    let matches: Vec<&Volume> = data
        .volumes()
        .iter()
        .filter(|volume| query.is_empty() || volume.label.to_lowercase().contains(&query))
        .collect();
    let shown: &[&Volume] = if pane.query.kind.includes(true) {
        &matches
    } else {
        &[]
    };

    crate::keyboard::composite(
        ui,
        crate::keyboard::Region::Listing(pane.id),
        listing_id,
        body,
        "This PC drives",
    );
    foldex_ui::components::register_focus_target(ui, body, listing_id);
    if !shown
        .iter()
        .any(|volume| keyboard.focused.as_ref() == Some(&volume.path))
    {
        keyboard.focused = shown
            .iter()
            .find(|volume| pane.home_selected.as_ref() == Some(&volume.path))
            .or_else(|| shown.first())
            .map(|volume| volume.path.clone());
    }
    let owned = ui.memory(|memory| memory.has_focus(listing_id));
    foldex_ui::components::focus_ring(
        ui,
        body,
        listing_id.with("empty-this-pc-ring"),
        t,
        owned && shown.is_empty(),
    );
    let mut context_menu = false;
    if active && owned && !pane.has_popup() && foldex_ui::components::can_focus(ui) {
        keyboard.reveal |= !keyboard.was_focused;
        for _ in 0..32 {
            let Some(key) = Pane::take_composite_key(
                ui.ctx(),
                &[
                    Key::ArrowDown,
                    Key::ArrowUp,
                    Key::ArrowLeft,
                    Key::ArrowRight,
                    Key::Home,
                    Key::End,
                    Key::PageDown,
                    Key::PageUp,
                    Key::Escape,
                    Key::Enter,
                    Key::Space,
                    Key::F10,
                ],
            ) else {
                break;
            };
            if key == Key::Escape {
                if !pane.filter.is_empty() {
                    pane.filter.clear();
                } else {
                    pane.home_selected = None;
                }
                ui.ctx().request_repaint();
                break;
            }
            if shown.is_empty() {
                continue;
            }
            if key == Key::F10 {
                context_menu = true;
                break;
            }
            if matches!(key, Key::Enter | Key::Space) {
                if let Some(path) = &keyboard.focused {
                    pane.home_selected = Some(path.clone());
                    if key == Key::Enter {
                        actions.push(Action::Navigate(path.clone()));
                    }
                }
                if key == Key::Space {
                    continue;
                }
                break;
            }
            let at = shown
                .iter()
                .position(|volume| keyboard.focused.as_ref() == Some(&volume.path))
                .unwrap_or(0);
            let page = (body.height() / (tile_height + gap)).floor().max(1.0) as usize * columns;
            keyboard.focused = shown
                .get(this_pc_step(at, shown.len(), columns, key, page))
                .map(|volume| volume.path.clone());
            keyboard.reveal = true;
        }
    }
    keyboard.was_focused = owned;

    let tiles_top = 40.0 * scale;
    let tile_rows = shown.len().div_ceil(columns);
    let total_height = (tiles_top
        + tile_rows as f32 * (tile_height + gap)
        + if tile_rows == 0 { 40.0 * scale } else { 0.0 })
    .max(body.height() - 2.0 * padding);
    let pending = data.pending();
    ui.scope_builder(
        UiBuilder::new()
            .id_salt((pane.id, "this-pc-body"))
            .max_rect(body),
        |ui| {
            ui.set_clip_rect(body.intersect(ui.clip_rect()));
            egui::ScrollArea::vertical()
                .id_salt((pane.id, "this-pc-scroll"))
                .auto_shrink([false, false])
                .min_scrolled_height(1.0)
                .show_viewport(ui, |ui, _viewport| {
                    let (content, _) = ui.allocate_exact_size(
                        vec2(
                            (body.width() - scroll_width).max(0.0),
                            total_height + padding * 2.0,
                        ),
                        Sense::hover(),
                    );
                    let origin = content.min + vec2(padding, padding);
                    let p = ui.painter().with_clip_rect(body.intersect(ui.clip_rect()));
                    let tiles: Vec<(&Volume, Rect)> = shown
                        .iter()
                        .enumerate()
                        .map(|(slot, volume)| {
                            (
                                *volume,
                                Rect::from_min_size(
                                    origin
                                        + vec2(
                                            (slot % columns) as f32 * (tile_width + gap),
                                            tiles_top
                                                + (slot / columns) as f32 * (tile_height + gap),
                                        ),
                                    vec2(tile_width, tile_height),
                                ),
                            )
                        })
                        .collect();

                    // One bounded asset lane per visible This PC pane: the page
                    // icon for the address bar, then one native icon per drive.
                    let physical = icon_size.max(m.icon) * ui.ctx().pixels_per_point();
                    let size = [16, 48, 96, 152, 256]
                        .into_iter()
                        .find(|size| *size as f32 >= physical)
                        .unwrap_or(256);
                    let mut signature = std::collections::hash_map::DefaultHasher::new();
                    for (volume, _) in &tiles {
                        volume.path.hash(&mut signature);
                    }
                    let epoch = pane.navigation_epoch().wrapping_add(signature.finish());
                    let _ = assets.set_requests(
                        pane.id,
                        ViewportSignature {
                            epoch,
                            revision: data.revision(),
                            first: 0,
                            end: tiles.len() + 1,
                            size,
                            thumbnails: false,
                        },
                        || {
                            std::iter::once(foldex_assets::AssetKey::stock_icon(
                                foldex_assets::StockIcon::Computer,
                                size,
                            ))
                            .chain(tiles.iter().map(|(volume, _)| foldex_assets::AssetKey {
                                path: volume.path.clone(),
                                size,
                                thumbnail: false,
                            }))
                            .map(|key| VisibleAsset {
                                key,
                                stamp: AssetStamp::UNOBSERVED,
                            })
                            .collect()
                        },
                    );
                    if let Some(image) = assets.image(pane.id, epoch, 0) {
                        paint_image(
                            &toolbar_painter,
                            Rect::from_center_size(
                                pos2(address.left() + 12.0 * scale, address.center().y),
                                vec2(m.icon, m.icon),
                            ),
                            image,
                        );
                    }

                    section_heading(&p, origin, width, t, "Devices and drives");
                    if shown.is_empty() {
                        p.text(
                            origin + vec2(0.0, 44.0 * scale),
                            Align2::LEFT_TOP,
                            if !pane.query.kind.includes(true) {
                                "Folders are hidden."
                            } else if !query.is_empty() && !data.volumes().is_empty() {
                                "No drives match this filter."
                            } else if pending {
                                "Looking for drives…"
                            } else {
                                "No drives are available."
                            },
                            egui::FontId::proportional(t.font_size),
                            t.dim(),
                        );
                    }
                    for (asset, (volume, rect)) in tiles.iter().enumerate() {
                        let rect = *rect;
                        let selected = pane.home_selected.as_ref() == Some(&volume.path);
                        let focused = keyboard.focused.as_ref() == Some(&volume.path);
                        if focused && keyboard.reveal {
                            ui.scroll_to_rect(rect.expand(gap), None);
                        }
                        let response = ui.interact(
                            rect,
                            Id::new((pane.id, "this-pc-drive", &volume.path)),
                            Sense::CLICK,
                        );
                        if response.clicked() || response.secondary_clicked() {
                            pane.home_selected = Some(volume.path.clone());
                            keyboard.focused = Some(volume.path.clone());
                            crate::keyboard::request_id(ui.ctx(), listing_id);
                        }
                        if response.double_clicked() {
                            actions.push(Action::Navigate(volume.path.clone()));
                        }
                        if response.middle_clicked() {
                            actions.push(Action::NewTab(volume.path.clone()));
                        }
                        if response.secondary_clicked() || context_menu && focused {
                            actions.push(Action::Context {
                                paths: vec![volume.path.clone()],
                                background: false,
                                position: response
                                    .interact_pointer_pos()
                                    .filter(|_| response.secondary_clicked())
                                    .unwrap_or(rect.left_bottom()),
                            });
                        }
                        if !rect.intersects(p.clip_rect()) {
                            continue;
                        }
                        p.rect_filled(
                            rect,
                            t.radius.saturating_add(2),
                            if response.hovered() {
                                Theme::color(t.hover)
                            } else {
                                Theme::color(t.surface)
                            },
                        );
                        p.rect_stroke(
                            rect,
                            t.radius.saturating_add(2),
                            Stroke::new(1.0, if selected { t.accent() } else { t.line() }),
                            StrokeKind::Inside,
                        );
                        let icon_rect = Rect::from_center_size(
                            pos2(
                                rect.left() + 14.0 * scale + icon_size * 0.5,
                                rect.center().y,
                            ),
                            vec2(icon_size, icon_size),
                        );
                        let image = assets.image(pane.id, epoch, asset + 1);
                        icon(
                            &p,
                            icon_rect.center(),
                            Icon::Drive,
                            t.dim().gamma_multiply(
                                image.as_ref().map_or(1.0, |image| 1.0 - image.opacity),
                            ),
                            icon_size,
                        );
                        if let Some(image) = image {
                            paint_image(&p, icon_rect, image);
                        }
                        let left = icon_rect.right() + 12.0 * scale;
                        let right = (rect.right() - 14.0 * scale).max(left);
                        let line = t.font_size + 6.0 * scale;
                        let bar_height = 6.0 * scale;
                        let top = rect.center().y - (2.0 * line + bar_height + 8.0 * scale) * 0.5;
                        let name = Rect::from_min_max(pos2(left, top), pos2(right, top + line));
                        foldex_ui::components::elided_label(
                            &p,
                            name,
                            t,
                            &volume.label,
                            t.fg(),
                            false,
                        );
                        let bar = Rect::from_min_max(
                            pos2(left, name.bottom() + 4.0 * scale),
                            pos2(right, name.bottom() + 4.0 * scale + bar_height),
                        );
                        p.rect_filled(bar, bar_height * 0.5, t.line());
                        if let Some(usage) = volume.usage {
                            p.rect_filled(
                                Rect::from_min_size(
                                    bar.min,
                                    vec2(bar.width() * usage.fraction(), bar.height()),
                                ),
                                bar_height * 0.5,
                                if usage.near_full() {
                                    Theme::color(t.danger)
                                } else {
                                    t.accent()
                                },
                            );
                        }
                        foldex_ui::components::elided_label(
                            &p,
                            Rect::from_min_max(
                                pos2(left, bar.bottom() + 4.0 * scale),
                                pos2(right, bar.bottom() + 4.0 * scale + line),
                            ),
                            t,
                            &usage_summary(volume.usage, pending),
                            t.dim(),
                            false,
                        );
                        if focused && owned {
                            foldex_ui::components::focus_ring_with_radius(
                                ui,
                                rect,
                                listing_id.with("this-pc-item-ring"),
                                t,
                                true,
                                t.radius.saturating_add(2),
                            );
                        }
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                ui.is_enabled(),
                                selected,
                                &volume.label,
                            )
                        });
                        if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
                            response.on_hover_ui(|ui| {
                                ui.set_max_width(ui.spacing().tooltip_width);
                                ui.label(match volume.usage {
                                    Some(usage) => format!(
                                        "{}\n{} · {:.0}% full",
                                        crate::pane::friendly_path(&volume.path),
                                        usage_summary(Some(usage), false),
                                        usage.fraction() * 100.0
                                    ),
                                    None => format!(
                                        "{}\n{}",
                                        crate::pane::friendly_path(&volume.path),
                                        usage_summary(None, pending)
                                    ),
                                });
                            });
                        }
                    }
                });
        },
    );

    let selected = usize::from(
        shown
            .iter()
            .any(|volume| pane.home_selected.as_ref() == Some(&volume.path)),
    );
    ui.painter().rect_filled(footer, 0, t.bg());
    ui.painter()
        .hline(footer.x_range(), footer.top(), Stroke::new(1.0, t.line()));
    ui.scope_builder(UiBuilder::new().max_rect(footer), |ui| {
        ui.set_clip_rect(footer.intersect(ui.clip_rect()));
        pane.home_footer(
            ui,
            t,
            footer,
            (!pending || !data.volumes().is_empty()).then_some((matches.len(), 0)),
            (selected, 0),
        );
    });
    keyboard.reveal = false;
    ui.ctx()
        .data_mut(|data| data.insert_temp(state_id, keyboard));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_tiles_move_through_the_grid_and_stop_at_the_ends() {
        assert_eq!(this_pc_step(0, 5, 2, Key::ArrowRight, 4), 1);
        assert_eq!(this_pc_step(4, 5, 2, Key::ArrowRight, 4), 4);
        assert_eq!(this_pc_step(1, 5, 2, Key::ArrowDown, 4), 3);
        assert_eq!(this_pc_step(3, 5, 2, Key::ArrowDown, 4), 4);
        assert_eq!(this_pc_step(3, 5, 2, Key::ArrowUp, 4), 1);
        assert_eq!(this_pc_step(0, 5, 2, Key::ArrowLeft, 4), 0);
        assert_eq!(this_pc_step(2, 5, 2, Key::End, 4), 4);
        assert_eq!(this_pc_step(2, 5, 2, Key::Home, 4), 0);
    }

    #[test]
    fn drives_cannot_be_moved_renamed_or_deleted_from_this_pc() {
        use crate::commands::Command;
        for command in [
            Command::Copy,
            Command::Cut,
            Command::Rename,
            Command::Recycle,
            Command::DeletePermanent,
        ] {
            assert!(!command_allowed(command));
        }
        assert!(command_allowed(Command::CopyPath));
        assert!(command_allowed(Command::Properties));
    }

    #[test]
    fn capacity_reads_like_file_explorer() {
        let usage = Usage::from_bytes(476 << 30, 38 << 30);
        assert_eq!(usage_summary(usage, false), "38.0 GB free of 476 GB");
        assert_eq!(usage_summary(None, true), "Measuring…");
        assert_eq!(usage_summary(None, false), "Capacity unavailable");
    }
}
