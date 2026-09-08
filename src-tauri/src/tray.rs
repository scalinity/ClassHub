//! SPEC §12 — the tray: the next meeting and the shift's state, with `Open
//! ClassHub`, `Run the shift now`, `Pause tonight` and `Quit`. Closing the
//! window hides it, so the shift's thread survives the window; this is the
//! way back, beside the dock.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

const TRAY_ID: &str = "main";

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(glyph())
        .icon_as_template(true)
        .tooltip("ClassHub")
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "run" => {
                if let Err(e) = crate::shift::run_now(app) {
                    eprintln!("shift: not started from the tray — {e:#}");
                }
            }
            "pause" | "resume" => {
                let paused = event.id().as_ref() == "pause";
                if let Err(e) = crate::shift::pause_tonight(app, paused) {
                    eprintln!("shift: pause not recorded — {e:#}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    refresh(app);
    Ok(())
}

/// Rebuilds the menu from what is true now. Menus are the main thread's, so
/// the shift's and the scheduler's threads hand it over.
pub fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(e) = rebuild(&handle) {
            eprintln!("tray: menu not rebuilt — {e}");
        }
    });
}

/// What the menu last showed, so a minute that changed nothing rebuilds
/// nothing: the scheduler asks every tick, and most ticks are alike.
static LAST_MENU: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn rebuild(app: &AppHandle) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let summary = crate::shift::tray_summary(app);
    let (pause_id, pause_label) = if summary.paused {
        ("resume", "Resume tonight")
    } else {
        ("pause", "Pause tonight")
    };
    let shown = format!(
        "{}\n{}\n{}\n{}\n{:?}",
        summary.next_meeting, summary.shift, pause_id, summary.can_run, summary.title
    );
    {
        let mut last = crate::db::lock(&LAST_MENU);
        if *last == shown {
            return Ok(());
        }
        *last = shown;
    }
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "next", &summary.next_meeting, false, None::<&str>)?,
            &MenuItem::with_id(app, "shift", &summary.shift, false, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "open", "Open ClassHub", true, None::<&str>)?,
            &MenuItem::with_id(app, "run", "Run the shift now", summary.can_run, None::<&str>)?,
            &MenuItem::with_id(app, pause_id, pause_label, true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit ClassHub", true, None::<&str>)?,
        ],
    )?;
    tray.set_menu(Some(menu))?;
    tray.set_title(summary.title.as_deref())?;
    Ok(())
}

/// Shows the window a close hid, and brings it forward.
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The menu bar glyph: a ring open on the right — a C — drawn as a template
/// image, so macOS inks it for the light and the dark menu bar alike.
fn glyph() -> Image<'static> {
    const SIZE: u32 = 36;
    let centre = SIZE as f32 / 2.0;
    let (outer, inner) = (15.0_f32, 10.0_f32);
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage =
                (outer + 0.5 - distance).clamp(0.0, 1.0) * (distance - inner + 0.5).clamp(0.0, 1.0);
            // The opening: within about 30° of the right-hand axis.
            let open = dy.atan2(dx).abs() < 0.55;
            let alpha = if open { 0.0 } else { coverage };
            rgba.extend_from_slice(&[0, 0, 0, (alpha * 255.0).round() as u8]);
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}
