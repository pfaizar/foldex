#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod assets;
mod batch;
mod bookmarks;
mod commands;
mod context;
mod cut_state;
mod goto;
mod home_history;
mod hotkeys;
mod inspector;
mod installation;
mod integration;
mod keyboard;
mod launcher;
mod native_frame;
mod native_keys;
mod navigation;
mod operation_history;
mod pane;
mod performance;
mod persistence;
mod picker;
mod platform;
mod recycle_bin;
mod search;
mod selection;
mod sidebar;
mod theme_store;
mod updates;
mod workspace;
mod workspace_tree;
#[cfg(all(feature = "renderer-glow", feature = "renderer-wgpu"))]
compile_error!("Choose exactly one renderer feature");

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
static UI_CONTEXT: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

fn main() -> eframe::Result {
    let launched = Instant::now();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Setup helpers must not enter single-instance activation, load user state,
    // start network checks, or create a browser/GPU runtime.
    if args.iter().any(|arg| arg == "--uninstall-integration") {
        if let Err(error) = integration::uninstall() {
            installation::show_error(&error);
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--probe-lifecycle") {
        return app::lifecycle_probe::run(&args);
    }
    if args.iter().any(|arg| arg == "--probe-window-transfer") {
        return app::window_transfer_probe::run(&args);
    }
    if sidebar::network::run_helper_if_requested(&args) {
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--benchmark-monitors") {
        println!(
            "{}",
            serde_json::to_string(&performance::monitor::inventory().unwrap_or_default()).unwrap()
        );
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--benchmark-gpu") {
        if let Err(error) = performance::gpu_context(&args, launched) {
            eprintln!("GPU context benchmark failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--benchmark-search") {
        if let Err(error) = performance::search(&args, launched) {
            eprintln!("Search benchmark failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let mut startup_transfer = app::window_transfer::read_startup(&args)
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    let _transfer_cleanup = startup_transfer.as_ref().map(|startup| startup.cleanup());
    let transfer_probe = startup_transfer
        .as_ref()
        .is_some_and(|startup| startup.is_probe());
    let (mut ui_benchmark, _ui_fixture) = match performance::prepare_ui(&args, launched) {
        Ok(Some((run, fixture))) => (Some(run), Some(fixture)),
        Ok(None) => (None, None),
        Err(error) => {
            eprintln!("UI benchmark setup failed: {error}");
            std::process::exit(1);
        }
    };
    let initial = ui_benchmark
        .as_ref()
        .map(|run| run.scope.clone())
        .or_else(|| {
            if startup_transfer.is_some() {
                return None;
            }
            if args.iter().any(|argument| argument == "--home") {
                return Some(PathBuf::from("foldex:home"));
            }
            if let Some(index) = args.iter().position(|argument| argument == "--shell-open") {
                return args
                    .get(index + 1)
                    .map(|argument| integration::folder_argument(argument));
            }
            args.iter()
                .find(|a| !a.to_string_lossy().starts_with("--"))
                .map(PathBuf::from)
        });
    let exe_dir = ui_benchmark
        .as_ref()
        .map(|run| run.state_dir.clone())
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
                .unwrap_or_default()
        });
    let _running_installation = installation::claim(&exe_dir).map_err(|error| {
        installation::show_error(&error.to_string());
        eframe::Error::AppCreation(Box::new(error))
    })?;
    let state_dir = startup_transfer
        .as_ref()
        .map(|startup| startup.state_directory())
        .map(Ok)
        .unwrap_or_else(|| installation::state_directory(&exe_dir))
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    let crash_path = state_dir.join("foldex-crash.log");
    install_panic_log(crash_path.clone());
    let theme_path = state_dir.join("theme.toml");
    let theme = startup_transfer
        .as_ref()
        .and_then(|startup| foldex_ui::Theme::read(&startup.theme).ok())
        .or_else(|| {
            read_small(&theme_path, 64 * 1024)
                .and_then(|s| String::from_utf8(s).ok())
                .and_then(|s| foldex_ui::Theme::read(&s).ok())
        })
        .unwrap_or_default();
    let auxiliary = args.iter().any(|arg| arg == "--new-window");
    let shared_state_dir = installation::state_directory(&exe_dir)
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    let preferences_path = shared_state_dir.join("foldex-preferences.json");
    let primary_session = state_dir.join("foldex-session.json");
    let session_path = persistence::window_path(&state_dir, auxiliary);
    let session: app::Session = startup_transfer
        .as_mut()
        .map(|startup| std::mem::take(&mut startup.session))
        .or_else(|| {
            read_small(&primary_session, persistence::MAX_SESSION_BYTES)
                .and_then(|s| serde_json::from_slice(&s).ok())
        })
        .unwrap_or_default();
    let mut session_value = serde_json::to_value(&session)
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    persistence::shared::load(&preferences_path, &mut session_value)
        .map_err(|error| {
            installation::show_error(&format!("Shared preferences could not be read: {error}\n\nThe original file was preserved at {}. A previous valid version, when available, is stored beside it as foldex-preferences.previous.json.",preferences_path.display()));
            eframe::Error::AppCreation(Box::new(error))
        })?;
    let session: app::Session = serde_json::from_value(session_value)
        .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    let font = read_small(Path::new("C:/Windows/Fonts/segoeui.ttf"), 4 * 1024 * 1024);
    let secondary = if args.iter().any(|arg| arg == "--secondary-monitor") {
        match performance::monitor::secondary() {
            Ok(monitor) => Some(monitor),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    if let Some(run) = &mut ui_benchmark {
        run.monitor = secondary;
    }
    let isolate_launcher = ui_benchmark.is_some() || auxiliary;
    let open_settings = args.iter().any(|arg| arg == "--launcher-settings");
    let open_launcher = open_settings || args.iter().any(|arg| arg == "--launcher");
    let activation = startup_activation(&args, initial.as_deref(), session.launcher.background);
    let tray_only = !isolate_launcher
        && matches!(
            activation,
            launcher::instance::Activation::StayInTray
                | launcher::instance::Activation::Launcher
                | launcher::instance::Activation::Settings
        );
    let channel = (!isolate_launcher).then(|| launcher::instance::identity(&exe_dir));
    let _instance = if let Some(channel) = &channel {
        match launcher::instance::claim(channel, &activation) {
            Ok(launcher::instance::Claim::Primary(guard)) => Some(guard),
            Ok(launcher::instance::Claim::Forwarded) => return Ok(()),
            Err(error) => {
                eprintln!("Foldex activation failed: {error}");
                return Err(eframe::Error::AppCreation(Box::new(error)));
            }
        }
    } else {
        None
    };
    let launcher_runtime = launcher::Runtime {
        isolate: isolate_launcher,
        auxiliary: auxiliary && !transfer_probe,
        tray_only,
        open_launcher,
        open_settings,
        channel,
        secondary,
    };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Foldex")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([420.0, 320.0])
            .with_decorations(true)
            .with_icon(Arc::new(app_icon())),
        renderer: {
            #[cfg(feature = "renderer-wgpu")]
            {
                eframe::Renderer::Wgpu
            }
            #[cfg(feature = "renderer-glow")]
            {
                eframe::Renderer::Glow
            }
        },
        persist_window: false,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    if secondary.is_some() || tray_only {
        options.viewport = options.viewport.with_visible(false).with_active(false);
    }
    if let Some(startup) = &startup_transfer {
        options.viewport = options.viewport.with_visible(false).with_active(false);
        if let Some(position) = startup.position {
            options.viewport = options.viewport.with_position(position);
        }
    }
    #[cfg(feature = "renderer-wgpu")]
    {
        #[cfg(windows)]
        wgpu::hal::dx12::set_pixel_preserving_windows(
            // Separate diagnostic for HWND presentation; the normal composition
            // path keeps content at identity and supplies its own backdrop.
            std::env::var("FOLDEX_DX12_RESIZE_SCALING").as_deref() == Ok("none"),
            theme.background,
        );
        #[cfg(windows)]
        wgpu::hal::dx12::set_window_timings(
            ui_benchmark
                .as_ref()
                .is_some_and(|run| run.stress.is_some() || run.resize.is_some()),
        );
        options.wgpu_options.surface.present_mode = wgpu::PresentMode::AutoVsync;
        options.wgpu_options.surface.desired_maximum_frame_latency = Some(
            std::env::var("FOLDEX_DX12_FRAME_LATENCY")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| (1..=3).contains(value))
                .unwrap_or(1),
        );
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
        {
            #[cfg(windows)]
            if setup.instance_descriptor.backends & wgpu::Instance::enabled_backend_features()
                == wgpu::Backends::DX12
            {
                let presentation = wgpu::Dx12SwapchainKind::from_env()
                    .unwrap_or(wgpu::Dx12SwapchainKind::DxgiFromVisual);
                setup
                    .instance_descriptor
                    .backend_options
                    .dx12
                    .presentation_system = presentation;
                options.windows_no_redirection_bitmap =
                    presentation == wgpu::Dx12SwapchainKind::DxgiFromVisual;
                wgpu::hal::dx12::set_composition_windows(
                    options.windows_no_redirection_bitmap,
                    theme.background,
                );
                wgpu::hal::dx12::set_single_adapter_preference(Some(setup.power_preference));
            }
            let base_descriptor = setup.device_descriptor.clone();
            setup.device_descriptor = Arc::new(move |adapter| {
                let mut descriptor = base_descriptor(adapter);
                // DX12 sizes an upfront descriptor heap from this limit. Foldex uses
                // only a font atlas, a few uniforms and a bounded preview texture.
                descriptor.required_limits.max_non_sampler_bindings = 4096;
                descriptor.memory_hints = wgpu::MemoryHints::MemoryUsage;
                descriptor
            });
        }
    }
    #[cfg(windows)]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_msg_hook(|raw| {
                use windows_sys::Win32::UI::{
                    Input::KeyboardAndMouse::{
                        GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
                    },
                    WindowsAndMessaging::{
                        MSG, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
                        WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
                        WM_SYSKEYDOWN, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN,
                    },
                };
                // Winit owns this MSG for the callback lifetime. This observes only our event loop.
                let msg = unsafe { &*(raw as *const MSG) };
                if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                    let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
                    let shift = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
                    let alt = unsafe { GetKeyState(VK_MENU as i32) } < 0;
                    let modifiers = egui::Modifiers {
                        ctrl,
                        shift,
                        alt,
                        command: ctrl,
                        mac_cmd: unsafe { GetKeyState(VK_LWIN as i32) } < 0
                            || unsafe { GetKeyState(VK_RWIN as i32) } < 0,
                    };
                    if native_keys::record(
                        msg.wParam as u32,
                        modifiers,
                        msg.lParam as usize & (1 << 30) != 0,
                    ) {
                        if let Some(ctx) = UI_CONTEXT.get() {
                            ctx.request_repaint();
                        }
                        return true;
                    }
                } else if matches!(msg.message, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK) {
                    native_keys::record_pointer(egui::PointerButton::Primary);
                } else if matches!(msg.message, WM_RBUTTONDOWN | WM_RBUTTONDBLCLK) {
                    native_keys::record_pointer(egui::PointerButton::Secondary);
                } else if matches!(msg.message, WM_MBUTTONDOWN | WM_MBUTTONDBLCLK) {
                    native_keys::record_pointer(egui::PointerButton::Middle);
                } else if matches!(msg.message, WM_XBUTTONDOWN | WM_XBUTTONDBLCLK) {
                    native_keys::record_pointer(if msg.wParam >> 16 == 1 {
                        egui::PointerButton::Extra1
                    } else {
                        egui::PointerButton::Extra2
                    });
                } else if msg.message == WM_KILLFOCUS {
                    native_keys::set_file_mode(false);
                }
                false
            });
        }));
    }
    if let Some(run) = &mut ui_benchmark {
        run.pre_framework_ms = launched.elapsed().as_secs_f64() * 1000.0;
    }
    let result = eframe::run_native(
        "Foldex",
        options,
        Box::new(move |cc| {
            if let Some(monitor) = secondary {
                performance::monitor::place(cc, monitor)?;
            }
            if let Some(run) = &mut ui_benchmark {
                run.framework_ready_ms = Some(launched.elapsed().as_secs_f64() * 1000.0);
            }
            let _ = UI_CONTEXT.set(cc.egui_ctx.clone());
            if let Some(font) = font {
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
                cc.egui_ctx.set_fonts(fonts);
            }
            let mut theme = theme;
            theme.resolve_system(cc.egui_ctx.system_theme());
            theme.apply(&cc.egui_ctx);
            let mut app = app::Foldex::new(
                cc,
                theme,
                initial,
                theme_path,
                session_path,
                preferences_path,
                session,
                launched,
                ui_benchmark,
                launcher_runtime,
            )?;
            if let Some(startup) = startup_transfer {
                app.accept_window_transfer(startup.incoming);
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(error) = &result {
        write_crash_report(&crash_path, format!("Foldex runtime error: {error}\n"));
    }
    if result.is_ok()
        && let Err(error) = updates::finish_handoff()
    {
        installation::show_error(&format!(
            "{error}\nYour current installation is unchanged. Open Foldex to try again."
        ));
    }
    result
}

fn startup_activation(
    args: &[std::ffi::OsString],
    initial: Option<&Path>,
    background_enabled: bool,
) -> launcher::instance::Activation {
    use launcher::instance::Activation;
    if let Some(path) = initial {
        return Activation::Path(path.to_owned());
    }
    if args.iter().any(|arg| arg == "--launcher-settings") {
        return Activation::Settings;
    }
    if args.iter().any(|arg| arg == "--launcher") {
        return Activation::Launcher;
    }
    // Background controls whether closing keeps Foldex resident. A deliberate
    // EXE launch must still open (or reactivate) the file browser. Quiet startup
    // is an explicit --background request, including future login shortcuts.
    let show = args
        .iter()
        .any(|arg| arg == "--show" || arg == "--shell-home" || arg == "--new-window");
    if background_enabled && !show && args.iter().any(|arg| arg == "--background") {
        Activation::StayInTray
    } else {
        Activation::Explorer
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use launcher::instance::Activation;

    fn activation(args: &[&str], background: bool) -> Activation {
        let args = args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        startup_activation(&args, None, background)
    }

    #[test]
    fn exe_launch_opens_explorer_even_when_background_is_enabled() {
        assert_eq!(activation(&[], true), Activation::Explorer);
        assert_eq!(activation(&[], false), Activation::Explorer);
        assert_eq!(
            activation(&["--secondary-monitor"], true),
            Activation::Explorer
        );
    }

    #[test]
    fn quiet_start_is_explicit_and_cannot_hide_a_disabled_background_app() {
        assert_eq!(activation(&["--background"], true), Activation::StayInTray);
        assert_eq!(activation(&["--background"], false), Activation::Explorer);
        for show in ["--show", "--shell-home", "--new-window"] {
            assert_eq!(
                activation(&["--background", show], true),
                Activation::Explorer
            );
        }
    }

    #[test]
    fn launcher_and_settings_keep_their_own_activation() {
        assert_eq!(activation(&["--launcher"], true), Activation::Launcher);
        assert_eq!(activation(&["--launcher"], false), Activation::Launcher);
        assert_eq!(
            activation(&["--launcher-settings"], true),
            Activation::Settings
        );
    }

    #[test]
    fn explicit_path_opens_instead_of_remaining_in_tray() {
        let args = [std::ffi::OsString::from("--background")];
        let path = Path::new("C:/Users/Example/Documents");
        assert_eq!(
            startup_activation(&args, Some(path), true),
            Activation::Path(path.into())
        );
    }
}

// No work while the UI runs. A single bounded local report survives an abort,
// including failures reached through a native resize or background callback.
fn install_panic_log(path: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = format!(
            "Foldex panic: {info}\n{}\n",
            std::backtrace::Backtrace::force_capture()
        );
        write_crash_report(&path, report);
        previous(info);
    }));
}

fn write_crash_report(path: &Path, mut report: String) {
    if report.len() > 64 * 1024 {
        let mut end = 64 * 1024;
        while !report.is_char_boundary(end) {
            end -= 1;
        }
        report.truncate(end);
    }
    let _ = std::fs::write(path, report);
}

fn read_small(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take((limit + 1) as u64)
        .read_to_end(&mut data)
        .ok()?;
    (data.len() <= limit).then_some(data)
}

fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/brand/foldex.png"))
        .expect("The embedded Foldex icon is a valid PNG")
}
