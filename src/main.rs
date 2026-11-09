// Always a GUI app: no console window. Logs still reach a terminal we were started from (see `attach_console`).
#![windows_subsystem = "windows"]

mod anim;
mod app;
mod config;
mod fonts;
mod fx;
mod import;
mod platform;
mod secrets;
mod session;
mod shader;
mod termview;
mod theme;
mod veil;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    attach_console();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // Network I/O runs on a small dedicated runtime; the UI thread never blocks on it.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("haunt-net")
        .enable_all()
        .build()
        .expect("failed to start the async runtime");
    let handle = runtime.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("haunt")
            .with_app_id("haunt")
            .with_inner_size([1180.0, 740.0])
            .with_min_inner_size([620.0, 380.0])
            .with_transparent(true)
            .with_decorations(false),
        ..Default::default()
    };

    let result = eframe::run_native(
        "haunt",
        options,
        Box::new(move |cc| Ok(Box::new(app::HauntApp::new(cc, handle)))),
    );
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

/// When launched from a terminal, attach to it so `RUST_LOG` output is visible.
/// Started from Explorer there is no parent console and this does nothing.
fn attach_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: plain Win32 call without pointers.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}
