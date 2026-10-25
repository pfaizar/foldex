//! Note that this file contains code very similar to [`super::glow_integration`].
//! When making changes to one you often also want to apply it to the other.
//!
//! This is also very complex code, and not very pretty.
//! There is a bunch of improvements we could do,
//! like removing a bunch of `unwraps`.

use core::{cell::RefCell, num::NonZeroU32};
use std::{rc::Rc, sync::Arc, time::Instant};

use egui_winit::ActionRequested;
use parking_lot::Mutex;
use raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};
use winit::{
    dpi::PhysicalSize,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

use ahash::HashMap;
use egui::{
    DeferredViewportUiCallback, FullOutput, ImmediateViewport, OrderedViewportIdMap, TexturesDelta,
    ViewportBuilder, ViewportClass, ViewportId, ViewportIdPair, ViewportIdSet, ViewportInfo,
    ViewportOutput,
};
#[cfg(feature = "accesskit")]
use egui_winit::accesskit_winit;
use log::warn;
use winit_integration::UserEvent;

use crate::{
    App, AppCreator, CreationContext, NativeOptions, Result, Storage,
    native::{
        epi_integration::EpiIntegration,
        winit_integration::{EventResult, sleep_if_invisible_or_minimized},
    },
};

use super::{epi_integration, event_loop_context, winit_integration, winit_integration::WinitApp};

// ----------------------------------------------------------------------------
// Types:

pub struct WgpuWinitApp<'app> {
    repaint_proxy: Arc<Mutex<EventLoopProxy<UserEvent>>>,
    app_name: String,
    native_options: NativeOptions,

    /// Set at initialization, then taken and set to `None` in `init_run_state`.
    app_creator: Option<AppCreator<'app>>,

    /// Set when we are actually up and running.
    running: Option<WgpuWinitRunning<'app>>,

    /// An optional pre-existing egui context. If `Some`, it is used instead of
    /// creating a new one via [`winit_integration::create_egui_context`]. Taken during initialization.
    egui_ctx: Option<egui::Context>,
}

/// State that is initialized when the application is first starts running via
/// a Resumed event. On Android this ensures that any graphics state is only
/// initialized once the application has an associated `SurfaceView`.
struct WgpuWinitRunning<'app> {
    integration: EpiIntegration,

    /// The users application.
    app: Box<dyn 'app + App>,

    /// Wrapped in an `Rc<RefCell<…>>` so it can be re-entrantly shared via a weak-pointer.
    shared: Rc<RefCell<SharedState>>,
}

/// Everything needed by the immediate viewport renderer.\
///
/// This is shared by all viewports.
///
/// Wrapped in an `Rc<RefCell<…>>` so it can be re-entrantly shared via a weak-pointer.
pub struct SharedState {
    egui_ctx: egui::Context,
    windows_no_redirection_bitmap: bool,
    viewports: Viewports,
    painter: egui_wgpu::winit::Painter,
    viewport_from_window: HashMap<WindowId, ViewportId>,
    focused_viewport: Option<ViewportId>,
    resized_viewport: Option<ViewportId>,
    /// Texture IDs and the font atlas belong to the context, not a window.
    /// Preserve upload order even when a producing viewport is hidden.
    pending_deltas: TexturesDelta,
}

impl Drop for SharedState {
    fn drop(&mut self) {
        self.pending_deltas.clear();
    }
}

pub type Viewports = egui::OrderedViewportIdMap<Viewport>;

pub struct Viewport {
    ids: ViewportIdPair,
    class: ViewportClass,
    builder: ViewportBuilder,
    deferred_commands: Vec<egui::viewport::ViewportCommand>,
    info: ViewportInfo,
    actions_requested: Vec<ActionRequested>,

    /// `None` for sync viewports.
    viewport_ui_cb: Option<Arc<DeferredViewportUiCallback>>,

    /// Window surface state that's initialized when the app starts running via a Resumed event
    /// and on Android will also be destroyed if the application is paused.
    window: Option<Arc<Window>>,

    /// `window` and `egui_winit` are initialized together.
    egui_winit: Option<egui_winit::State>,

    /// Last Windows frame snapshot applied to the painter. Resize events can
    /// be older than the HWND size used by egui, so they do not update this.
    configured_frame_size: Option<PhysicalSize<u32>>,
}

/// Take input and retain the exact Windows client-pixel extent used for layout.
/// No HWND size query after the UI callback may change this frame's extent.
#[cfg(target_os = "windows")]
fn windows_window_owner(window: &Window) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn windows_client_rect(window: &Window) -> Option<[i32; 4]> {
    let origin = window.inner_position().ok()?;
    let size = window.inner_size();
    Some([
        origin.x,
        origin.y,
        origin.x.checked_add(i32::try_from(size.width).ok()?)?,
        origin.y.checked_add(i32::try_from(size.height).ok()?)?,
    ])
}

fn take_frame_input(
    egui_winit: &mut egui_winit::State,
    window: &Window,
    viewport_info: &mut ViewportInfo,
) -> (egui::RawInput, Option<PhysicalSize<u32>>) {
    let input = egui_winit.take_egui_input(window);
    #[cfg(target_os = "windows")]
    {
        let mut input = input;
        // WM_NCCALCSIZE still exposes the old HWND geometry. A scoped native
        // pre-paint supplies the final proposed client rectangle instead.
        let proposed_rect =
            windows_window_owner(window).and_then(super::windows_resize::take_proposed_rect);
        let frame_size = proposed_rect.map_or_else(
            || window.inner_size(),
            |rect| PhysicalSize::new((rect[2] - rect[0]) as u32, (rect[3] - rect[1]) as u32),
        );
        let native_pixels_per_point = input
            .viewport()
            .native_pixels_per_point
            .unwrap_or_else(|| window.scale_factor() as f32);
        let pixels_per_point = egui_winit.egui_ctx().zoom_factor() * native_pixels_per_point;
        // take_egui_input queries the HWND internally. Replace its screen rect
        // with our retained snapshot instead of pairing two independent reads.
        input.screen_rect = frame_screen_rect(frame_size, pixels_per_point);
        if let Some(screen_rect) = input.screen_rect {
            if let Some(rect) = proposed_rect {
                viewport_info.inner_rect = Some(egui::Rect::from_min_size(
                    egui::pos2(rect[0] as f32, rect[1] as f32) / pixels_per_point,
                    screen_rect.size(),
                ));
            } else if let Some(inner_rect) = &mut viewport_info.inner_rect {
                inner_rect.max = inner_rect.min + screen_rect.size();
            }
        } else {
            viewport_info.inner_rect = None;
        }
        viewport_info.native_pixels_per_point = Some(native_pixels_per_point);
        (input, Some(frame_size))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = viewport_info;
        (input, None)
    }
}

#[cfg(any(target_os = "windows", test))]
fn frame_screen_rect(size: PhysicalSize<u32>, pixels_per_point: f32) -> Option<egui::Rect> {
    (size.width > 0 && size.height > 0).then(|| {
        egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.width as f32, size.height as f32) / pixels_per_point,
        )
    })
}

fn frame_is_paintable(frame_size: Option<PhysicalSize<u32>>) -> bool {
    frame_size.is_none_or(|size| size.width > 0 && size.height > 0)
}

