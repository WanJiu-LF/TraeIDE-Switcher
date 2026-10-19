// 系统托盘：单击弹菜单（账号快捷切换 + 常规项），双击显示/隐藏主窗口
use crate::core::logger::{log, Level, Source};
use crate::core::switch;
use crate::core::vault;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    let bundles: Vec<crate::core::model::AccountBundle> = vault::load_json()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v["accounts"].clone()).ok())
        .unwrap_or_default();

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&show, &sep];
    let mut submenus = Vec::new();
    for ide_label in ["Trae", "Trae CN"] {
        let ide_kind = if ide_label == "Trae" {
            crate::core::paths::IdeKind::Trae
        } else {
            crate::core::paths::IdeKind::TraeCn
        };
        let mut sub_items = Vec::new();
        for b in bundles.iter().filter(|b| b.ide == ide_kind) {
            sub_items.push(MenuItem::with_id(
                app,
                format!("switch:{}", b.id),
                b.name.clone(),
                true,
                None::<&str>,
            )?);
        }
        if sub_items.is_empty() {
            continue;
        }
        let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
            sub_items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
        submenus.push((Submenu::with_items(app, ide_label, true, &refs)?, sub_items));
    }
    for (sub, _) in &submenus {
        items.push(sub);
    }
    items.push(&sep);
    items.push(&settings);
    items.push(&quit);
    Menu::with_items(app, &items)
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let _tray = TrayIconBuilder::with_id("main-tray")
        .tooltip("TraeIDE-Switcher")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                if let Some(win) = tray.app_handle().get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        })
        .on_menu_event(|app: &AppHandle, event| {
            let id = event.id().as_ref();
            match id {
                "show" => {
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
                "settings" => {
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                        let _ = app.emit("ui-navigate", "settings");
                    }
                }
                "quit" => {
                    app.exit(0);
                }
                _ => {
                    if let Some(acc_id) = id.strip_prefix("switch:") {
                        let bundles: Vec<crate::core::model::AccountBundle> = vault::load_json()
                            .ok()
                            .flatten()
                            .and_then(|v| serde_json::from_value(v["accounts"].clone()).ok())
                            .unwrap_or_default();
                        if let Some(b) = bundles.iter().find(|b| b.id == acc_id) {
                            match switch::hot_switch_full(&b.ide, b, acc_id) {
                                Ok(_) => {
                                    log(Level::Info, Source::Switch, format!("托盘无感换号到 {}", b.name));
                                    let _ = app.emit("account-switched", serde_json::json!({ "to": b.name }));
                                }
                                Err(e) => {
                                    log(Level::Err, Source::Switch, format!("托盘切换失败：{e}"));
                                }
                            }
                        }
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}
