#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;
mod platform;
mod shell;
mod theme;
mod ui;

use raw_window_handle::HasWindowHandle;

fn main() -> eframe::Result {
    let cli = shell::ipc::parse_args(std::env::args().skip(1));
    // Graceful shutdown signal for the installer/uninstaller (works even when hidden in tray).
    if cli.quit {
        let _ = shell::ipc::try_send_to_existing(&shell::ipc::LaunchTarget::Quit);
        return Ok(());
    }
    // Single-instance fast path: forward folder / `/select,` requests
    // to the running primary Lumen window in < 15ms unless `--new-window` is set.
    if !cli.new_window && shell::ipc::try_send_to_existing(&cli.target) {
        return Ok(());
    }

    platform::init_process();

    let mut settings = core::settings::load();
    if cli.start_background {
        settings.tray_resident = true;
    }
    let window_state = core::windowstate::load();
    let (x, y, width, height) = platform::clamp_logical_window(
        window_state.x,
        window_state.y,
        window_state.w.unwrap_or(1440.0),
        window_state.h.unwrap_or(900.0),
    );

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Lumen")
        .with_app_id("Lumen.Explorer")
        .with_inner_size([width, height])
        .with_min_inner_size([820.0, 520.0])
        .with_decorations(false)
        .with_transparent(settings.mica_backdrop)
        .with_visible(!cli.start_background)
        .with_has_shadow(true)
        .with_maximized(window_state.maximized && !cli.start_background)
        .with_drag_and_drop(false);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    if let (Some(x), Some(y)) = (x, y) {
        viewport = viewport.with_position([x, y]);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        // The backend's text-only paste translation discards CF_HDROP file pastes.
        // Lumen translates clipboard keys in App::raw_input_hook instead.
        clipboard_shortcuts: false,
        ..Default::default()
    };

    eframe::run_native(
        "Lumen",
        options,
        Box::new(move |cc| {
            if let Ok(handle) = cc.window_handle()
                && let raw_window_handle::RawWindowHandle::Win32(window) = handle.as_raw()
            {
                platform::attach(window.hwnd.get());
            }
            Ok(Box::new(app::LumenApp::new(cc, settings, cli)))
        }),
    )
}

/// Window icon decoded from the embedded PNG; a broken asset must not stop the app.
fn app_icon() -> Option<egui::IconData> {
    const PNG: &[u8] = include_bytes!("../assets/lumen.png");
    let image = image::load_from_memory(PNG).ok()?.to_rgba8();
    let (width, height) = (image.width(), image.height());
    Some(egui::IconData { rgba: image.into_raw(), width, height })
}