/// A pending egui zoom can replace the raw screen rect with its previous
/// canvas. Size the framebuffer for the UI that actually completed, while
/// retaining the native zero-size guard and other platforms' resize behavior.
fn completed_frame_size(
    raw_size: Option<PhysicalSize<u32>>,
    viewport_size: egui::Vec2,
    pixels_per_point: f32,
) -> Option<PhysicalSize<u32>> {
    raw_size.map(|size| {
        if !frame_is_paintable(Some(size)) {
            return size;
        }
        let pixels = viewport_size * pixels_per_point;
        PhysicalSize::new(pixels.x.round() as u32, pixels.y.round() as u32)
    })
}

fn requested_visibility(
    viewport_output: &OrderedViewportIdMap<ViewportOutput>,
    viewport_id: ViewportId,
) -> Option<bool> {
    viewport_output.get(&viewport_id).and_then(|output| {
        output.commands.iter().rev().find_map(|command| {
            if let egui::ViewportCommand::Visible(visible) = command {
                Some(*visible)
            } else {
                None
            }
        })
    })
}

fn paint_before_visibility_commands(
    native_visible: Option<bool>,
    requested: Option<bool>,
    initial_root_frame: bool,
) -> bool {
    requested.unwrap_or(initial_root_frame || native_visible != Some(false))
}

/// Configure only when the retained frame snapshot differs from the surface.
/// `None` keeps other platforms' event-driven surface sizing unchanged.
fn configure_surface_for_frame(
    frame_size: Option<PhysicalSize<u32>>,
    configured_size: &mut Option<PhysicalSize<u32>>,
    configure: impl FnOnce(NonZeroU32, NonZeroU32),
) -> bool {
    let Some(size) = frame_size else {
        return true;
    };
    let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
    else {
        return false;
    };
    if *configured_size != Some(size) {
        configure(width, height);
        *configured_size = Some(size);
    }
    true
}

// ----------------------------------------------------------------------------

impl<'app> WgpuWinitApp<'app> {
    pub fn new(
        event_loop: &EventLoop<UserEvent>,
        app_name: &str,
        native_options: NativeOptions,
        egui_ctx: Option<egui::Context>,
        app_creator: AppCreator<'app>,
    ) -> Self {
        profiling::function_scope!();

        #[cfg(feature = "__screenshot")]
        assert!(
            std::env::var("EFRAME_SCREENSHOT_TO").is_err(),
            "EFRAME_SCREENSHOT_TO not yet implemented for wgpu backend"
        );

        Self {
            repaint_proxy: Arc::new(Mutex::new(event_loop.create_proxy())),
            app_name: app_name.to_owned(),
            native_options,
            running: None,
            app_creator: Some(app_creator),
            egui_ctx,
        }
    }

    /// Create a window for all viewports lacking one.
    fn initialized_all_windows(&mut self, event_loop: &ActiveEventLoop) {
        let Some(running) = &mut self.running else {
            return;
        };
        let mut shared = running.shared.borrow_mut();
        let SharedState {
            viewports,
            painter,
            viewport_from_window,
            windows_no_redirection_bitmap,
            ..
        } = &mut *shared;

        for viewport in viewports.values_mut() {
            viewport.initialize_window(
                event_loop,
                &running.integration.egui_ctx,
                viewport_from_window,
                painter,
                *windows_no_redirection_bitmap,
            );
        }
    }

    #[cfg(target_os = "android")]
    fn recreate_window(&self, event_loop: &ActiveEventLoop, running: &WgpuWinitRunning<'app>) {
        let SharedState {
            egui_ctx,
            viewports,
            viewport_from_window,
            painter,
            windows_no_redirection_bitmap,
            ..
        } = &mut *running.shared.borrow_mut();

        initialize_or_update_viewport(
            viewports,
            ViewportIdPair::ROOT,
            ViewportClass::Root,
            self.native_options.viewport.clone(),
            None,
            painter,
        )
        .initialize_window(
            event_loop,
            egui_ctx,
            viewport_from_window,
            painter,
            *windows_no_redirection_bitmap,
        );
    }

    #[cfg(target_os = "android")]
    fn drop_window(&mut self) -> Result<(), egui_wgpu::WgpuError> {
        if let Some(running) = &mut self.running {
            let mut shared = running.shared.borrow_mut();
            shared.viewports.remove(&ViewportId::ROOT);
            for viewport in shared.viewports.values_mut() {
                viewport.configured_frame_size = None;
            }
            pollster::block_on(shared.painter.set_window(ViewportId::ROOT, None))?;
        }
        Ok(())
    }

    fn init_run_state(
        &mut self,
        egui_ctx: egui::Context,
        event_loop: &ActiveEventLoop,
        storage: Option<Box<dyn Storage>>,
        window: Window,
        builder: ViewportBuilder,
    ) -> crate::Result<&mut WgpuWinitRunning<'app>> {
        profiling::function_scope!();
        // Inject the display handle into the wgpu setup so that wgpu can create
        // surfaces on platforms that require it (e.g. GLES on Wayland).
        let mut wgpu_options = self.native_options.wgpu_options.clone();
        if let egui_wgpu::WgpuSetup::CreateNew(ref mut create_new) = wgpu_options.wgpu_setup
            && create_new.display_handle.is_none()
        {
            create_new.display_handle = Some(Box::new(event_loop.owned_display_handle()));
        }
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(
            egui_ctx.clone(),
            wgpu_options,
            self.native_options.viewport.transparent.unwrap_or(false),
            egui_wgpu::RendererOptions {
                msaa_samples: self.native_options.multisampling as _,
                depth_stencil_format: egui_wgpu::depth_format_from_bits(
                    self.native_options.depth_buffer,
                    self.native_options.stencil_buffer,
                ),
                dithering: self.native_options.dithering,
                ..Default::default()
            },
        ));

        let mut viewport_info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut viewport_info, &egui_ctx, &window, true);

        {
            // Tell egui right away about native_pixels_per_point etc,
            // so that the app knows about it during app creation:
            let pixels_per_point = egui_winit::pixels_per_point(&egui_ctx, &window);

            egui_ctx.input_mut(|i| {
                i.raw
                    .viewports
                    .insert(ViewportId::ROOT, viewport_info.clone());
                i.pixels_per_point = pixels_per_point;
            });
        }

        let window = Arc::new(window);

        {
            profiling::scope!("set_window");
            pollster::block_on(painter.set_window(ViewportId::ROOT, Some(Arc::clone(&window))))?;
        }

        let wgpu_render_state = painter.render_state();

        let integration = EpiIntegration::new(
            egui_ctx.clone(),
            &window,
            &self.app_name,
            &self.native_options,
            storage,
            #[cfg(feature = "glow")]
            None,
            #[cfg(feature = "glow")]
            None,
            wgpu_render_state.clone(),
        );

        {
            let event_loop_proxy = Arc::clone(&self.repaint_proxy);

            egui_ctx.set_request_repaint_callback(move |info| {
                log::trace!("request_repaint_callback: {info:?}");
                let when = Instant::now() + info.delay;
                let cumulative_pass_nr = info.current_cumulative_pass_nr;

                event_loop_proxy
                    .lock()
                    .send_event(UserEvent::RequestRepaint {
                        when,
                        cumulative_pass_nr,
                        viewport_id: info.viewport_id,
                    })
                    .ok();
            });
        }

        #[allow(clippy::allow_attributes, unused_mut)] // used for accesskit
        let mut egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            event_loop.system_theme(),
            painter.max_texture_side(),
        );

        #[cfg(feature = "accesskit")]
        {
            let event_loop_proxy = self.repaint_proxy.lock().clone();
            egui_winit.init_accesskit(event_loop, &window, event_loop_proxy);
        }

        let app_creator = core::mem::take(&mut self.app_creator)
            .expect("Single-use AppCreator has unexpectedly already been taken");

        crate::maybe_attach_inspection_plugin(&egui_ctx, Some(self.app_name.clone()));

        let cc = CreationContext {
            egui_ctx: egui_ctx.clone(),
            integration_info: integration.frame.info().clone(),
            storage: integration.frame.storage(),
            #[cfg(feature = "glow")]
            gl: None,
            #[cfg(feature = "glow")]
            get_proc_address: None,
            wgpu_render_state,
            window: Some(Arc::clone(&window)),
            raw_display_handle: window.display_handle().map(|h| h.as_raw()),
            raw_window_handle: window.window_handle().map(|h| h.as_raw()),
        };
        let app = {
            profiling::scope!("user_app_creator");
            app_creator(&cc).map_err(crate::Error::AppCreation)?
        };

        let mut viewport_from_window = HashMap::default();
        viewport_from_window.insert(window.id(), ViewportId::ROOT);

        let mut viewports = Viewports::default();
        viewports.insert(
            ViewportId::ROOT,
            Viewport {
                ids: ViewportIdPair::ROOT,
                class: ViewportClass::Root,
                builder,
                deferred_commands: vec![],
                info: viewport_info,
                actions_requested: Default::default(),
                viewport_ui_cb: None,
                window: Some(window),
                egui_winit: Some(egui_winit),
                configured_frame_size: None,
            },
        );

        let shared = Rc::new(RefCell::new(SharedState {
            egui_ctx,
            windows_no_redirection_bitmap: self.native_options.windows_no_redirection_bitmap,
            viewport_from_window,
            viewports,
            painter,
            focused_viewport: Some(ViewportId::ROOT),
            resized_viewport: None,
            pending_deltas: Default::default(),
        }));

        {
            // Create a weak pointer so that we don't keep state alive for too long.
            let shared = Rc::downgrade(&shared);
            let beginning = integration.beginning;

            egui::Context::set_immediate_viewport_renderer(move |_egui_ctx, immediate_viewport| {
                if let Some(shared) = shared.upgrade() {
                    render_immediate_viewport(beginning, &shared, immediate_viewport);
                } else {
                    log::warn!("render_sync_callback called after window closed");
                }
            });
        }

        Ok(self.running.insert(WgpuWinitRunning {
            integration,
            app,
            shared,
        }))
    }
}

