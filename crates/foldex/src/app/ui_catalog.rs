//! Offscreen dialog catalog. Each scenario runs the real app frame against a
//! generated fixture folder and renders it through egui-wgpu, without a window
//! or desktop input. `python scripts/ui_catalog.py` runs it and builds the page.
//!
//! Environment: `FOLDEX_UI_CATALOG_OUT` (output folder, required) and
//! `FOLDEX_UI_CATALOG_ONLY` (comma-separated scenario name filter).
use super::tests::fixture_app;
use super::*;
use crate::workspace::NamedWorkspace;
use eframe::{App, egui_wgpu};
use std::path::Path;

const SIZE: egui::Vec2 = vec2(1200.0, 760.0);
const PIXELS_PER_POINT: f32 = 2.0;
/// Context kept around a cropped dialog, in points.
const MARGIN: f32 = 28.0;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    struct ThreadWake(std::thread::Thread);
    impl std::task::Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

impl Gpu {
    fn new() -> Self {
        let egui_wgpu::WgpuSetup::CreateNew(setup) = egui_wgpu::WgpuSetup::without_display_handle()
        else {
            unreachable!()
        };
        let instance = wgpu::Instance::new(setup.instance_descriptor);
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: setup.power_preference,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .expect("a wgpu adapter");
        let (device, queue) =
            block_on(adapter.request_device(&(setup.device_descriptor)(&adapter)))
                .expect("a wgpu device");
        Self { device, queue }
    }
}

/// One fixture app, its context and the renderer that owns its textures.
struct Stage<'a> {
    gpu: &'a Gpu,
    renderer: egui_wgpu::Renderer,
    app: Foldex,
    ctx: egui::Context,
    root: PathBuf,
    frame: usize,
    last: Option<egui::FullOutput>,
}

impl<'a> Stage<'a> {
    fn new(gpu: &'a Gpu) -> Self {
        let (mut app, ctx, root) = fixture_app();
        for name in ["Documents", "Music", "Photos", "Projects"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for name in [
            "Photo 001.jpg",
            "Photo 002.jpg",
            "Photo 003.jpg",
            "Notes.md",
        ] {
            std::fs::write(root.join(name), b"generated fixture").unwrap();
        }
        // The same interface font main() installs; the fixture otherwise uses
        // egui's built-in face, which lacks arrows and differs in metrics.
        if let Ok(font) = std::fs::read("C:/Windows/Fonts/segoeui.ttf") {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "Segoe UI".into(),
                Arc::new(egui::FontData::from_owned(font)),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "Segoe UI".into());
            ctx.set_fonts(fonts);
        }
        app.theme.reduced_motion = true;
        app.theme.apply(&ctx);
        app.pane_mut().navigate(root.clone(), false);
        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            wgpu::TextureFormat::Rgba8Unorm,
            egui_wgpu::RendererOptions::default(),
        );
        let mut stage = Self {
            gpu,
            renderer,
            app,
            ctx,
            root,
            frame: 0,
            last: None,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while stage.app.pane().loading || stage.frame < 8 {
            stage.pump(vec![]);
            assert!(
                Instant::now() < deadline,
                "the catalog fixture did not load"
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        stage
    }

    fn pump(&mut self, events: Vec<egui::Event>) {
        let mut input = egui::RawInput {
            time: Some(self.frame as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SIZE)),
            events,
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(PIXELS_PER_POINT);
        self.app.raw_input_hook(&self.ctx, &mut input);
        let app = &mut self.app;
        let mut output = self
            .ctx
            .run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.gpu.device, &self.gpu.queue, *id, delta);
            }
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        output.textures_delta.clear();
        self.frame += 1;
        self.last = Some(output);
    }

