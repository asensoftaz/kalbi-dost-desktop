//! Store-dan kənar (.exe) versiyanın özünü yeniləməsi.
//!
//! Necə işləyir: GitHub Releases-dəki `latest.json` oxunur (ünvan
//! tauri.conf.json → plugins.updater.endpoints). Yeni versiya varsa
//! istifadəçidən soruşulur, razı olsa NSIS quraşdırıcısı endirilir,
//! imzası açıq açarla yoxlanılır və "passive" rejimdə quraşdırılır
//! (yalnız irəliləmə zolağı görünür, sonra tətbiq özü yenidən açılır).
//!
//! Niyə soruşuruq, səssiz quraşdırmırıq: quraşdırma tətbiqi bağlayır.
//! İstifadəçi bota yazarkən pəncərənin birdən bağlanması mətnin itməsi deməkdir.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

use crate::i18n::texts;

/// Eyni anda iki yoxlama getməsin (məs. avtomatik yoxlama gedərkən
/// istifadəçi tray-dən "yoxla" basarsa — iki pəncərə açılardı).
static CHECKING: AtomicBool = AtomicBool::new(false);

/// İstifadəçi "Sonra" dediyi versiya. Avtomatik yoxlama bu versiya üçün
/// bu sessiyada bir daha soruşmur (6 saatdan bir eyni sual zəhlətökən olar).
/// Tray-dən əl ilə yoxlama isə yenə soruşur — istifadəçi özü istəyib.
static DECLINED: Mutex<Option<String>> = Mutex::new(None);

/// İlk yoxlamadan əvvəl gözləmə: tətbiq açılışda saytı yükləyir,
/// şəbəkəni eyni anda yeniləmə sorğusu ilə yükləməyək.
const FIRST_DELAY: Duration = Duration::from_secs(15);
/// Sonrakı yoxlamalar arası. Tətbiq tray-də günlərlə aça bilər.
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Fon yoxlamasını başladır. Ayrıca OS axını (thread) işlədirik, çünki
/// dialoqun `blocking_show`-u əsas axında çağırıla bilməz, fon axınında isə
/// kod sadə və ardıcıl qalır.
pub fn start_background(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        loop {
            check(&app, false);
            std::thread::sleep(INTERVAL);
        }
    });
}

/// Tray menyusundan əl ilə yoxlama. Fon axınında işləyir ki, menyu donmasın.
pub fn check_manual(app: AppHandle) {
    std::thread::spawn(move || check(&app, true));
}

fn check(app: &AppHandle, manual: bool) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    run_check(app, manual);
    CHECKING.store(false, Ordering::SeqCst);
}

fn run_check(app: &AppHandle, manual: bool) {
    let t = texts();

    let result = tauri::async_runtime::block_on(async {
        let updater = app.updater()?;
        updater.check().await
    });

    let update = match result {
        Ok(Some(u)) => u,
        Ok(None) => {
            if manual {
                info(app, t.up_to_date, MessageDialogKind::Info);
            }
            return;
        }
        Err(e) => {
            // Avtomatik yoxlamada xətanı göstərmirik: internet yoxdursa
            // istifadəçi hər 6 saatdan bir xəta pəncərəsi görməməlidir.
            log_err("yeniləmə yoxlanışı", &e);
            if manual {
                info(app, t.update_failed, MessageDialogKind::Error);
            }
            return;
        }
    };

    if !manual {
        let declined = DECLINED.lock().map(|d| d.clone()).unwrap_or(None);
        if declined.as_deref() == Some(update.version.as_str()) {
            return;
        }
    }

    let agreed = app
        .dialog()
        .message(t.update_available.replace("{v}", &update.version))
        .title(t.update_title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            t.update_now.to_string(),
            t.update_later.to_string(),
        ))
        .blocking_show();

    if !agreed {
        if let Ok(mut d) = DECLINED.lock() {
            *d = Some(update.version.clone());
        }
        return;
    }

    // Uğurlu olsa Windows-da bu funksiya geri qayıtmır: plugin quraşdırıcını
    // işə salıb tətbiqi özü bağlayır (`/R` ilə quraşdırıcı onu yenidən açır).
    let res = tauri::async_runtime::block_on(update.download_and_install(|_, _| {}, || {}));
    if let Err(e) = res {
        log_err("yeniləmənin quraşdırılması", &e);
        info(app, t.update_failed, MessageDialogKind::Error);
    }
}

fn info(app: &AppHandle, msg: &str, kind: MessageDialogKind) {
    let t = texts();
    app.dialog()
        .message(msg)
        .title(t.update_title)
        .kind(kind)
        .buttons(MessageDialogButtons::OkCustom(t.ok.to_string()))
        .blocking_show();
}

/// Release yığımında konsol yoxdur; xəta debug yığımında görünsün deyə
/// eprintln. (Log faylı 2-ci mərhələdə körpü ilə birlikdə gələcək.)
fn log_err(what: &str, e: &tauri_plugin_updater::Error) {
    eprintln!("[kalbi-dost] {what} alınmadı: {e}");
}