impl WinitApp for WgpuWinitApp<'_> {
    fn egui_ctx(&self) -> Option<&egui::Context> {
        self.running.as_ref().map(|r| &r.integration.egui_ctx)
    }

    fn window(&self, window_id: WindowId) -> Option<Arc<Window>> {
        self.running
            .as_ref()
            .and_then(|r| {
                let shared = r.shared.borrow();
                let id = shared.viewport_from_window.get(&window_id)?;
                shared.viewports.get(id).map(|v| v.window.clone())
            })
            .flatten()
    }

    fn window_id_from_viewport_id(&self, id: ViewportId) -> Option<WindowId> {
        Some(
            self.running
                .as_ref()?
                .shared
                .borrow()
                .viewports
                .get(&id)?
                .window
                .as_ref()?
                .id(),
        )
    }

    fn save(&mut self) {
        log::debug!("WinitApp::save called");
        if let Some(running) = self.running.as_mut() {
            running.save();
        }
    }

    fn save_and_destroy(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.save_and_destroy();
        }
    }

    fn run_ui_and_paint(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
    ) -> Result<EventResult> {
        #[cfg(target_os = "windows")]
        let _render_guard = super::windows_resize::RenderGuard::new();
        self.initialized_all_windows(event_loop);

        if let Some(running) = &mut self.running {
            running.run_ui_and_paint(window_id, event_loop)
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) -> crate::Result<EventResult> {
        log::debug!("Event::Resumed");

        let running = if let Some(running) = &self.running {
            #[cfg(target_os = "android")]
            self.recreate_window(event_loop, running);
            running
        } else {
            let storage = if let Some(file) = &self.native_options.persistence_path {
                epi_integration::create_storage_with_file(file)
            } else {
                epi_integration::create_storage(
                    self.native_options
                        .viewport
                        .app_id
                        .as_ref()
                        .unwrap_or(&self.app_name),
                )
            };
            let egui_ctx = self
                .egui_ctx
                .take()
                .unwrap_or_else(|| winit_integration::create_egui_context(storage.as_deref()));
            let (window, builder) = create_window(
                &egui_ctx,
                event_loop,
                storage.as_deref(),
                &mut self.native_options,
            )?;
            self.init_run_state(egui_ctx, event_loop, storage, window, builder)?
        };

        let viewport = &running.shared.borrow().viewports[&ViewportId::ROOT];
        if let Some(window) = &viewport.window {
            Ok(EventResult::RepaintNow(window.id()))
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn suspended(&mut self, _: &ActiveEventLoop) -> crate::Result<EventResult> {
        #[cfg(target_os = "windows")]
        if let Some(running) = &mut self.running {
            for viewport in running.shared.borrow_mut().viewports.values_mut() {
                viewport.configured_frame_size = None;
            }
        }
        #[cfg(target_os = "android")]
        self.drop_window()?;
        Ok(EventResult::Save)
    }

    fn device_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) -> crate::Result<EventResult> {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event
            && let Some(running) = &mut self.running
        {
            let mut shared = running.shared.borrow_mut();
            if let Some(viewport) = shared
                .focused_viewport
                .and_then(|viewport| shared.viewports.get_mut(&viewport))
                && let Some(window) = viewport.window.as_ref()
            {
                if !window.has_focus()
                    && !viewport
                        .egui_winit
                        .as_ref()
                        .map(|state| state.is_any_pointer_button_down())
                        .unwrap_or(false)
                {
                    return Ok(EventResult::Wait);
                }

                if let Some(egui_winit) = viewport.egui_winit.as_mut()
                    && egui_winit.on_mouse_motion(delta)
                {
                    return Ok(EventResult::RepaintNext(window.id()));
                }
            }
        }

        Ok(EventResult::Wait)
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: winit::event::WindowEvent,
    ) -> crate::Result<EventResult> {
        self.initialized_all_windows(event_loop);

        if let Some(running) = &mut self.running {
            Ok(running.on_window_event(window_id, &event))
        } else {
            // running is removed to get ready for exiting
            Ok(EventResult::Exit)
        }
    }

    #[cfg(feature = "accesskit")]
    fn on_accesskit_event(&mut self, event: accesskit_winit::Event) -> crate::Result<EventResult> {
        if let Some(running) = &mut self.running {
            let mut shared_lock = running.shared.borrow_mut();
            let SharedState {
                viewport_from_window,
                viewports,
                ..
            } = &mut *shared_lock;
            if let Some(viewport) = viewport_from_window
                .get(&event.window_id)
                .and_then(|id| viewports.get_mut(id))
                && let Some(egui_winit) = &mut viewport.egui_winit
            {
                return Ok(winit_integration::on_accesskit_window_event(
                    egui_winit,
                    event.window_id,
                    &event.window_event,
                ));
            }
        }

        Ok(EventResult::Wait)
    }
}

impl WgpuWinitRunning<'_> {
    /// Saves the application state
    fn save(&mut self) {
        let shared = self.shared.borrow();
        // This is done because of the "save on suspend" logic on Android. Once the application is suspended, there is no window associated to it.
        let window = if let Some(Viewport { window, .. }) = shared.viewports.get(&ViewportId::ROOT)
        {
            window.as_deref()
        } else {
            None
        };
        self.integration.save(self.app.as_mut(), window);
    }

    fn save_and_destroy(&mut self) {
        profiling::function_scope!();

        self.save();

        #[cfg(feature = "glow")]
        self.app.on_exit(None);

        #[cfg(not(feature = "glow"))]
        self.app.on_exit();

        let mut shared = self.shared.borrow_mut();
        shared.painter.destroy();
    }

    /// This is called both for the root viewport, and all deferred viewports
    fn run_ui_and_paint(
        &mut self,
        window_id: WindowId,
        event_loop: &ActiveEventLoop,
    ) -> Result<EventResult> {
        profiling::function_scope!();

        let Some(viewport_id) = self
            .shared
            .borrow()
            .viewport_from_window
            .get(&window_id)
            .copied()
        else {
            return Ok(EventResult::Wait);
        };

        profiling::finish_frame!();

        let Self {
            app,
            integration,
            shared,
        } = self;

        let mut frame_timer = crate::stopwatch::Stopwatch::new();
        frame_timer.start();

        let (viewport_ui_cb, raw_input, frame_size, is_visible, show_ui) = {
            profiling::scope!("Prepare");
            let mut shared_lock = shared.borrow_mut();

            let SharedState {
                viewports, painter, ..
            } = &mut *shared_lock;

            if viewport_id != ViewportId::ROOT {
                let Some(viewport) = viewports.get(&viewport_id) else {
                    return Ok(EventResult::Wait);
                };

                if viewport.viewport_ui_cb.is_none() {
                    // This will only happen if this is an immediate viewport.
                    // That means that the viewport cannot be rendered by itself and needs his parent to be rendered.
                    if let Some(viewport) = viewports.get(&viewport.ids.parent)
                        && let Some(window) = viewport.window.as_ref()
                    {
                        return Ok(EventResult::RepaintNext(window.id()));
                    }
                    return Ok(EventResult::Wait);
                }
            }

            let Some(viewport) = viewports.get_mut(&viewport_id) else {
                return Ok(EventResult::Wait);
            };

            let Viewport {
                viewport_ui_cb,
                window,
                egui_winit,
                info,
                ..
            } = viewport;

            let viewport_ui_cb = viewport_ui_cb.clone();

            let Some(window) = window else {
                return Ok(EventResult::Wait);
            };
            egui_winit::update_viewport_info(info, &integration.egui_ctx, window, false);

            let is_visible = info.visible().unwrap_or(true);

            #[cfg(not(target_os = "windows"))]
            {
                profiling::scope!("set_window");
                pollster::block_on(painter.set_window(viewport_id, Some(Arc::clone(window))))?;
            }

            let Some(egui_winit) = egui_winit.as_mut() else {
                return Ok(EventResult::Wait);
            };
            let (mut raw_input, frame_size) = take_frame_input(egui_winit, window, info);

            // UI eligibility is distinct from GPU visibility: an intentionally
            // hidden secondary root may need its UI to queue Visible(true),
            // and a hidden parent may host a visible immediate child.
            let show_ui = is_visible
                || is_viewport_or_descendant_visible(viewports, viewport_id)
                || (viewport_id == ViewportId::ROOT && app.run_ui_when_hidden());

            integration.pre_update();

            raw_input.time = Some(integration.beginning.elapsed().as_secs_f64());
            raw_input.viewports = viewports
                .iter()
                .map(|(id, viewport)| (*id, viewport.info.clone()))
                .collect();

            painter.handle_screenshots(&mut raw_input.events);

            (viewport_ui_cb, raw_input, frame_size, is_visible, show_ui)
        };

        if !show_ui {
            // Nothing will be shown, so we run no egui pass at all.
            // That way all ui state is left untouched, and is still there
            // when this viewport becomes visible again.
            let is_root_viewport = viewport_ui_cb.is_none();
            if is_root_viewport {
                // The app logic keeps ticking, so it can e.g. ask to be shown again:
                let egui::LogicOutput {
                    platform_output,
                    viewport_commands,
                } = integration.update_logic_only(app.as_mut(), raw_input);

                let mut shared_mut = shared.borrow_mut();
                let SharedState { viewports, .. } = &mut *shared_mut;

                if let Some(viewport) = viewports.get_mut(&viewport_id) {
                    viewport.info.events.clear(); // they should have been processed
                    if let Viewport {
                        window: Some(window),
                        egui_winit: Some(egui_winit),
                        ..
                    } = viewport
                    {
                        egui_winit.handle_platform_output_with_event_loop(
                            window,
                            event_loop,
                            platform_output,
                        );
                    }
                }

                for (id, commands) in viewport_commands {
                    if let Some(viewport) = viewports.get_mut(&id) {
                        viewport.process_commands(&integration.egui_ctx, commands);
                    }
                }
            }

            sleep_if_invisible_or_minimized(
                shared
                    .borrow()
                    .viewports
                    .get(&viewport_id)
                    .and_then(|viewport| viewport.window.as_deref()),
            );

            return Ok(if integration.should_close() {
                EventResult::CloseRequested
            } else {
                EventResult::Wait
            });
        }

        // ------------------------------------------------------------

        // Runs the update, which could call immediate viewports,
        // so make sure we hold no locks here!
        let full_output = integration.update(app.as_mut(), viewport_ui_cb.as_deref(), raw_input);
        let frame_size = completed_frame_size(
            frame_size,
            integration
                .egui_ctx
                .input_for(viewport_id, |input| input.viewport_rect().size()),
            full_output.pixels_per_point,
        );

        // ------------------------------------------------------------

        let mut shared_mut = shared.borrow_mut();

        let SharedState {
            egui_ctx,
            viewports,
            painter,
            viewport_from_window,
            pending_deltas,
            ..
        } = &mut *shared_mut;

        let FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            viewport_output,
        } = full_output;

        pending_deltas.append(textures_delta);

        remove_viewports_not_in(viewports, painter, viewport_from_window, &viewport_output);

        let Some(viewport) = viewports.get_mut(&viewport_id) else {
            return Ok(EventResult::Wait);
        };

        viewport.info.events.clear(); // they should have been processed

        let Viewport {
            window: Some(window),
            egui_winit: Some(egui_winit),
            configured_frame_size,
            ..
        } = viewport
        else {
            return Ok(EventResult::Wait);
        };

        egui_winit.handle_platform_output_with_event_loop(window, event_loop, platform_output);

        let visible_command = requested_visibility(&viewport_output, viewport_id);
        if cfg!(target_os = "windows")
            && viewport_id == ViewportId::ROOT
            && visible_command == Some(false)
        {
            integration.cancel_first_frame();
        }
        let will_be_visible = is_visible
            && (!cfg!(target_os = "windows")
                || paint_before_visibility_commands(
                    window.is_visible(),
                    visible_command,
                    viewport_id == ViewportId::ROOT && integration.needs_first_frame(),
                ));

        let vsync_secs = if will_be_visible && frame_is_paintable(frame_size) {
            // An immediate child can recreate surfaces while the parent UI
            // runs. Ensure this surface exists, then size it from the retained
            // completed UI extent, never from a newer HWND or older resize event.
            #[cfg(target_os = "windows")]
            pollster::block_on(painter.set_window(viewport_id, Some(Arc::clone(window))))?;
            configure_surface_for_frame(frame_size, configured_frame_size, |width, height| {
                painter.on_window_resized(viewport_id, width, height);
            });

            let clipped_primitives = egui_ctx.tessellate(shapes, pixels_per_point);

            let mut screenshot_commands = vec![];
            viewport.actions_requested.retain(|cmd| {
                if let ActionRequested::Screenshot(info) = cmd {
                    screenshot_commands.push(info.clone());
                    false
                } else {
                    true
                }
            });
            let vsync_secs = painter.paint_and_update_textures(
                viewport_id,
                pixels_per_point,
                app.clear_color(&egui_ctx.global_style().visuals),
                &clipped_primitives,
                pending_deltas,
                screenshot_commands,
                window,
            );

            #[cfg(target_os = "windows")]
            if let Some(owner) = windows_window_owner(window)
                && let Some(size) = frame_size
            {
                // Native caller separately verifies successful presentation.
                // A skipped painter frame must never suppress the WM_SIZE fallback.
                super::windows_resize::frame_painted(owner, size, *configured_frame_size);
            }

            for action in viewport.actions_requested.drain(..) {
                match action {
                    ActionRequested::Screenshot { .. } => {
                        // already handled above
                    }
                    ActionRequested::Cut => {
                        egui_winit.egui_input_mut().events.push(egui::Event::Cut);
                    }
                    ActionRequested::Copy => {
                        egui_winit.egui_input_mut().events.push(egui::Event::Copy);
                    }
                    ActionRequested::Paste => {
                        if let Some(contents) = egui_winit.clipboard_text() {
                            let contents = contents.replace("\r\n", "\n");
                            if !contents.is_empty() {
                                egui_winit
                                    .egui_input_mut()
                                    .events
                                    .push(egui::Event::Paste(contents));
                            }
                        }
                    }
                }
            }

            integration.post_rendering(window);

            vsync_secs
        } else {
            0.0
        };

        let active_viewports_ids: ViewportIdSet = viewport_output.keys().copied().collect();

        handle_viewport_output(
            &integration.egui_ctx,
            &viewport_output,
            viewports,
            painter,
            viewport_from_window,
        );

        // Prune dead viewports:
        viewports.retain(|id, _| active_viewports_ids.contains(id));
        viewport_from_window.retain(|_, id| active_viewports_ids.contains(id));
        painter.gc_viewports(&active_viewports_ids);

        let window = viewport_from_window
            .get(&window_id)
            .and_then(|id| viewports.get(id))
            .and_then(|vp| vp.window.as_ref());

        integration.report_frame_time(frame_timer.total_time_sec() - vsync_secs); // don't count auto-save time as part of regular frame time

        integration.maybe_autosave(app.as_mut(), window.map(|w| w.as_ref()));

        sleep_if_invisible_or_minimized(window.map(|window| window.as_ref()));

        if integration.should_close() {
            Ok(EventResult::CloseRequested)
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn on_window_event(
        &mut self,
        window_id: WindowId,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        let Self {
            integration,
            shared,
            ..
        } = self;
        let mut shared = shared.borrow_mut();

        let viewport_id = shared.viewport_from_window.get(&window_id).copied();

        // On Windows, if a window is resized by the user, it should repaint synchronously, inside the
        // event handler. If this is not done, the compositor will assume that the window does not want
        // to redraw and continue ahead.
        //
        // In eframe's case, that causes the window to rapidly flicker, as it struggles to deliver
        // new frames to the compositor in time. The flickering is technically glutin or glow's fault, but we should be responding properly
        // to resizes anyway, as doing so avoids dropping frames.
        //
        // See: https://github.com/emilk/egui/issues/903
        let mut repaint_asap = false;
        #[cfg(target_os = "windows")]
        let mut resize_was_painted = false;

        // On MacOS the asap repaint is not enough. The drawn frames must be synchronized with
        // the CoreAnimation transactions driving the window resize process.
        //
        // Thus, Painter, responsible for wgpu surfaces and their resize, has to be notified of the
        // resize lifecycle, yet winit does not provide any events for that. To work around,
        // the last resized viewport is tracked until a later event outside the live resize stream
        // is received.
        //
        // AppKit can emit `Moved` events during top/left live resize because the window origin
        // changes along with the content size. Treat those as part of live resize on macOS.
        //
        // See: https://github.com/emilk/egui/issues/903
        let event_keeps_resize_active = matches!(event, winit::event::WindowEvent::Resized(_))
            || (cfg!(target_os = "macos") && matches!(event, winit::event::WindowEvent::Moved(_)));

        if !event_keeps_resize_active
            && let Some(id) = viewport_id
            && shared.resized_viewport == viewport_id
        {
            shared.painter.on_window_resize_state_change(id, false);
            shared.resized_viewport = None;
        }

        match event {
            winit::event::WindowEvent::Moved(_) if cfg!(target_os = "windows") => {
                if let Some(viewport_id) = viewport_id
                    && let Some(viewport) = shared.viewports.get_mut(&viewport_id)
                    && let Some(window) = viewport.window.as_ref()
                {
                    egui_winit::update_viewport_info(
                        &mut viewport.info,
                        &integration.egui_ctx,
                        window,
                        false,
                    );
                }
            }

            winit::event::WindowEvent::Focused(focused) => {
                let focused = if cfg!(target_os = "macos")
                    && let Some(viewport_id) = viewport_id
                    && let Some(viewport) = shared.viewports.get(&viewport_id)
                    && let Some(window) = &viewport.window
                {
                    // TODO(emilk): remove this work-around once we update winit
                    // https://github.com/rust-windowing/winit/issues/4371
                    // https://github.com/emilk/egui/issues/7588
                    window.has_focus()
                } else {
                    *focused
                };

                shared.focused_viewport = focused.then_some(viewport_id).flatten();
            }

            winit::event::WindowEvent::Resized(physical_size) => {
                #[cfg(target_os = "windows")]
                {
                    resize_was_painted = viewport_id
                        .and_then(|id| shared.viewports.get(&id))
                        .and_then(|viewport| viewport.window.as_ref())
                        .is_some_and(|window| {
                            windows_window_owner(window).is_some_and(|owner| {
                                super::windows_resize::consume_resize_ack(
                                    owner,
                                    *physical_size,
                                    windows_client_rect(window),
                                )
                            })
                        });
                }
                // Resize with 0 width and height is used by winit to signal a minimize event on Windows.
                // See: https://github.com/rust-windowing/winit/issues/208
                // This solves an issue where the app would panic when minimizing on Windows.
                if let Some(id) = viewport_id
                    && let (Some(width), Some(height)) = (
                        NonZeroU32::new(physical_size.width),
                        NonZeroU32::new(physical_size.height),
                    )
                {
                    if shared.resized_viewport != viewport_id {
                        shared.resized_viewport = viewport_id;
                        shared.painter.on_window_resize_state_change(id, true);
                    }
                    if !cfg!(target_os = "windows") {
                        shared.painter.on_window_resized(id, width, height);
                    }
                    repaint_asap = true;
                }
            }

            winit::event::WindowEvent::Occluded(is_occluded) => {
                if let Some(viewport_id) = viewport_id
                    && let Some(viewport) = shared.viewports.get_mut(&viewport_id)
                {
                    viewport.info.occluded = Some(*is_occluded);
                }
            }

            winit::event::WindowEvent::CloseRequested => {
                if viewport_id == Some(ViewportId::ROOT) && integration.should_close() {
                    log::debug!(
                        "Received WindowEvent::CloseRequested for main viewport - shutting down."
                    );
                    return EventResult::CloseRequested;
                }

                log::debug!("Received WindowEvent::CloseRequested for viewport {viewport_id:?}");

                if let Some(viewport_id) = viewport_id
                    && let Some(viewport) = shared.viewports.get_mut(&viewport_id)
                {
                    // Tell viewport it should close:
                    viewport.info.events.push(egui::ViewportEvent::Close);

                    // We may need to repaint both us and our parent to close the window,
                    // and perhaps twice (once to notice the close-event, once again to enforce it).
                    // `request_repaint_of` does a double-repaint though:
                    integration.egui_ctx.request_repaint_of(viewport_id);
                    integration.egui_ctx.request_repaint_of(viewport.ids.parent);
                }
            }

            _ => {}
        }

        let mut event_response = viewport_id
            .and_then(|viewport_id| {
                let viewport = shared.viewports.get_mut(&viewport_id)?;
                Some(integration.on_window_event(
                    viewport.window.as_deref()?,
                    viewport.egui_winit.as_mut()?,
                    event,
                ))
            })
            .unwrap_or_default();

        #[cfg(target_os = "windows")]
        if resize_was_painted {
            // Keep resize/egui/accessibility bookkeeping above. Only the
            // matching already-presented resize loses its duplicate paint.
            event_response.repaint = false;
        }

        // WM_WINDOWPOSCHANGED emits Moved before WM_SIZE during a left/top
        // resize. Painting for both spends a second display wait on identical
        // content; a pure move can reuse the compositor's existing surface.
        if cfg!(target_os = "windows")
            && matches!(event, winit::event::WindowEvent::Moved(_))
            && !self.app.repaint_on_window_move()
        {
            event_response.repaint = false;
        }

        if integration.should_close() {
            EventResult::CloseRequested
        } else if event_response.repaint {
            if repaint_asap {
                EventResult::RepaintNow(window_id)
            } else {
                EventResult::RepaintNext(window_id)
            }
        } else {
            EventResult::Wait
        }
    }
}

impl Viewport {
    /// Apply the commands, or defer them until we have a window.
    fn process_commands(
        &mut self,
        egui_ctx: &egui::Context,
        mut commands: Vec<egui::ViewportCommand>,
    ) {
        self.deferred_commands.append(&mut commands);

        if let Some(window) = self.window.as_ref() {
            egui_winit::process_viewport_commands(
                egui_ctx,
                &mut self.info,
                core::mem::take(&mut self.deferred_commands),
                window,
                &mut self.actions_requested,
            );
        }
    }

    /// Create winit window, if needed.
    fn initialize_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        egui_ctx: &egui::Context,
        windows_id: &mut HashMap<WindowId, ViewportId>,
        painter: &mut egui_wgpu::winit::Painter,
        windows_no_redirection_bitmap: bool,
    ) {
        if self.window.is_some() {
            return; // we already have one
        }

        profiling::function_scope!();

        let viewport_id = self.ids.this;
        self.configured_frame_size = None;

        match create_wgpu_window(
            egui_ctx,
            event_loop,
            &self.builder,
            windows_no_redirection_bitmap,
        ) {
            Ok(window) => {
                windows_id.insert(window.id(), viewport_id);

                let window = Arc::new(window);

                if let Err(err) =
                    pollster::block_on(painter.set_window(viewport_id, Some(Arc::clone(&window))))
                {
                    log::error!("on set_window: viewport_id {viewport_id:?} {err}");
                }

                self.egui_winit = Some(egui_winit::State::new(
                    egui_ctx.clone(),
                    viewport_id,
                    event_loop,
                    Some(window.scale_factor() as f32),
                    event_loop.system_theme(),
                    painter.max_texture_side(),
                ));

                egui_winit::update_viewport_info(&mut self.info, egui_ctx, &window, true);
                self.window = Some(window);
            }
            Err(err) => {
                log::error!("Failed to create window: {err}");
            }
        }
    }
}

