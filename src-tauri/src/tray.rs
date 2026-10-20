// 系统托盘：单击弹菜单（账号快捷切换 + 常规项），双击显示/隐藏主窗口。
// 菜单随账号池与 IDE 运行状态动态重建：标题带运行状态与当前账号，当前账号以 ● 标注，
// IDE 未运行时禁用该版本下的切换项（无感换号要求 IDE 在运行）。
use crate::core::logger::{log, Level, Source};
use crate::core::model::AccountBundle;
use crate::core::paths::{IdeKind, IdePaths};
use crate::core::{launcher, switch, vault};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

const TRAY_ID: &str = "main-tray";

fn ide_label(ide: IdeKind) -> &'static str {
    if ide == IdeKind::Trae {
        "Trae"
    } else {
        "Trae CN"
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    let bundles: Vec<AccountBundle> = vault::load_json()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v["accounts"].clone()).ok())
        .unwrap_or_default();

    // 子菜单（按版本）：标题带"未运行"或当前账号名；仅运行中才允许切换
    let mut submenus = Vec::new();
    for ide in [IdeKind::Trae, IdeKind::TraeCn] {
        let label = ide_label(ide);
        let running = IdePaths::detect(ide)
            .map(|p| launcher::is_running(&p))
            .unwrap_or(false);
        let current = switch::scan_current(&ide).ok().flatten();
        let cur_id = current.as_ref().map(|b| b.id.clone());
        let cur_name = current.as_ref().map(|b| b.name.clone());
        let title = if !running {
            format!("{label}（未运行）")
        } else if let Some(n) = &cur_name {
            format!("{label}（{n}）")
        } else {
            label.to_string()
        };

        let mut sub_items = Vec::new();
        for b in bundles.iter().filter(|b| b.ide == ide) {
            let is_cur = cur_id.as_deref() == Some(b.id.as_str());
            let text = if is_cur {
                format!("● {}", b.name)
            } else {
                b.name.clone()
            };
            sub_items.push(MenuItem::with_id(
                app,
                format!("switch:{}", b.id),
                text,
                running && !is_cur,
                None::<&str>,
            )?);
        }
        if sub_items.is_empty() {
            sub_items.push(MenuItem::with_id(
                app,
                format!("empty:{}", ide as u8),
                "（无账号）",
                false,
                None::<&str>,
            )?);
        }
        let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = sub_items
            .iter()
            .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
            .collect();
        submenus.push((Submenu::with_items(app, title, true, &refs)?, sub_items));
    }

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&show, &sep];
    for (sub, _) in &submenus {
        items.push(sub);
    }
    items.push(&sep);
    items.push(&settings);
    items.push(&quit);
    Menu::with_items(app, &items)
}

/// 账号池或 IDE 状态变化后重建托盘菜单（不重建托盘图标本身）
pub fn refresh(app: &AppHandle) {
    if let Ok(menu) = build_menu(app) {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let _tray = TrayIconBuilder::with_id(TRAY_ID)
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
                        let bundles: Vec<AccountBundle> = vault::load_json()
                            .ok()
                            .flatten()
                            .and_then(|v| serde_json::from_value(v["accounts"].clone()).ok())
                            .unwrap_or_default();
                        if let Some(b) = bundles.iter().find(|b| b.id == acc_id) {
                            match switch::hot_switch_full(&b.ide, b, acc_id) {
                                Ok(_) => {
                                    log(Level::Info, Source::Switch, format!("托盘无感换号到 {}", b.name));
                                    let _ = app.emit("account-switched", serde_json::json!({ "to": b.name }));
                                    // 当前账号已变 → 立即刷新菜单标注
                                    refresh(app);
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