    /// Workers (previews, pane loads) answer on their own threads.
    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.pump(vec![]);
            std::thread::sleep(Duration::from_millis(12));
        }
    }

    fn capture(&mut self, crop: Option<Rect>, path: &Path) {
        let output = self.last.take().expect("a rendered frame");
        let ppp = output.pixels_per_point;
        let jobs = self.ctx.tessellate(output.shapes, ppp);
        let size = [(SIZE.x * ppp).round() as u32, (SIZE.y * ppp).round() as u32];
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: ppp,
        };
        let device = &self.gpu.device;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-catalog"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut commands =
            self.renderer
                .update_buffers(device, &self.gpu.queue, &mut encoder, &jobs, &screen);
        {
            let background = Theme::color(self.app.theme.background);
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ui-catalog"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(background.r()) / 255.0,
                                g: f64::from(background.g()) / 255.0,
                                b: f64::from(background.b()) / 255.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &jobs, &screen);
        }
        let row = (size[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui-catalog-readback"),
            size: u64::from(row * size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(size[1]),
                },
            },
            target.size(),
        );
        commands.push(encoder.finish());
        self.gpu.queue.submit(commands);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |result| result.unwrap());
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the catalog frame to finish");
        let data = slice.get_mapped_range().expect("a mapped readback");
        let mut image = image::RgbaImage::new(size[0], size[1]);
        for (y, line) in data.chunks(row as usize).take(size[1] as usize).enumerate() {
            for x in 0..size[0] as usize {
                let pixel = &line[x * 4..x * 4 + 4];
                image.put_pixel(
                    x as u32,
                    y as u32,
                    image::Rgba([pixel[0], pixel[1], pixel[2], 255]),
                );
            }
        }
        drop(data);
        buffer.unmap();
        let image = match crop {
            Some(rect) => {
                let rect = rect
                    .expand(MARGIN)
                    .intersect(Rect::from_min_size(egui::Pos2::ZERO, SIZE));
                let min = (rect.min.to_vec2() * ppp).round();
                let extent = (rect.size() * ppp).round();
                image::imageops::crop_imm(
                    &image,
                    min.x as u32,
                    min.y as u32,
                    extent.x as u32,
                    extent.y as u32,
                )
                .to_image()
            }
            None => image,
        };
        image.save(path).expect("the catalog image to save");
    }

    fn area(&self, id: Id) -> Option<Rect> {
        self.ctx.memory(|memory| memory.area_rect(id))
    }
}

struct Scenario {
    group: &'static str,
    name: &'static str,
    caption: &'static str,
    run: fn(&mut Stage) -> Option<Rect>,
}

macro_rules! scenario {
    ($group:literal, $name:literal, $caption:literal, $run:expr) => {
        Scenario {
            group: $group,
            name: $name,
            caption: $caption,
            run: $run,
        }
    };
}

fn layout(stage: &Stage, name: &str) -> NamedWorkspace {
    let mut state = stage.app.workspace_state();
    state.clear_navigation_history();
    NamedWorkspace {
        name: name.into(),
        state,
    }
}

fn delete(stage: &mut Stage, count: usize) -> Option<Rect> {
    let paths = (0..count)
        .map(|index| stage.root.join(format!("Photo {:03}.jpg", index + 1)))
        .collect();
    stage
        .app
        .request_delete(paths, crate::pane::DeleteMode::Permanent);
    stage.settle(6);
    stage
        .app
        .catalog_delete_area()
        .and_then(|id| stage.area(id))
}

fn batch(stage: &mut Stage, template: Option<&str>) -> Option<Rect> {
    let paths = (1..=3)
        .map(|index| stage.root.join(format!("Photo {index:03}.jpg")))
        .collect();
    stage.app.action(Action::RenameMany(paths));
    stage.settle(10);
    if let Some(template) = template {
        stage.app.batch.as_mut().unwrap().catalog_template(template);
        stage.settle(20);
    }
    stage.settle(10);
    stage.area(Id::new("batch-rename"))
}

fn recycle(stage: &mut Stage, intent: crate::recycle_bin::Intent) -> Option<Rect> {
    let ctx = stage.ctx.clone();
    stage.app.benchmark_recycle_preview(&ctx);
    stage.settle(6);
    stage.app.recycle_action(intent);
    stage.settle(6);
    stage
        .app
        .catalog_recycle_area()
        .and_then(|id| stage.area(id))
}

fn updates(stage: &mut Stage, phase: crate::updates::CatalogPhase) -> Option<Rect> {
    stage.app.updates.catalog(phase);
    stage.settle(6);
    stage.area(Id::new("updates-dialog"))
}

fn recording(stage: &mut Stage, keys: &[&str]) -> Option<Rect> {
    stage.app.hotkeys_open = true;
    stage.settle(4);
    stage.app.hotkeys.catalog_recording(keys);
    stage.settle(6);
    stage.area(Id::new("hotkey-recording"))
}