fn create_window(
    egui_ctx: &egui::Context,
    event_loop: &ActiveEventLoop,
    storage: Option<&dyn Storage>,
    native_options: &mut NativeOptions,
) -> Result<(Window, ViewportBuilder), winit::error::OsError> {
    profiling::function_scope!();

    let window_settings = epi_integration::load_window_settings(storage);
    let viewport_builder = epi_integration::viewport_builder(
        egui_ctx.zoom_factor(),
        event_loop,
        native_options,
        window_settings,
    )
    .with_visible(false); // Start hidden until we render the first frame to fix white flash on startup (https://github.com/emilk/egui/pull/3631)

    let window = create_wgpu_window(
        egui_ctx,
        event_loop,
        &viewport_builder,
        native_options.windows_no_redirection_bitmap,
    )?;
    epi_integration::apply_window_settings(&window, window_settings);
    Ok((window, viewport_builder))
}

fn create_wgpu_window(
    egui_ctx: &egui::Context,
    event_loop: &ActiveEventLoop,
    builder: &ViewportBuilder,
    windows_no_redirection_bitmap: bool,
) -> Result<Window, winit::error::OsError> {
    let attributes = egui_winit::apply_monitor_to_window_attributes(
        egui_winit::create_winit_window_attributes(egui_ctx, builder.clone()),
        builder,
        event_loop,
    );
    #[cfg(target_os = "windows")]
    let attributes = {
        use winit::platform::windows::WindowAttributesExtWindows as _;
        attributes.with_no_redirection_bitmap(windows_no_redirection_bitmap)
    };
    #[cfg(not(target_os = "windows"))]
    let _ = windows_no_redirection_bitmap;
    let window = event_loop.create_window(attributes)?;
    egui_winit::apply_viewport_builder_to_window(egui_ctx, &window, builder);
    Ok(window)
}

