use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open GIAP").build(app)?;
    let summon = MenuItemBuilder::with_id("summon", "Summon (Voice)").build(app)?;
    let canvas = MenuItemBuilder::with_id("canvas", "Show Canvas").build(app)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&open, &summon, &canvas, &separator, &quit])
        .build()?;

    // Load tray icon from the icons directory if it exists, otherwise use a default.
    // Run `npx @tauri-apps/cli icon` to generate all icon sizes.
    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("Goose In A Pond")
        .menu(&menu);

    // Attempt to load tray icon; if missing, Tauri will use an OS default
    let icon_path = app
        .path()
        .resource_dir()
        .ok()
        .map(|d| d.join("icons").join("tray-icon.png"));

    if let Some(path) = icon_path.filter(|p| p.exists()) {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(icon) = tauri::image::Image::from_bytes(&bytes) {
                builder = builder.icon(icon);
            }
        }
    }

    builder
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "summon" => {
                let _ = app.emit(crate::hotkey::DESKTOP_SUMMON_EVENT, ());
            }
            "canvas" => tray_show_canvas(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click on tray icon shows/restores main window
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(&tray.app_handle().clone());
            }
        })
        .build(app)?;

    Ok(())
}

pub fn set_tray_tooltip(app: &AppHandle, status: &str) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(&format!("Goose In A Pond — {}", status)));
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn tray_show_canvas(app: &AppHandle) {
    show_main_window(app);
    let _ = app.emit(crate::hotkey::CANVAS_TOGGLE_EVENT, ());
}