fn scenarios() -> Vec<Scenario> {
    use crate::recycle_bin::Intent;
    use crate::updates::CatalogPhase;
    vec![
        scenario!(
            "Window",
            "window",
            "Main window with a selection",
            |stage| {
                stage.app.pane_mut().catalog_select(5, 6);
                stage.settle(4);
                None
            }
        ),
        scenario!("Delete permanently", "delete-1", "1 item", |stage| delete(
            stage, 1
        )),
        scenario!("Delete permanently", "delete-3", "3 items", |stage| delete(
            stage, 3
        )),
        scenario!("Delete permanently", "delete-7", "7 items", |stage| delete(
            stage, 7
        )),
        scenario!(
            "Recycle Bin",
            "recycle-empty",
            "Empty Recycle Bin",
            |stage| recycle(stage, Intent::Empty)
        ),
        scenario!(
            "Recycle Bin",
            "recycle-restore",
            "Restore all items",
            |stage| recycle(stage, Intent::RestoreAll)
        ),
        scenario!(
            "Rename",
            "rename-extension",
            "Change file extension",
            |stage| {
                let origin = stage.app.pane().id;
                let path = stage.root.join("Notes.md");
                stage.app.action(Action::ConfirmRenameExtension {
                    origin,
                    path,
                    name: "Notes.txt".into(),
                });
                stage.settle(6);
                stage.area(Id::new("rename-extension-confirm"))
            }
        ),
        scenario!("File operations", "operations-empty", "Empty", |stage| {
            stage.app.operations_open = true;
            stage.settle(6);
            stage.area(Id::new("file-operations"))
        }),
        scenario!(
            "File operations",
            "operations",
            "Running, queued, failed, complete and cancelled",
            |stage| {
                stage.app.catalog_operations();
                stage.app.operations_open = true;
                stage.settle(6);
                stage.area(Id::new("file-operations"))
            }
        ),
        scenario!(
            "Saved layouts",
            "layouts-empty",
            "Saved layouts, empty",
            |stage| {
                stage.app.load_layout_open = true;
                stage.settle(6);
                stage.area(Id::new("saved-layouts"))
            }
        ),
        scenario!(
            "Saved layouts",
            "layouts",
            "Saved layouts, populated",
            |stage| {
                for name in ["Photo sorting", "Release checklist", "Two-pane review"] {
                    let item = layout(stage, name);
                    stage.app.saved_layouts.push(item);
                }
                stage.app.load_layout_open = true;
                stage.settle(6);
                stage.area(Id::new("saved-layouts"))
            }
        ),
        scenario!(
            "Saved layouts",
            "save-layout-empty",
            "Save layout, no name yet",
            |stage| {
                stage.app.save_layout_name = Some(String::new());
                stage.settle(6);
                stage.area(Id::new("save-layout"))
            }
        ),
        scenario!(
            "Saved layouts",
            "save-layout",
            "Save layout, named",
            |stage| {
                stage.app.save_layout_name = Some("Photo sorting".into());
                stage.settle(6);
                stage.area(Id::new("save-layout"))
            }
        ),
        scenario!(
            "Rename files",
            "batch-default",
            "Default template",
            |stage| batch(stage, None)
        ),
        scenario!("Rename files", "batch-changes", "With changes", |stage| {
            batch(stage, Some("{stem} Holiday {id}"))
        }),
        scenario!(
            "Rename files",
            "batch-conflicts",
            "With conflicts",
            |stage| batch(stage, Some("Holiday"))
        ),
        scenario!("Search conditions", "search", "Empty", |stage| {
            stage.app.open_search_editor();
            stage.settle(6);
            stage.area(Id::new("search-conditions"))
        }),
        scenario!(
            "Search conditions",
            "search-error",
            "Validation error",
            |stage| {
                stage.app.open_search_editor();
                stage.settle(2);
                stage.app.catalog_search_error();
                stage.settle(6);
                stage.area(Id::new("search-conditions"))
            }
        ),
        scenario!("Hotkeys", "hotkey-remove", "Remove shortcut", |stage| {
            stage.app.hotkeys_open = true;
            stage.settle(4);
            stage.app.hotkeys.catalog_confirmation(false);
            stage.settle(6);
            stage.area(Id::new("hotkey-confirmation"))
        }),
        scenario!("Hotkeys", "hotkey-reset", "Reset all hotkeys", |stage| {
            stage.app.hotkeys_open = true;
            stage.settle(4);
            stage.app.hotkeys.catalog_confirmation(true);
            stage.settle(6);
            stage.area(Id::new("hotkey-confirmation"))
        }),
        scenario!(
            "Hotkeys",
            "hotkey-waiting",
            "Assign shortcut, waiting",
            |stage| recording(stage, &[])
        ),
        scenario!(
            "Hotkeys",
            "hotkey-captured",
            "Assign shortcut, captured",
            |stage| recording(stage, &["Ctrl+Alt+J"])
        ),
        scenario!(
            "Hotkeys",
            "hotkey-conflict",
            "Assign shortcut, conflict",
            |stage| recording(stage, &["Ctrl+C"])
        ),
        scenario!(
            "Application updates",
            "updates-unavailable",
            "Unavailable",
            |stage| updates(stage, CatalogPhase::Unavailable)
        ),
        scenario!(
            "Application updates",
            "updates-checking",
            "Checking",
            |stage| updates(stage, CatalogPhase::Checking)
        ),
        scenario!(
            "Application updates",
            "updates-current",
            "Up to date",
            |stage| updates(stage, CatalogPhase::Current)
        ),
        scenario!(
            "Application updates",
            "updates-available",
            "Update available (installed copy)",
            |stage| updates(stage, CatalogPhase::Available { managed: true })
        ),
        scenario!(
            "Application updates",
            "updates-portable",
            "Update available (portable copy)",
            |stage| updates(stage, CatalogPhase::Available { managed: false })
        ),
        scenario!(
            "Application updates",
            "updates-downloading",
            "Downloading",
            |stage| updates(stage, CatalogPhase::Downloading)
        ),
        scenario!("Application updates", "updates-error", "Error", |stage| {
            updates(stage, CatalogPhase::Error)
        }),
        scenario!(
            "Application updates",
            "updates-installing",
            "Installing, exit pending",
            |stage| updates(stage, CatalogPhase::Installing)
        ),
        scenario!(
            "Application updates",
            "updates-notification",
            "Corner notification",
            |stage| {
                stage.app.updates.catalog(CatalogPhase::Notification);
                stage.settle(6);
                stage.area(Id::new("update-notification"))
            }
        ),
        scenario!(
            "Keyboard Navigation",
            "keyboard-help",
            "Full help",
            |stage| {
                stage.app.keyboard_help = true;
                stage.settle(6);
                stage.area(Id::new("keyboard-navigation-help"))
            }
        ),
        scenario!("Performance", "performance", "Ctrl+Alt+P", |stage| {
            crate::friendly::set_developer_mode(true);
            stage.app.show_stats = true;
            stage.settle(6);
            let area = stage.area(Id::new("performance-dialog"));
            crate::friendly::set_developer_mode(false);
            area
        }),
        scenario!("Options", "accent-color", "Accent Color editor", |stage| {
            let ctx = stage.ctx.clone();
            stage.app.open_options(&ctx);
            stage.settle(4);
            stage.app.options_popup.catalog_accent(&mut stage.app.theme);
            stage.settle(6);
            stage.area(Id::new("appearance-options-popup").with("color-editor"))
        }),
    ]
}

#[test]
#[ignore = "renders PNG files; run through scripts/ui_catalog.py"]
fn ui_catalog() {
    let Ok(out) = std::env::var("FOLDEX_UI_CATALOG_OUT") else {
        panic!("set FOLDEX_UI_CATALOG_OUT");
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let only: Vec<String> = std::env::var("FOLDEX_UI_CATALOG_ONLY")
        .map(|value| value.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let gpu = Gpu::new();
    let mut manifest = Vec::new();
    for scenario in scenarios() {
        if !only.is_empty()
            && !only
                .iter()
                .any(|name| scenario.name.starts_with(name.as_str()))
        {
            continue;
        }
        let mut stage = Stage::new(&gpu);
        let crop = (scenario.run)(&mut stage);
        let file = format!("{}.png", scenario.name);
        stage.capture(crop, &out.join(&file));
        manifest.push(serde_json::json!({
            "group": scenario.group,
            "name": scenario.name,
            "caption": scenario.caption,
            "file": file,
        }));
        println!("captured {}", scenario.name);
    }
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "shots": manifest })).unwrap(),
    )
    .unwrap();
}