/// Is this viewport, or any of its (transitive) descendant viewports, visible?
///
/// Immediate viewports are rendered inline while their parent's UI runs, so even
/// if this viewport's window is occluded or minimized we must still run its UI to
/// give any visible descendant a chance to be painted.
fn is_viewport_or_descendant_visible(viewports: &Viewports, viewport_id: ViewportId) -> bool {
    let Some(viewport) = viewports.get(&viewport_id) else {
        return false;
    };
    if viewport.info.visible().unwrap_or(true)
        && viewport
            .window
            .as_ref()
            .is_none_or(|window| window.is_visible() != Some(false))
    {
        return true;
    }
    viewports.values().any(|child| {
        child.ids.parent == viewport_id
            && child.ids.this != viewport_id
            && is_viewport_or_descendant_visible(viewports, child.ids.this)
    })
}

fn render_immediate_viewport(
    beginning: Instant,
    shared: &RefCell<SharedState>,
    immediate_viewport: ImmediateViewport<'_>,
) {
    profiling::function_scope!();
    #[cfg(target_os = "windows")]
    let _render_guard = super::windows_resize::RenderGuard::new();

    let ImmediateViewport {
        ids,
        builder,
        mut viewport_ui_cb,
    } = immediate_viewport;

    let (input, frame_size) = {
        let SharedState {
            egui_ctx,
            viewports,
            painter,
            viewport_from_window,
            windows_no_redirection_bitmap,
            ..
        } = &mut *shared.borrow_mut();

        let viewport = initialize_or_update_viewport(
            viewports,
            ids,
            ViewportClass::Immediate,
            builder,
            None,
            painter,
        );
        if viewport.window.is_none() {
            event_loop_context::with_current_event_loop(|event_loop| {
                viewport.initialize_window(
                    event_loop,
                    egui_ctx,
                    viewport_from_window,
                    painter,
                    *windows_no_redirection_bitmap,
                );
            });
        }

        let (Some(window), Some(egui_winit)) = (&viewport.window, &mut viewport.egui_winit) else {
            return;
        };
        egui_winit::update_viewport_info(&mut viewport.info, egui_ctx, window, false);

        let (mut input, frame_size) = take_frame_input(egui_winit, window, &mut viewport.info);
        // Immediate viewports bypass App::raw_input_hook. Inherit the parent's
        // actual atlas limit instead of resetting the shared atlas to the GPU
        // maximum on every child pass (and back on every parent pass).
        input.max_texture_side = Some(egui_ctx.fonts(|fonts| fonts.options().max_texture_side));
        input.viewports = viewports
            .iter()
            .map(|(id, viewport)| (*id, viewport.info.clone()))
            .collect();
        input.time = Some(beginning.elapsed().as_secs_f64());
        (input, frame_size)
    };

    let egui_ctx = shared.borrow().egui_ctx.clone();

    // ------------------------------------------

    // Run the user code, which could re-entrantly call this function again (!).
    // Make sure no locks are held during this call.
    let egui::FullOutput {
        platform_output,
        textures_delta,
        shapes,
        pixels_per_point,
        viewport_output,
    } = egui_ctx.run_ui(input, |ui| {
        viewport_ui_cb(ui);
    });
    let frame_size = completed_frame_size(
        frame_size,
        egui_ctx.input_for(ids.this, |input| input.viewport_rect().size()),
        pixels_per_point,
    );

    // ------------------------------------------

    let mut shared_mut = shared.borrow_mut();
    let SharedState {
        viewports,
        painter,
        viewport_from_window,
        pending_deltas,
        ..
    } = &mut *shared_mut;

    let Some(viewport) = viewports.get_mut(&ids.this) else {
        warn!("Viewport disappeared unexpectedly!");
        return;
    };
    pending_deltas.append(textures_delta);

    viewport.info.events.clear(); // they should have been processed
    let (Some(egui_winit), Some(window)) = (&mut viewport.egui_winit, &viewport.window) else {
        return;
    };

    #[cfg(not(target_os = "windows"))]
    {
        profiling::scope!("set_window");
        if let Err(err) = pollster::block_on(painter.set_window(ids.this, Some(Arc::clone(window))))
        {
            log::error!(
                "when rendering viewport_id={:?}, set_window Error {err}",
                ids.this
            );
        }
    }

    // Immediate viewports use the same visibility and action rules as the
    // ordinary viewport path. A retained hidden launcher needs no GPU work.
    let will_be_visible = paint_before_visibility_commands(
        window.is_visible(),
        requested_visibility(&viewport_output, ids.this),
        false,
    );
    if will_be_visible && frame_is_paintable(frame_size) {
        #[cfg(target_os = "windows")]
        let surface_ready =
            match pollster::block_on(painter.set_window(ids.this, Some(Arc::clone(window)))) {
                Ok(()) => true,
                Err(err) => {
                    log::error!(
                        "when rendering viewport_id={:?}, set_window Error {err}",
                        ids.this
                    );
                    viewport.configured_frame_size = None;
                    false
                }
            };
        #[cfg(not(target_os = "windows"))]
        let surface_ready = true;
        if surface_ready {
            configure_surface_for_frame(
                frame_size,
                &mut viewport.configured_frame_size,
                |width, height| painter.on_window_resized(ids.this, width, height),
            );
            let clipped_primitives = egui_ctx.tessellate(shapes, pixels_per_point);
            let mut screenshot_commands = vec![];
            viewport.actions_requested.retain(|action| {
                if let ActionRequested::Screenshot(data) = action {
                    screenshot_commands.push(data.clone());
                    false
                } else {
                    true
                }
            });
            painter.paint_and_update_textures(
                ids.this,
                pixels_per_point,
                [0.0, 0.0, 0.0, 0.0],
                &clipped_primitives,
                pending_deltas,
                screenshot_commands,
                window,
            );
            for action in viewport.actions_requested.drain(..) {
                match action {
                    ActionRequested::Screenshot { .. } => {}
                    ActionRequested::Cut => {
                        egui_winit.egui_input_mut().events.push(egui::Event::Cut)
                    }
                    ActionRequested::Copy => {
                        egui_winit.egui_input_mut().events.push(egui::Event::Copy)
                    }
                    ActionRequested::Paste => {
                        if let Some(contents) = egui_winit.clipboard_text() {
                            let contents = contents.replace("\r\n", "\n");
                            if !contents.is_empty() {
                                egui_winit
                                    .egui_input_mut()
                                    .events
                                    .push(egui::Event::Paste(contents));
                            }
                        }
                    }
                }
            }
        }
    }

    egui_winit.handle_platform_output(window, platform_output);

    handle_viewport_output(
        &egui_ctx,
        &viewport_output,
        viewports,
        painter,
        viewport_from_window,
    );
}

