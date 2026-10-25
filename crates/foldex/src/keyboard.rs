//! Root-window focus regions. Collections register one tab stop; their own
//! arrow-key navigation stays independent of file selection and text editing.
use egui::{Context, Id, Rect, Response, Sense, Ui};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum Region {
    Tabs,
    Sidebar,
    Toolbar(u64),
    Listing(u64),
    Search(u64),
    Footer(u64),
    Inspector,
}

#[derive(Clone, Copy)]
struct Target {
    region: Region,
    id: Id,
    primary: bool,
    rect: Rect,
}

#[derive(Clone, Default)]
struct Regions {
    frame: u64,
    current: Vec<Target>,
    previous: Vec<Target>,
    remembered: Vec<(Region, Id)>,
}

#[derive(Clone, Default)]
struct Composition {
    owner: Option<Id>,
    frame: Option<u64>,
    active: bool,
    blocked: bool,
}

/// Tab belongs to the input method while a text field has an active candidate.
pub(crate) fn composing(ctx: &Context) -> bool {
    let owner = ctx.memory(|memory| memory.focused());
    let frame = ctx.cumulative_frame_nr();
    let events = ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    Some(Some(!text.is_empty()))
                }
                egui::Event::Ime(egui::ImeEvent::Commit(_)) => Some(Some(false)),
                egui::Event::Ime(_) => Some(None),
                _ => None,
            })
            .take(64)
            .collect::<Vec<_>>()
    });
    let key = Id::new(("root-keyboard-composition", ctx.viewport_id()));
    ctx.data_mut(|data| {
        let state = data.get_temp_mut_or_default::<Composition>(key);
        if state.owner != owner {
            state.owner = owner;
            state.active = false;
        }
        if state.frame != Some(frame) {
            state.frame = Some(frame);
            state.blocked = state.active || !events.is_empty();
            for active in events.iter().flatten() {
                state.active = *active;
            }
        }
        state.active || state.blocked
    })
}

pub(crate) fn printable_search_text(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|character| !character.is_control())
}

/// Text comes from the platform, rather than a guessed character for a Key.
/// Inspect modifiers in event order: a frame's final modifier state can differ
/// from the one that produced its first character. IME owns its entire batch.
pub(crate) fn has_search_text(ctx: &Context) -> bool {
    if !ctx.input(|input| {
        input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Text(_) | egui::Event::Ime(_)))
    }) {
        return false;
    }
    if composing(ctx) {
        return false;
    }
    ctx.input(|input| {
        if !input.focused
            || input.key_down(egui::Key::SuperLeft)
            || input.key_down(egui::Key::SuperRight)
        {
            return false;
        }
        let mut modifiers = input.modifiers;
        for event in &input.events {
            match event {
                egui::Event::Key {
                    key: egui::Key::SuperLeft | egui::Key::SuperRight,
                    pressed: true,
                    ..
                } => return false,
                egui::Event::Key {
                    modifiers: next, ..
                }
                | egui::Event::ModifiersChanged(next) => modifiers = *next,
                egui::Event::Text(text) if !text.is_empty() => {
                    if modifiers.alt
                        || modifiers.ctrl
                        || modifiers.command
                        || modifiers.mac_cmd
                        || !printable_search_text(text)
                    {
                        return false;
                    }
                    // A lone Space retains preview/button activation. Later
                    // strokes and payloads belong to the editor after this
                    // opening text has established its ownership.
                    return text.chars().any(|character| !character.is_whitespace());
                }
                // Clipboard payloads must keep their file/editor ownership.
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => return false,
                _ => {}
            }
        }
        false
    })
}

fn state_id(ctx: &Context) -> Id {
    Id::new(("foldex-keyboard-regions", ctx.viewport_id()))
}

pub(crate) fn listing_id(pane: u64) -> Id {
    Id::new((pane, "listing-focus"))
}

pub(crate) fn sidebar_id() -> Id {
    Id::new("sidebar-navigation-focus")
}

