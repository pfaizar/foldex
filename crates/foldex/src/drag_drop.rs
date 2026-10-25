//! File drag destinations are published by the UI and hit-tested in native
//! client coordinates, independently of which pane has keyboard focus.

use egui::{Context, Id, Pos2, Rect, Ui};
use std::{path::PathBuf, sync::Arc};

#[cfg(windows)]
mod native;
#[cfg(windows)]
pub use native::Native;

#[derive(Clone, Default)]
struct Targets(Arc<Vec<Destination>>);

#[derive(Clone)]
struct Destination {
    rect: Rect,
    path: PathBuf,
    accent: [u8; 3],
    pane: bool,
}

pub fn begin_frame(ctx: &Context) {
    ctx.data_mut(|data| data.insert_temp(Id::new("file-drop-targets"), Targets::default()));
}

pub fn target(ui: &Ui, rect: Rect, path: PathBuf) {
    register_target(ui, rect, path, false);
}

pub fn pane_target(ui: &Ui, rect: Rect, path: PathBuf) {
    register_target(ui, rect, path, true);
}

fn register_target(ui: &Ui, rect: Rect, path: PathBuf, pane: bool) {
    let rect = rect.intersect(ui.clip_rect());
    if !ui.is_enabled() || !rect.is_positive() {
        return;
    }
    ui.ctx().data_mut(|data| {
        let targets = data.get_temp_mut_or_default::<Targets>(Id::new("file-drop-targets"));
        // Only visible hit targets are retained, with a fixed per-frame bound.
        if targets.0.len() < 8192 {
            let accent = ui.visuals().selection.stroke.color;
            Arc::make_mut(&mut targets.0).push(Destination {
                rect,
                path,
                accent: [accent.r(), accent.g(), accent.b()],
                pane,
            });
        }
    });
}

pub fn destination(ctx: &Context, position: Pos2) -> Option<PathBuf> {
    destination_at(ctx, position).map(|target| target.path)
}

fn destination_at(ctx: &Context, position: Pos2) -> Option<Destination> {
    ctx.data(|data| {
        data.get_temp::<Targets>(Id::new("file-drop-targets"))?
            .0
            .iter()
            .rev()
            .find(|target| target.rect.contains(position))
            .cloned()
    })
}

#[derive(Debug)]
pub struct DropRequest {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    pub cut: bool,
}

#[cfg(not(windows))]
#[derive(Default)]
pub struct Native;
#[cfg(not(windows))]
impl Native {
    pub fn new(_: isize, _: &Context) -> std::io::Result<Self> {
        Ok(Self)
    }
    pub fn start(&self, _: &[PathBuf]) -> std::io::Result<()> {
        Err(std::io::Error::other(
            "Native file dragging requires Windows",
        ))
    }
    pub fn take_drop(&self) -> Option<DropRequest> {
        None
    }
    pub fn take_changed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_hit_targets_override_their_pane_without_using_keyboard_focus() {
        let ctx = Context::default();
        let output = ctx.run_ui(Default::default(), |ui| {
            begin_frame(&ctx);
            target(
                ui,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(300.0, 300.0)),
                "left".into(),
            );
            target(
                ui,
                Rect::from_min_max(Pos2::new(320.0, 0.0), Pos2::new(620.0, 300.0)),
                "right".into(),
            );
            target(
                ui,
                Rect::from_min_max(Pos2::new(330.0, 30.0), Pos2::new(500.0, 60.0)),
                "right/child".into(),
            );
        });
        output.drop_without_applying_deltas();
        assert_eq!(
            destination(&ctx, Pos2::new(10.0, 10.0)),
            Some("left".into())
        );
        assert_eq!(
            destination(&ctx, Pos2::new(340.0, 40.0)),
            Some("right/child".into())
        );
        assert_eq!(
            destination(&ctx, Pos2::new(340.0, 100.0)),
            Some("right".into())
        );
        assert_eq!(destination(&ctx, Pos2::new(310.0, 100.0)), None);
        begin_frame(&ctx);
        assert_eq!(destination(&ctx, Pos2::new(340.0, 40.0)), None);
    }
}