pub(crate) fn remove_viewports_not_in(
    viewports: &mut Viewports,
    painter: &mut egui_wgpu::winit::Painter,
    viewport_from_window: &mut HashMap<WindowId, ViewportId>,
    viewport_output: &OrderedViewportIdMap<ViewportOutput>,
) {
    let active_viewports_ids: ViewportIdSet = viewport_output.keys().copied().collect();

    // Prune dead viewports:
    viewports.retain(|id, _| active_viewports_ids.contains(id));
    viewport_from_window.retain(|_, id| active_viewports_ids.contains(id));
    painter.gc_viewports(&active_viewports_ids);
}

/// Add new viewports, and update existing ones:
fn handle_viewport_output(
    egui_ctx: &egui::Context,
    viewport_output: &OrderedViewportIdMap<ViewportOutput>,
    viewports: &mut Viewports,
    painter: &mut egui_wgpu::winit::Painter,
    viewport_from_window: &mut HashMap<WindowId, ViewportId>,
) {
    for (
        viewport_id,
        ViewportOutput {
            parent,
            class,
            builder,
            viewport_ui_cb,
            commands,
            repaint_delay: _, // ignored - we listened to the repaint callback instead
        },
    ) in viewport_output.clone()
    {
        let ids = ViewportIdPair::from_self_and_parent(viewport_id, parent);

        let viewport =
            initialize_or_update_viewport(viewports, ids, class, builder, viewport_ui_cb, painter);

        let old_inner_size = viewport.window.as_ref().map(|window| window.inner_size());

        viewport.process_commands(egui_ctx, commands);

        // For Wayland : https://github.com/emilk/egui/issues/4196
        if cfg!(target_os = "linux")
            && let Some(window) = viewport.window.as_ref()
            && let Some(old_inner_size) = old_inner_size
        {
            let new_inner_size = window.inner_size();
            if new_inner_size != old_inner_size
                && let (Some(width), Some(height)) = (
                    NonZeroU32::new(new_inner_size.width),
                    NonZeroU32::new(new_inner_size.height),
                )
            {
                painter.on_window_resized(viewport_id, width, height);
            }
        }
    }

    remove_viewports_not_in(viewports, painter, viewport_from_window, viewport_output);
}