fn register_target(ui: &Ui, region: Region, id: Id, rect: Rect, primary: bool) {
    if !foldex_ui::components::can_focus(ui)
        || !rect.is_positive()
        || !rect.intersects(ui.clip_rect())
        || ui.is_sizing_pass()
    {
        return;
    }
    let ctx = ui.ctx();
    let key = state_id(ctx);
    let frame = ctx.cumulative_frame_nr();
    let focused = ctx.memory(|memory| memory.focused());
    ctx.data_mut(|data| {
        let state = data.get_temp_mut_or_default::<Regions>(key);
        if state.frame != frame {
            if frame == state.frame.saturating_add(1) {
                std::mem::swap(&mut state.current, &mut state.previous);
                state.current.clear();
            } else {
                state.current.clear();
                state.previous.clear();
            }
            state.frame = frame;
        }
        if let Some(target) = state.current.iter_mut().find(|target| target.id == id) {
            target.primary |= primary;
            target.rect = rect;
        } else if state.current.len() < 512 {
            state.current.push(Target {
                region,
                id,
                primary,
                rect,
            });
        }
        if focused == Some(id) {
            if let Some(entry) = state.remembered.iter_mut().find(|entry| entry.0 == region) {
                entry.1 = id;
            } else {
                if state.remembered.len() >= 64 {
                    state.remembered.remove(0);
                }
                state.remembered.push((region, id));
            }
        }
    });
}

/// Register an existing control without adding a second interaction/focus node.
pub(crate) fn register(ui: &Ui, region: Region, id: Id, rect: Rect) {
    register_target(ui, region, id, rect, false);
}

/// One tab stop for a list/tree/grid. Pointer rows should use Sense::CLICK/DRAG.
/// The caller paints its focused item and handles collection navigation keys.
pub(crate) fn composite(ui: &Ui, region: Region, id: Id, rect: Rect, label: &str) -> Response {
    let response = ui.interact(rect, id, Sense::focusable_noninteractive());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), label));
    foldex_ui::components::register_focus_target(ui, rect, id);
    register_target(ui, region, id, rect, true);
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                    tab: false,
                },
            )
        });
    }
    response
}

fn with_regions<R>(ctx: &Context, read: impl FnOnce(&Regions) -> R) -> R {
    let key = state_id(ctx);
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| {
        let state = data.get_temp_mut_or_default::<Regions>(key);
        if frame > state.frame.saturating_add(1) {
            state.current.clear();
            state.previous.clear();
        }
        read(state)
    })
}

pub(crate) fn focused_region(ctx: &Context) -> Option<Region> {
    let id = ctx.memory(|memory| memory.focused())?;
    with_regions(ctx, |state| {
        state
            .current
            .iter()
            .chain(&state.previous)
            .find(|target| target.id == id)
            .map(|target| target.region)
    })
}

pub(crate) fn owns(ctx: &Context, region: Region) -> bool {
    focused_region(ctx) == Some(region)
}

pub(crate) fn listing_owns(ctx: &Context, pane: u64) -> bool {
    ctx.memory(|memory| memory.focused()) == Some(listing_id(pane))
        || owns(ctx, Region::Listing(pane))
}

pub(crate) fn any_listing_owns(ctx: &Context) -> bool {
    matches!(focused_region(ctx), Some(Region::Listing(_)))
}

pub(crate) fn request_id(ctx: &Context, id: Id) {
    ctx.memory_mut(|memory| {
        memory.move_focus(egui::FocusDirection::None);
        memory.request_focus(id);
    });
    ctx.request_repaint();
}

fn region_target(state: &Regions, region: Region) -> Option<Id> {
    let targets = || {
        state
            .current
            .iter()
            .chain(&state.previous)
            .filter(|target| target.region == region)
    };
    state
        .remembered
        .iter()
        .find(|entry| entry.0 == region)
        .and_then(|entry| {
            targets()
                .find(|target| target.id == entry.1)
                .map(|target| target.id)
        })
        .or_else(|| {
            targets()
                .find(|target| target.primary)
                .map(|target| target.id)
        })
        .or_else(|| targets().next().map(|target| target.id))
}

pub(crate) fn request(ctx: &Context, region: Region) -> bool {
    let Some(id) = with_regions(ctx, |state| region_target(state, region)) else {
        return false;
    };
    request_id(ctx, id);
    true
}

