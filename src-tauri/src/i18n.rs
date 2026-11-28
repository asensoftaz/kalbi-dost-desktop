//! Tətbiqin öz (doğma) mətnləri: tray menyusu və yeniləmə pəncərələri.
//!
//! Niyə ayrıca fayl və niyə JSON yox: mətnlər azdır, hamısı kompilyasiya
//! vaxtı yoxlanılsın deyə Rust-da saxlanılır — tərcümə əskik qalsa, yığım
//! dayanır (struct-un bütün sahələri doldurulmalıdır).
//! Saytın özü (dost.kalbi.pro) öz dilini idarə edir; bura yalnız pəncərədən
//! kənardakı hissələrdir.

pub struct Texts {
    pub tray_tooltip: &'static str,
    pub tray_open: &'static str,
    pub tray_check_updates: &'static str,
    pub tray_quit: &'static str,
    pub update_title: &'static str,
    /// `{v}` yeni versiya ilə əvəz olunur.
    pub update_available: &'static str,
    pub update_now: &'static str,
    pub update_later: &'static str,
    pub up_to_date: &'static str,
    pub update_failed: &'static str,
    pub ok: &'static str,
    // --- Körpü (2-ci mərhələ) ---
    /// Tray menyusu: qoşma/qovluqlar pəncərəsi.
    pub tray_bridge: &'static str,
    pub bridge_title: &'static str,
    /// Təsdiq pəncərəsinin başlığı ("<bot> — icazə istəyir").
    pub approve_title: &'static str,
    pub pick_folder: &'static str,
    /// Tray ipucunda körpünün vəziyyəti.
    pub st_online: &'static str,
    pub st_offline: &'static str,
    pub st_revoked: &'static str,
}

const AZ: Texts = Texts {
    tray_tooltip: "KalBi Dost",
    tray_open: "Aç",
    tray_check_updates: "Yeniləmələri yoxla",
    tray_quit: "Çıx",
    update_title: "KalBi Dost — yeniləmə",
    update_available: "Yeni versiya hazırdır: {v}.\n\nİndi yenilənsin? Tətbiq bir neçə saniyəlik bağlanıb yenidən açılacaq.",
    update_now: "Yenilə",
    update_later: "Sonra",
    up_to_date: "Ən son versiya quraşdırılıb.",
    update_failed: "Yeniləməni yoxlamaq alınmadı. İnternet bağlantısını yoxlayıb sonra yenidən cəhd edin.",
    ok: "Oldu",
    tray_bridge: "Kompüter körpüsü…",
    bridge_title: "KalBi Dost — kompüter körpüsü",
    approve_title: "icazə istəyir",
    pick_folder: "Bot üçün qovluq seçin",
    st_online: "kompüter qoşulub",
    st_offline: "kompüter oflayndır",
    st_revoked: "kompüter ayrılıb",
};

const TR: Texts = Texts {
    tray_tooltip: "KalBi Dost",
    tray_open: "Aç",
    tray_check_updates: "Güncellemeleri denetle",
    tray_quit: "Çıkış",
    update_title: "KalBi Dost — güncelleme",
    update_available: "Yeni sürüm hazır: {v}.\n\nŞimdi güncellensin mi? Uygulama birkaç saniyeliğine kapanıp yeniden açılacak.",
    update_now: "Güncelle",
    update_later: "Sonra",
    up_to_date: "En son sürüm yüklü.",
    update_failed: "Güncelleme denetlenemedi. İnternet bağlantınızı kontrol edip daha sonra tekrar deneyin.",
    ok: "Tamam",
    tray_bridge: "Bilgisayar köprüsü…",
    bridge_title: "KalBi Dost — bilgisayar köprüsü",
    approve_title: "izin istiyor",
    pick_folder: "Bot için klasör seçin",
    st_online: "bilgisayar bağlı",
    st_offline: "bilgisayar çevrimdışı",
    st_revoked: "bilgisayar ayrıldı",
};

const RU: Texts = Texts {
    tray_tooltip: "KalBi Dost",
    tray_open: "Открыть",
    tray_check_updates: "Проверить обновления",
    tray_quit: "Выход",
    update_title: "KalBi Dost — обновление",
    update_available: "Доступна новая версия: {v}.\n\nОбновить сейчас? Приложение закроется на несколько секунд и откроется снова.",
    update_now: "Обновить",
    update_later: "Позже",
    up_to_date: "Установлена последняя версия.",
    update_failed: "Не удалось проверить обновления. Проверьте подключение к интернету и повторите попытку позже.",
    ok: "ОК",
    tray_bridge: "Мост с компьютером…",
    bridge_title: "KalBi Dost — мост с компьютером",
    approve_title: "запрашивает разрешение",
    pick_folder: "Выберите папку для бота",
    st_online: "компьютер подключён",
    st_offline: "компьютер не в сети",
    st_revoked: "компьютер отключён",
};

const EN: Texts = Texts {
    tray_tooltip: "KalBi Dost",
    tray_open: "Open",
    tray_check_updates: "Check for updates",
    tray_quit: "Quit",
    update_title: "KalBi Dost — update",
    update_available: "A new version is ready: {v}.\n\nUpdate now? The app will close for a few seconds and reopen.",
    update_now: "Update",
    update_later: "Later",
    up_to_date: "You have the latest version.",
    update_failed: "Could not check for updates. Check your internet connection and try again later.",
    ok: "OK",
    tray_bridge: "Computer bridge…",
    bridge_title: "KalBi Dost — computer bridge",
    approve_title: "asks for permission",
    pick_folder: "Choose a folder for the bot",
    st_online: "computer connected",
    st_offline: "computer offline",
    st_revoked: "computer unlinked",
};

const ZH: Texts = Texts {
    tray_tooltip: "KalBi Dost",
    tray_open: "打开",
    tray_check_updates: "检查更新",
    tray_quit: "退出",
    update_title: "KalBi Dost — 更新",
    update_available: "新版本已就绪：{v}。\n\n现在更新吗？应用将关闭几秒钟后重新打开。",
    update_now: "更新",
    update_later: "稍后",
    up_to_date: "已是最新版本。",
    update_failed: "无法检查更新。请检查网络连接后稍后重试。",
    ok: "确定",
    tray_bridge: "电脑桥接…",
    bridge_title: "KalBi Dost — 电脑桥接",
    approve_title: "请求权限",
    pick_folder: "为机器人选择文件夹",
    st_online: "电脑已连接",
    st_offline: "电脑离线",
    st_revoked: "电脑已断开",
};

/// Sistem dilinə görə mətnlər. Windows "az-Latn-AZ", "tr-TR", "zh-Hans-CN"
/// kimi teq qaytarır — yalnız ilk hissəyə (dil koduna) baxırıq.
/// Tanınmayan dil → ingiliscə (beynəlxalq istifadəçi üçün ən anlaşıqlısı).
pub fn texts() -> &'static Texts {
    match lang_code() {
        "az" => &AZ,
        "tr" => &TR,
        "ru" => &RU,
        "zh" => &ZH,
        _ => &EN,
    }
}

/// Dil kodu — lokal HTML səhifələri (körpü, təsdiq) eyni dili seçsin deyə
/// `?lang=` ilə ötürülür. Mətnlər səhifənin özündədir.
pub fn lang_code() -> &'static str {
    let locale = sys_locale::get_locale().unwrap_or_default().to_lowercase();
    match locale.split(['-', '_']).next().unwrap_or("") {
        "az" => "az",
        "tr" => "tr",
        "ru" => "ru",
        "zh" => "zh",
        _ => "en",
    }
}
