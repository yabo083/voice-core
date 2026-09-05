//! `VoiceCore.exe`: the one thing a user launches.
//!
//! Everything else in the install is something this process starts. The window is
//! a panel, not the app — closing it hides it to the tray, and only Quit ends the
//! process, after stopping the children. That asymmetry is the whole reason a
//! single supervisor exists: before this, the backend, the subtitle dialog and the
//! provisioning script were three things a user had to start in the right order.

// A GUI has no console to write to, and a console window flashing behind it is
// what made the old launcher look like a script. Debug builds keep the console:
// panics have to go somewhere while the app is being worked on.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod asset;
mod caption;
mod config_edit;
mod config_view;
mod contract;
mod detect;
mod host;
mod jobobj;
mod jsonstream;
mod layout;
mod provision;
mod runtime_api;
mod shell;
mod supervise;
mod training;
mod usage;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use host::Host;

fn main() {
    let app = tauri::Builder::default()
        // First, deliberately. Everything after this line assumes it is the only
        // instance: two GUIs would mean two tray icons and two supervisors racing
        // for one port and one data directory.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // The second launch is a user asking for the panel, not for a second
            // app. This is the callback that makes clicking the shortcut twice
            // behave like clicking it once.
            show_panel(app);
        }))
        // Registered for its Rust API only. Its JS commands are granted to nobody,
        // so a file dialog can only be opened by `shell::pick_folder`/`pick_file`.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(Host::new());
            build_tray(app.handle())?;
            supervise::watch(app.handle().clone());
            // Before the first paint, so the caption is never briefly the system's.
            if let Some(window) = app.get_webview_window("main") {
                caption::paint(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Wallpaper Engine's contract: the X button puts the panel away and
                // leaves the thing running. Quit is in the tray menu.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            detect::detect,
            provision::provision,
            provision::cancel_provision,
            shell::pick_folder,
            shell::pick_file,
            runtime_api::runtime_status,
            runtime_api::list_voices,
            config_edit::register_pack,
            config_edit::remove_pack,
            config_edit::import_avatar,
            asset::pack_avatar,
            config_view::pack_manifest_file,
            config_view::pack_effective,
            config_view::settings_read,
            config_view::settings_history,
            config_view::pack_config,
            config_view::speak_preview,
            config_edit::settings_write,
            config_edit::settings_restore,
            config_edit::pack_config_write,
            supervise::start_stack,
            supervise::stop_stack,
            usage::resource_usage,
            shell::open_path,
            training::training_runs,
            training::training_scratch,
            training::training_log,
            training::install_trained_pack,
            training::training_discard,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        // A startup failure that only reached stderr would be invisible: there is
        // no console. Same reason the runtime writes its own startup failures to
        // `logs/runtime.err.log`.
        Err(err) => {
            Host::new().log(&format!("startup failed: {err}"));
            panic!("failed to start VoiceCore: {err}");
        }
    };

    app.run(|_app, event| {
        // `code` is `None` for every user-driven exit — including the last window
        // closing, which here means "the panel was hidden" — and `Some` only for
        // `AppHandle::exit`, which is what the tray's Quit calls. So this single
        // arm is the difference between hiding and quitting.
        //
        // Nothing needs to stop the children on `RunEvent::Exit`: the job object's
        // last handle closes with the process, and KILL_ON_JOB_CLOSE does the rest
        // even if this process is killed rather than asked to leave.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn show_panel(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开面板", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动后端", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止后端", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    // Two separator instances rather than one used twice: a menu item belongs to
    // one position in one menu.
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &start,
            &stop,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    // Start and stop stay enabled in both states: starting what runs and stopping
    // what does not are both no-ops, and a menu whose entries grey out needs the
    // supervisor to push state into the tray for no gain.
    let mut tray = TrayIconBuilder::with_id("voice-core")
        .tooltip("voice-core")
        .menu(&menu)
        // Left click opens the panel — the Windows convention. The menu is the
        // right-click gesture.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_panel(app),
            "start" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = supervise::start(&app).await {
                        app.state::<Host>().log(&format!("start failed: {err}"));
                    }
                });
            }
            "stop" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = supervise::stop_stack(app).await;
                });
            }
            // The children first, then us. `exit` is also what makes the run loop
            // stop preventing exit.
            "quit" => {
                supervise::stop(&app.state::<Host>());
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
            }
        });

    // The same icon the window and the executable carry, decoded at build time by
    // `generate_context!` from `icons/icon.ico`.
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}