fn initialize_or_update_viewport<'a>(
    viewports: &'a mut Viewports,
    ids: ViewportIdPair,
    class: ViewportClass,
    mut builder: ViewportBuilder,
    viewport_ui_cb: Option<Arc<dyn Fn(&mut egui::Ui) + Send + Sync>>,
    painter: &mut egui_wgpu::winit::Painter,
) -> &'a mut Viewport {
    use std::collections::btree_map::Entry;

    profiling::function_scope!();

    if builder.icon.is_none() {
        // Inherit icon from parent
        builder.icon = viewports
            .get_mut(&ids.parent)
            .and_then(|vp| vp.builder.icon.clone());
    }

    let mut recreate_surface = false;
    match viewports.entry(ids.this) {
        Entry::Vacant(entry) => {
            // New viewport:
            log::debug!("Creating new viewport {:?} ({:?})", ids.this, builder.title);
            entry.insert(Viewport {
                ids,
                class,
                builder,
                deferred_commands: vec![],
                info: Default::default(),
                actions_requested: Vec::new(),
                viewport_ui_cb,
                window: None,
                egui_winit: None,
                configured_frame_size: None,
            });
        }

        Entry::Occupied(mut entry) => {
            // Patch an existing viewport:
            let viewport = entry.get_mut();

            viewport.class = class;
            viewport.ids.parent = ids.parent;
            viewport.viewport_ui_cb = viewport_ui_cb;

            let (mut delta_commands, recreate) = viewport.builder.patch(builder);

            if recreate {
                log::debug!(
                    "Recreating window for viewport {:?} ({:?})",
                    ids.this,
                    viewport.builder.title
                );
                viewport.window = None;
                viewport.egui_winit = None;
                recreate_surface = true;
            }

            viewport.deferred_commands.append(&mut delta_commands);
        }
    }

    if recreate_surface {
        // Painter::set_window(None) clears every surface, not only this ID.
        // Invalidate all extent caches before any viewport can paint again.
        for viewport in viewports.values_mut() {
            viewport.configured_frame_size = None;
        }
        if let Err(err) = pollster::block_on(painter.set_window(ids.this, None)) {
            log::error!(
                "when rendering viewport_id={:?}, set_window Error {err}",
                ids.this
            );
        }
    }

    viewports.get_mut(&ids.this).unwrap()
}

