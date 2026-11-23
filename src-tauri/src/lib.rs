//! KalBi Dost masaüstü qabığı — 1-ci mərhələ.
//!
//! Nə edir: dost.kalbi.pro-nu öz pəncərəsində açır, bağlananda tray-ə
//! kiçilir, Store-dan kənar versiyada özünü yeniləyir.
//!
//! Nə ETMİR (qəsdən): sayta heç bir Tauri əmri (IPC) açılmır — capabilities
//! qovluğu yoxdur. Uzaq sayt nə fayla, nə terminala çata bilər. Körpü
//! 2-ci mərhələdə ayrıca, yalnız dost.kalbi.pro üçün və təsdiq pəncərəsi ilə
//! əlavə olunacaq. Ona görə bu mərhələdə hücum səthi adi brauzer tabı qədərdir.

mod i18n;
mod store;
mod update;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use crate::i18n::texts;

const MAIN: &str = "main";
const START_URL: &str = "https://dost.kalbi.pro";

pub fn run() {
    let packaged = store::is_packaged();

    let mut builder = tauri::Builder::default()
        // Single-instance İLK plugin olmalıdır: ikinci nüsxə digər
        // plugin-ləri işə salmadan çıxsın. İkinci dəfə açanda (məs. Start
        // menyusundan) yeni pəncərə yox, mövcud pəncərə önə gəlir.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());

    // Store versiyasında updater plugin-i heç qeydiyyata alınmır — onu
    // təsadüfən çağırmaq belə mümkün olmasın (bax store.rs).
    if !packaged {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .setup(move |app| {
            create_main_window(app.handle(), packaged)?;
            create_tray(app.handle(), packaged)?;
            if !packaged {
                update::start_background(app.handle().clone());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Pəncərəni bağlamaq tətbiqdən çıxmaq deyil: bot tray-də qalır ki,
            // sonrakı mərhələlərdə gələn işləri (fayl, terminal) götürə bilsin.
            // Tam çıxış yalnız tray menyusundakı "Çıx" ilədir.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == MAIN {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("KalBi Dost işə düşmədi");
}

fn create_main_window(app: &AppHandle, packaged: bool) -> tauri::Result<()> {
    let url: Url = START_URL.parse().expect("START_URL düzgün URL-dir");
    let version = app.package_info().version.to_string();

    // Sayt tətbiqin içində açıldığını bilsin (məs. "Tətbiqi endir" düyməsini
    // gizlətmək üçün). Niyə user-agent yox: UA-nı əvəz etsək, WebView2-nin
    // öz Edge versiyası itər və saytlar köhnə brauzer sanar. Bu skript yalnız
    // əsas çərçivədə işləyir, iframe-lərdə yox. Burada sirr yoxdur — sadəcə
    // bayraqdır, ona görə üçüncü saytların da görməsi zərərsizdir.
    let init = format!(
        "Object.defineProperty(window,'kalbiDesktop',{{value:Object.freeze({{platform:'windows',version:'{version}',store:{packaged}}}),writable:false,configurable:false}});"
    );

    let handle = app.clone();
    WebviewWindowBuilder::new(app, MAIN, WebviewUrl::External(url))
        .title("KalBi Dost")
        .inner_size(1200.0, 800.0)
        .min_inner_size(420.0, 600.0)
        .center()
        .initialization_script(init)
        // target="_blank" və window.open: tətbiqin içində ikinci, idarəsiz
        // pəncərə açmırıq — link istifadəçinin adi brauzerində açılır.
        // Yalnız http/https; başqa sxemlər (file:, ms-settings: və s.)
        // səssizcə rədd olunur ki, sayt sistemdə bir şey işə sala bilməsin.
        .on_new_window(move |url, _features| {
            if matches!(url.scheme(), "http" | "https") {
                let _ = handle.opener().open_url(url.as_str(), None::<&str>);
            }
            NewWindowResponse::Deny
        })
        .build()?;
    Ok(())
}

fn create_tray(app: &AppHandle, packaged: bool) -> tauri::Result<()> {
    let t = texts();

    let open = MenuItem::with_id(app, "open", t.tray_open, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t.tray_quit, true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    // Store versiyasında "Yeniləmələri yoxla" göstərilmir — orada yeniləməni
    // Store edir, düymə heç nə etməzdi.
    let menu = if packaged {
        Menu::with_items(app, &[&open, &sep, &quit])?
    } else {
        let check = MenuItem::with_id(app, "check", t.tray_check_updates, true, None::<&str>)?;
        Menu::with_items(app, &[&open, &check, &sep, &quit])?
    };

    let mut tray = TrayIconBuilder::with_id("main-tray")
        .tooltip(t.tray_tooltip)
        .menu(&menu)
        // Sol klik pəncərəni açır, menyu sağ kliklədir — Windows vərdişi belədir.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "check" => update::check_manual(app.clone()),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Pəncərəni tray-dən qaytarır. Kiçildilibsə açır, önə gətirir.
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}
