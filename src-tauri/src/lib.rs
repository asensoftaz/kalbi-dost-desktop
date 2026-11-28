//! KalBi Dost masaüstü qabığı.
//!
//! 1-ci mərhələ: dost.kalbi.pro-nu öz pəncərəsində açır, bağlananda tray-ə
//! kiçilir, Store-dan kənar versiyada özünü yeniləyir.
//! 2-ci mərhələ: körpü — bot seçilmiş qovluqlarda faylları siyahılaya,
//! oxuya, axtara və (təsdiqlə) yaza bilir (bridge.rs + bridge_ui.rs).
//!
//! Sayta (əsas pəncərə) yenə heç bir Tauri əmri açılmır: körpü komandaları
//! build.rs-də ACL altına alınıb və capabilities/bridge.json onları yalnız
//! tətbiqin öz lokal pəncərələrinə (`bridge`, `approve-*`) verir. Bot işi
//! saytdan yox, serverdən (Supabase) gəlir; təsdiq pəncərəsi tətbiqindir.
//! Ona görə sayt nə fayla çata, nə də yazmanı özü təsdiqləyə bilər.

mod bridge;
mod bridge_ui;
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
pub(crate) const TRAY_ID: &str = "main-tray";
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
        .plugin(tauri_plugin_opener::init())
        // Körpünün lokal səhifələri (qoşma, təsdiq) — .exe-nin içindən.
        .register_uri_scheme_protocol(bridge_ui::SCHEME, bridge_ui::serve)
        .invoke_handler(tauri::generate_handler![
            bridge_ui::bridge_state,
            bridge_ui::bridge_pair,
            bridge_ui::bridge_unlink,
            bridge_ui::bridge_add_folder,
            bridge_ui::bridge_remove_folder,
            bridge_ui::bridge_set_read_policy,
            bridge_ui::approval_get,
            bridge_ui::approval_decide,
        ]);

    // Store versiyasında updater plugin-i heç qeydiyyata alınmır — onu
    // təsadüfən çağırmaq belə mümkün olmasın (bax store.rs).
    if !packaged {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .setup(move |app| {
            create_main_window(app.handle(), packaged)?;
            // Körpü tray-dən əvvəl: tray menyusu körpünün açıq olub-olmadığına baxır.
            let bridge_on = bridge_ui::init(app.handle())?;
            create_tray(app.handle(), packaged, bridge_on)?;
            if !packaged {
                update::start_background(app.handle().clone());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Pəncərəni bağlamaq tətbiqdən çıxmaq deyil: bot tray-də qalır ki,
            // sonrakı mərhələlərdə gələn işləri (fayl, terminal) götürə bilsin.
            // Tam çıxış yalnız tray menyusundakı "Çıx" ilədir.
            match event {
                WindowEvent::CloseRequested { api, .. } if window.label() == MAIN => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // Təsdiq pəncərəsi X ilə bağlandı → rədd (bridge_ui).
                WindowEvent::Destroyed => {
                    bridge_ui::on_window_destroyed(window.app_handle(), window.label());
                }
                _ => {}
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

fn create_tray(app: &AppHandle, packaged: bool, bridge_on: bool) -> tauri::Result<()> {
    let t = texts();

    let open = MenuItem::with_id(app, "open", t.tray_open, true, None::<&str>)?;
    let bridge = MenuItem::with_id(app, "bridge", t.tray_bridge, true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", t.tray_check_updates, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t.tray_quit, true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    // Store versiyasında "Yeniləmələri yoxla" göstərilmir — orada yeniləməni
    // Store edir, düymə heç nə etməzdi. Körpü bu yığımda sönülüdürsə
    // (Supabase dəyişənləri yoxdur) onun bəndi də göstərilmir.
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&open];
    if bridge_on {
        items.push(&bridge);
    }
    if !packaged {
        items.push(&check);
    }
    items.push(&sep);
    items.push(&quit);
    let menu = Menu::with_items(app, &items)?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(t.tray_tooltip)
        .menu(&menu)
        // Sol klik pəncərəni açır, menyu sağ kliklədir — Windows vərdişi belədir.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "bridge" => bridge_ui::open_bridge_window(app),
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