/// The caller supplies currently visible regions, so closed/hidden panes are
/// never reactivated by stale last-frame registrations.
pub(crate) fn cycle(ctx: &Context, order: &[Region], backwards: bool) -> Option<Region> {
    let current = focused_region(ctx);
    let available: Vec<_> = with_regions(ctx, |state| {
        order
            .iter()
            .copied()
            .filter_map(|region| region_target(state, region).map(|id| (region, id)))
            .collect()
    });
    if available.is_empty() {
        return None;
    }
    let at = current.and_then(|region| available.iter().position(|entry| entry.0 == region));
    let next = match (at, backwards) {
        (Some(at), true) => (at + available.len() - 1) % available.len(),
        (Some(at), false) => (at + 1) % available.len(),
        (None, true) => available.len() - 1,
        (None, false) => 0,
    };
    let (region, id) = available[next];
    request_id(ctx, id);
    Some(region)
}

/// Control traversal follows the application's logical region order instead
/// of paint order. In particular Search -> Files -> Footer -> next pane forms
/// a complete circuit, without trapping Tab in the search/results shortcut.
pub(crate) fn tab(ctx: &Context, order: &[Region], backwards: bool) -> bool {
    let focused = ctx.memory(|memory| memory.focused());
    let targets = with_regions(ctx, |state| {
        let mut targets = Vec::new();
        for region in order {
            let mut controls: Vec<_> = state
                .current
                .iter()
                .filter(|target| target.region == *region)
                .collect();
            controls.sort_by(|a, b| {
                // Tiny glyph/field inset differences do not reverse a visual
                // row. Within that row traverse from left to right.
                (a.rect.center().y / 8.0)
                    .round()
                    .total_cmp(&(b.rect.center().y / 8.0).round())
                    .then_with(|| a.rect.left().total_cmp(&b.rect.left()))
            });
            for target in controls {
                if !targets.contains(&target.id) {
                    targets.push(target.id);
                }
            }
        }
        targets
    });
    if targets.is_empty() {
        return false;
    }
    let at = focused.and_then(|id| targets.iter().position(|target| *target == id));
    let next = match (at, backwards) {
        (Some(at), true) => (at + targets.len() - 1) % targets.len(),
        (Some(at), false) => (at + 1) % targets.len(),
        (None, true) => targets.len() - 1,
        (None, false) => 0,
    };
    request_id(ctx, targets[next]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn search_text_frame(
        ctx: &Context,
        viewport: egui::ViewportId,
        modifiers: egui::Modifiers,
        mut events: Vec<egui::Event>,
    ) -> bool {
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let expected = events.clone();
        let mut input = egui::RawInput {
            viewport_id: viewport,
            events,
            focused: true,
            ..Default::default()
        };
        input.viewports.entry(viewport).or_default();
        let mut result = false;
        ctx.run_ui(input, |ui| {
            request_id(ui.ctx(), listing_id(1));
            result = has_search_text(ui.ctx());
            assert_eq!(ui.input(|input| input.events.clone()), expected);
            ui.interact(
                ui.max_rect(),
                listing_id(1),
                Sense::focusable_noninteractive(),
            );
        })
        .drop_without_applying_deltas();
        result
    }

    #[test]
    fn search_text_uses_platform_unicode_and_never_guesses_text_from_a_key() {
        use egui::{Event, Key, Modifiers, ViewportId};
        for events in [
            vec![press(Key::G, Modifiers::NONE), Event::Text("g".into())],
            vec![Event::Text("é\u{301}键🙂".into())],
            vec![press(Key::A, Modifiers::SHIFT), Event::Text("A".into())],
            vec![Event::Text("two words".into())],
            vec![
                press(Key::G, Modifiers::NONE),
                Event::Text("g".into()),
                press(Key::C, Modifiers::CTRL),
                Event::Copy,
            ],
        ] {
            assert!(search_text_frame(
                &Context::default(),
                ViewportId::ROOT,
                Modifiers::NONE,
                events,
            ));
        }
        for events in [
            vec![press(Key::A, Modifiers::NONE)],
            vec![press(Key::ArrowDown, Modifiers::NONE)],
            vec![press(Key::Space, Modifiers::NONE), Event::Text(" ".into())],
            vec![Event::Text("\n".into())],
            vec![Event::Paste("query".into())],
            vec![Event::Text(String::new())],
        ] {
            assert!(!search_text_frame(
                &Context::default(),
                ViewportId::ROOT,
                Modifiers::NONE,
                events,
            ));
        }
    }

    #[test]
    fn modified_text_and_windows_shortcuts_cannot_start_search() {
        use egui::{Event, Key, Modifiers, ViewportId};
        for modifiers in [
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::CTRL | Modifiers::ALT,
            Modifiers {
                command: true,
                ..Modifiers::NONE
            },
            Modifiers {
                mac_cmd: true,
                ..Modifiers::NONE
            },
        ] {
            assert!(!search_text_frame(
                &Context::default(),
                ViewportId::ROOT,
                modifiers,
                vec![press(Key::G, modifiers), Event::Text("g".into())],
            ));
        }
        assert!(!search_text_frame(
            &Context::default(),
            ViewportId::ROOT,
            Modifiers::NONE,
            vec![
                press(Key::SuperLeft, Modifiers::NONE),
                press(Key::G, Modifiers::NONE),
                Event::Text("g".into()),
            ],
        ));
        assert!(!search_text_frame(
            &Context::default(),
            ViewportId::ROOT,
            Modifiers::CTRL,
            vec![Event::Text("g".into())],
        ));
    }

    #[test]
    fn composition_blocks_its_commit_frame_and_recovers_after_cancellation() {
        use egui::{Event, ImeEvent, Modifiers, ViewportId};
        let ctx = Context::default();
        let check = |events| search_text_frame(&ctx, ViewportId::ROOT, Modifiers::NONE, events);
        assert!(!check(vec![Event::Ime(ImeEvent::Preedit {
            text: "に".into(),
            active_range_chars: None,
        })]));
        assert!(!check(vec![Event::Text("candidate".into())]));
        assert!(!check(vec![
            Event::Ime(ImeEvent::Commit("日本".into())),
            Event::Text("日本".into()),
        ]));
        assert!(check(vec![Event::Text("next".into())]));
        assert!(!check(vec![Event::Ime(ImeEvent::Preedit {
            text: "候補".into(),
            active_range_chars: None,
        })]));
        assert!(!check(vec![Event::Ime(ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        })]));
        assert!(check(vec![Event::Text("resumed".into())]));
    }

    #[test]
    fn composition_is_owned_by_its_viewport() {
        use egui::{Event, ImeEvent, Modifiers, ViewportId};
        let ctx = Context::default();
        let child = ViewportId::from_hash_of("type-to-search-child");
        assert!(!search_text_frame(
            &ctx,
            child,
            Modifiers::NONE,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "候補".into(),
                active_range_chars: None,
            })],
        ));
        assert!(search_text_frame(
            &ctx,
            ViewportId::ROOT,
            Modifiers::NONE,
            vec![Event::Text("root".into())],
        ));
        assert!(!search_text_frame(
            &ctx,
            child,
            Modifiers::NONE,
            vec![Event::Text("candidate".into())],
        ));
    }

    fn frame(ctx: &Context, targets: &[(Region, Id)]) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for (region, id) in targets {
                let rect = ui.allocate_space(egui::vec2(100.0, 24.0)).1;
                composite(ui, *region, *id, rect, "Test region");
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn cycle_wraps_both_directions_and_excludes_hidden_regions() {
        let ctx = Context::default();
        let tabs = Id::new("tabs");
        let sidebar = sidebar_id();
        let listing = listing_id(1);
        let targets = [
            (Region::Tabs, tabs),
            (Region::Sidebar, sidebar),
            (Region::Listing(1), listing),
        ];
        frame(&ctx, &targets);
        request_id(&ctx, tabs);
        assert_eq!(
            cycle(&ctx, &[Region::Tabs, Region::Listing(1)], false),
            Some(Region::Listing(1))
        );
        assert_eq!(ctx.memory(|m| m.focused()), Some(listing));
        assert_eq!(
            cycle(&ctx, &[Region::Tabs, Region::Listing(1)], false),
            Some(Region::Tabs)
        );
        assert_eq!(
            cycle(&ctx, &[Region::Tabs, Region::Listing(1)], true),
            Some(Region::Listing(1))
        );
        assert_ne!(ctx.memory(|m| m.focused()), Some(sidebar));
    }

    #[test]
    fn detached_region_registration_expires_after_missing_frames() {
        let ctx = Context::default();
        frame(&ctx, &[(Region::Listing(7), listing_id(7))]);
        frame(&ctx, &[]);
        frame(&ctx, &[]);
        assert!(!request(&ctx, Region::Listing(7)));
        assert_eq!(cycle(&ctx, &[Region::Listing(7)], false), None);
    }
}
