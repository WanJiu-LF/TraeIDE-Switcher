pub mod core;
mod commands;
mod tray;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::list_accounts,
            commands::scan_local_account,
            commands::import_local_account,
            commands::add_account,
            commands::delete_account,
            commands::update_account,
            commands::set_account_pinned,
            commands::export_vault_backup,
            commands::list_vault_backups,
            commands::import_vault_backup,
            commands::export_portable,
            commands::import_portable,
            commands::query_quota,
            commands::usage_history,
            commands::switch_history,
            commands::account_devices,
            commands::launch_isolated_instance,
            commands::session_usage,
            commands::query_assets,
            commands::claim_activity_rewards,
            commands::draw_checkin_lottery,
            commands::icdrs_log,
            commands::verify_account,
            commands::runtime_config,
            commands::shim_commands,
            commands::shim_invoke,
            commands::query_checkin,
            commands::claim_checkin,
            commands::refresh_account,
            commands::switch_account,
            commands::switch_isolated,
            commands::cold_switch_restart,
            commands::inject_bridge,
            commands::remove_bridge,
            commands::bridge_status,
            commands::shim_install,
            commands::shim_uninstall,
            commands::shim_status,
            commands::start_login,
            commands::poll_login,
            commands::cancel_login,
            commands::launch_ide,
            commands::ide_status,
            commands::registry_deviceid,
            commands::install_paths,
            commands::get_logs,
            commands::export_logs,
            commands::get_settings,
            commands::update_settings,
            commands::app_update,
            commands::autostart,
        ])
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            crate::core::logger::init();
            crate::core::logger::set_app(app.handle().clone());
            crate::core::logger::log(
                crate::core::logger::Level::Info,
                crate::core::logger::Source::System,
                "TraeIDE-Switcher 启动",
            );
            let handle = app.handle().clone();
            std::thread::spawn(move || crate::core::autoswitch::run(handle));
            tray::setup(app.handle())?;
            // 关闭到托盘：CloseRequested 拦截为隐藏
            if let Some(win) = tauri::Manager::get_webview_window(app, "main") {
                let h = app.handle().clone();
                win.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        if crate::core::settings::load().close_to_tray {
                            api.prevent_close();
                            if let Some(w) = h.get_webview_window("main") {
                                let _ = w.hide();
                            }
                            crate::core::logger::log(
                                crate::core::logger::Level::Info,
                                crate::core::logger::Source::System,
                                "窗口关闭，最小化到托盘",
                            );
                        }
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