#[cfg(test)]
mod frame_size_tests {
    use super::*;

    #[test]
    fn frame_layout_and_surface_use_the_same_snapshot_at_fractional_dpi() {
        // This is the 36 x 20 pixel mismatch observed in displayed captures.
        // The queued/native sizes may move again while this frame is built;
        // only its retained input snapshot is allowed to size the framebuffer.
        for pixels_per_point in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5] {
            let mut configured = Some(PhysicalSize::new(948, 660));
            for snapshot in [PhysicalSize::new(912, 640), PhysicalSize::new(948, 660)] {
                let screen_rect = frame_screen_rect(snapshot, pixels_per_point).unwrap();
                let mut actual_framebuffer = None;
                assert!(configure_surface_for_frame(
                    Some(snapshot),
                    &mut configured,
                    |width, height| {
                        actual_framebuffer = Some(PhysicalSize::new(width.get(), height.get()));
                    },
                ));
                assert_eq!(actual_framebuffer, Some(snapshot));
                let layout_pixels = screen_rect.size() * pixels_per_point;
                assert_eq!(layout_pixels.x.round() as u32, snapshot.width);
                assert_eq!(layout_pixels.y.round() as u32, snapshot.height);
                assert_eq!(configured, Some(snapshot));
            }
        }
    }

    #[test]
    fn unchanged_or_zero_frames_do_not_resize_and_invalidation_restores_the_surface() {
        let snapshot = PhysicalSize::new(912, 640);
        let mut configured = None;
        let mut calls = 0;
        for _ in 0..4 {
            assert!(configure_surface_for_frame(
                Some(snapshot),
                &mut configured,
                |_, _| calls += 1,
            ));
        }
        assert_eq!(calls, 1, "identical frames must not call ResizeBuffers");
        for zero in [PhysicalSize::new(0, 640), PhysicalSize::new(912, 0)] {
            assert!(!frame_is_paintable(Some(zero)));
            assert!(!configure_surface_for_frame(
                Some(zero),
                &mut configured,
                |_, _| calls += 1,
            ));
        }
        assert_eq!(calls, 1);
        assert_eq!(
            configured,
            Some(snapshot),
            "hidden/zero frames do not advance the cache"
        );

        // Window recreation/suspension invalidates the cache even when the
        // replacement HWND happens to have the same physical dimensions.
        configured = None;
        assert!(configure_surface_for_frame(
            Some(snapshot),
            &mut configured,
            |_, _| calls += 1,
        ));
        assert_eq!(calls, 2);

        assert!(configure_surface_for_frame(
            None,
            &mut configured,
            |_, _| calls += 1
        ));
        assert_eq!(calls, 2, "other platforms keep their existing resize path");
    }

    #[test]
    fn pending_zoom_and_resize_size_the_surface_for_the_completed_egui_canvas() {
        let ctx = egui::Context::default();
        let before = PhysicalSize::new(948, 660);
        let resized = PhysicalSize::new(912, 640);
        let input = |size| {
            let mut input = egui::RawInput {
                screen_rect: frame_screen_rect(size, ctx.zoom_factor()),
                ..Default::default()
            };
            input
                .viewports
                .entry(ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(1.0);
            input
        };
        let mut initial_output = ctx.run_ui(input(before), |_| {});
        initial_output.textures_delta.clear();
        ctx.set_zoom_factor(1.25);
        let mut output = ctx.run_ui(input(resized), |_| {});
        let canvas = ctx.input_for(ViewportId::ROOT, |input| input.viewport_rect().size());
        let completed = completed_frame_size(Some(resized), canvas, output.pixels_per_point);
        // egui's pending-zoom pass intentionally reuses the previous canvas.
        assert_eq!(completed, Some(before));
        assert_ne!(completed, Some(resized));
        let mut configured = Some(resized);
        let mut actual_framebuffer = None;
        configure_surface_for_frame(completed, &mut configured, |width, height| {
            actual_framebuffer = Some(PhysicalSize::new(width.get(), height.get()));
        });
        assert_eq!(actual_framebuffer, completed);
        let minimized = PhysicalSize::new(0, 0);
        assert_eq!(
            completed_frame_size(Some(minimized), canvas, output.pixels_per_point),
            Some(minimized)
        );
        output.textures_delta.clear();
    }
}
