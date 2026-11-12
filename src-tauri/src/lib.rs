mod commands;
mod i18n;
mod monitor;
pub mod settings;
pub mod snapshot;
pub mod ssh;
mod tray;
mod tray_anim;
mod ubus;

use snapshot::AppState;
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let store = settings::SettingsStore::load(&handle);
            ssh::init_hostkeys(
                &handle.path().app_data_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                handle.clone(),
            );
            app.manage(AppState {
                settings: store,
                client: parking_lot::RwLock::new(None),
                ssh: crate::ssh::SshPool::default(),
            });
            tray::setup(&handle)?;
            app.manage(parking_lot::Mutex::new(tray_anim::AnimState::default()));
            tray_anim::start(handle.clone());
            monitor::start(handle.clone());
            // show the window on first launch so the user can enter settings
            tray::show_main_window(&handle, false);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::test_connection,
            commands::save_settings,
            commands::get_settings,
            commands::set_alias,
            commands::set_kind,
            commands::kick_client,
            commands::block_device,
            commands::reboot_radio,
            commands::reboot_router,
            commands::show_window,
            commands::ssh_fix_limited,
            commands::toggle_pin,
            commands::wifi_scan,
            commands::run_diag,
            commands::backup_config,
            commands::switch_profile,
            commands::add_profile,
            commands::delete_profile,
            commands::set_channel,
            commands::ssh_forget_host_key,
            commands::set_ui_prefs,
            commands::open_external,
            commands::acl_setup_command_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OpenWrt Companion");
}
