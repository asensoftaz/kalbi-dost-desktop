//! Körpünün pəncərə tərəfi: `bridge.rs` (məntiq) ↔ Tauri (pəncərələr, tray).
//!
//! İki lokal pəncərə var, hər ikisi tətbiqin İÇİNDƏN gələn HTML ilə
//! (`kdost://` sxemi, fayllar .exe-yə `include_str!` ilə yığılır):
//!   • `bridge`      — qoşma kodu, qovluqlar, status, "ayır" (tray → "Kompüter körpüsü").
//!   • `approve-<id>` — bir iş üçün təsdiq pəncərəsi (həmişə üstdə, vaxtı bitəndə bağlanır).
//!
//! NİYƏ bu qədər qat (vacib):
//!   Tauri `frontendDist`-in (https://dost.kalbi.pro) altındakı səhifələri
//!   "lokal" mənbə sayır. Tətbiq komandaları üçün icazə siyahısı (build.rs →
//!   AppManifest) olmasaydı, sayt — və ya ona yeridilmiş skript — körpü
//!   komandalarını çağırıb yazmanı özü "təsdiqləyə" bilərdi. Ona görə:
//!     1. build.rs komandaları ACL altına alır; capabilities/bridge.json onları
//!        YALNIZ `bridge` və `approve-*` pəncərələrinə verir (`main` yoxdur).
//!     2. Hər komanda çağıran pəncərənin adını özü də yoxlayır (ikinci qat).
//!     3. Təsdiq qərarı yalnız həmin işin öz pəncərəsindən qəbul olunur.
//!     4. `kdost://` səhifələri yalnız bu pəncərələrə verilir (başqa webview → 403),
//!        bu pəncərələr isə başqa ünvana keçə bilmir (on_navigation).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::http::{header, Response, StatusCode};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Runtime, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::oneshot;

use crate::bridge::{
    ApprovalRequest, BoxFuture, Bridge, Config, Decision, FolderError, Host, PairError, ReadPolicy,
    Status,
};
use crate::i18n::{lang_code, texts};

pub const SCHEME: &str = "kdost";
pub const BRIDGE_LABEL: &str = "bridge";
const APPROVE_PREFIX: &str = "approve-";
const ACTIVITY_MAX: usize = 30;

const BRIDGE_HTML: &str = include_str!("../ui/bridge.html");
const APPROVE_HTML: &str = include_str!("../ui/approve.html");

/// Lokal səhifələrin CSP-si: yalnız öz daxili skript/stil; şəbəkəyə çıxış
/// yalnız Tauri IPC-yə (Windows-da http://ipc.localhost). Kənar skript,
/// şəkil, iframe — heç biri yüklənmir.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   img-src data:; connect-src ipc: http://ipc.localhost; base-uri 'none'; \
                   form-action 'none'; frame-ancestors 'none'";

// =====================================================================
// Vəziyyət (Tauri State)
// =====================================================================

struct Pending {
    req: ApprovalRequest,
    tx: Option<oneshot::Sender<Decision>>,
}

type PendingMap = Arc<Mutex<HashMap<String, Pending>>>;

/// Tauri-də `app.manage(...)` ilə saxlanılır. `bridge` None-dırsa körpü
/// bu yığımda sönülüdür (Supabase dəyişənləri verilməyib).
pub struct BridgeUi {
    pub bridge: Option<Bridge>,
    host: Arc<DesktopHost>,
}

pub struct DesktopHost {
    app: AppHandle,
    pending: PendingMap,
    activity: Mutex<VecDeque<String>>,
}

impl DesktopHost {
    /// Təsdiq pəncərəsi istifadəçi tərəfindən (X ilə) bağlandı → rədd sayılır:
    /// göndərən silinir, gözləyən future `Err` alır → Deny.
    pub fn forget(&self, label: &str) {
        self.pending.lock().unwrap().remove(label);
    }
}

impl Host for DesktopHost {
    fn approve(&self, req: ApprovalRequest) -> BoxFuture<Decision> {
        let app = self.app.clone();
        let pending = self.pending.clone();
        Box::pin(async move {
            let label = format!("{APPROVE_PREFIX}{}", label_safe(&req.job_id));
            let (tx, rx) = oneshot::channel();
            pending
                .lock()
                .unwrap()
                .insert(label.clone(), Pending { req: req.clone(), tx: Some(tx) });
            // Guard: future vaxt bitdiyinə görə atılsa (bridge.rs timeout) və ya
            // cavab gəlsə — pəncərə hər halda bağlanır, qeyd silinir.
            let _guard = WindowGuard { app: app.clone(), label: label.clone(), pending };
            if let Err(e) = open_approve_window(&app, &label, &req) {
                eprintln!("[körpü] təsdiq pəncərəsi açılmadı: {e}");
                return Decision::Deny;
            }
            rx.await.unwrap_or(Decision::Deny)
        })
    }

    fn status(&self, status: Status) {
        let t = texts();
        let tip = match &status {
            Status::Online { .. } => format!("{} — {}", t.tray_tooltip, t.st_online),
            Status::Offline | Status::Connecting => format!("{} — {}", t.tray_tooltip, t.st_offline),
            Status::Revoked => format!("{} — {}", t.tray_tooltip, t.st_revoked),
            Status::Unpaired => t.tray_tooltip.to_string(),
        };
        if let Some(tray) = self.app.tray_by_id(crate::TRAY_ID) {
            let _ = tray.set_tooltip(Some(tip));
        }
    }

    fn activity(&self, line: String) {
        let stamp = chrono::Local::now().format("%H:%M");
        let mut a = self.activity.lock().unwrap();
        a.push_front(format!("{stamp}  {line}"));
        a.truncate(ACTIVITY_MAX);
    }

    /// Fayl → öz proqramında (Windows "aç" kimi), link → standart brauzer.
    /// Yol bridge.rs-də seçilmiş qovluğun içində olduğu yoxlanıb; link yalnız http(s).
    fn open(&self, target: &str, is_url: bool) -> Result<(), String> {
        let r = if is_url {
            tauri_plugin_opener::open_url(target, None::<&str>)
        } else {
            tauri_plugin_opener::open_path(target, None::<&str>)
        };
        r.map_err(|e| e.to_string())
    }

    fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        let title = if title.trim().is_empty() { "KalBi Dost" } else { title };
        self.app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    }
}

struct WindowGuard {
    app: AppHandle,
    label: String,
    pending: PendingMap,
}

impl Drop for WindowGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.label);
        if let Some(w) = self.app.get_webview_window(&self.label) {
            // destroy (close yox): close CloseRequested göndərir; burada
            // heç bir sual olmadan dərhal bağlanmalıdır.
            let _ = w.destroy();
        }
    }
}

/// Pəncərə adına yalnız hərf, rəqəm, `-`, `_` (iş id-si uuid-dir, amma
/// bazadan gələn mətnə kor-koranə güvənmirik).
fn label_safe(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

// =====================================================================
// Quraşdırma (lib.rs → setup)
// =====================================================================

/// Körpünü yaradır və fon dövrünü başladır. Dəyişənlər yoxdursa körpü
/// sönülüdür, amma State yenə idarə olunur ki, komandalar "disabled" desin.
pub fn init(app: &AppHandle) -> tauri::Result<bool> {
    let host = Arc::new(DesktopHost {
        app: app.clone(),
        pending: Arc::new(Mutex::new(HashMap::new())),
        activity: Mutex::new(VecDeque::new()),
    });
    let version = app.package_info().version.to_string();
    let data_dir = app.path().app_data_dir()?;
    let bridge = Config::from_build(data_dir, &version).map(|cfg| {
        let b = Bridge::new(cfg, host.clone());
        tauri::async_runtime::spawn(b.clone().run());
        b
    });
    let enabled = bridge.is_some();
    app.manage(BridgeUi { bridge, host });
    Ok(enabled)
}

/// Pəncərə bağlananda (lib.rs → on_window_event, Destroyed).
pub fn on_window_destroyed(app: &AppHandle, label: &str) {
    if label.starts_with(APPROVE_PREFIX) {
        if let Some(ui) = app.try_state::<BridgeUi>() {
            ui.host.forget(label);
        }
    }
}

// =====================================================================
// kdost:// sxemi
// =====================================================================

/// `register_uri_scheme_protocol` üçün işləyici. Yalnız öz pəncərələrimizə
/// səhifə verir: əsas pəncərədəki sayt `kdost://`-u iframe-də aça bilməsin.
pub fn serve<R: Runtime>(ctx: tauri::UriSchemeContext<'_, R>, req: tauri::http::Request<Vec<u8>>) -> Response<Vec<u8>> {
    let label = ctx.webview_label();
    let body = match req.uri().path() {
        "/bridge.html" if label == BRIDGE_LABEL => Some(BRIDGE_HTML),
        "/approve.html" if label.starts_with(APPROVE_PREFIX) => Some(APPROVE_HTML),
        _ => None,
    };
    match body {
        Some(html) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CONTENT_SECURITY_POLICY, CSP)
            .header(header::CACHE_CONTROL, "no-store")
            .body(html.as_bytes().to_vec())
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Vec::new())
            .unwrap(),
    }
}

/// Windows-da xüsusi sxem `http://<sxem>.localhost/` kimi açılır (WebView2
/// məhdudiyyəti), Linux/macOS-da `kdost://localhost/`.
fn ui_url(page: &str) -> Url {
    #[cfg(any(windows, target_os = "android"))]
    let base = format!("http://{SCHEME}.localhost/");
    #[cfg(not(any(windows, target_os = "android")))]
    let base = format!("{SCHEME}://localhost/");
    Url::parse(&format!("{base}{page}?lang={}", lang_code())).expect("lokal URL düzgündür")
}

fn is_ui_url(url: &Url) -> bool {
    url.scheme() == SCHEME || url.host_str() == Some(&format!("{SCHEME}.localhost"))
}

// =====================================================================
// Pəncərələr
// =====================================================================

/// Tray → "Kompüter körpüsü". Açıqdırsa önə gətirir.
pub fn open_bridge_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(BRIDGE_LABEL) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let t = texts();
    let res = WebviewWindowBuilder::new(app, BRIDGE_LABEL, WebviewUrl::CustomProtocol(ui_url("bridge.html")))
        .title(t.bridge_title)
        .inner_size(520.0, 680.0)
        .min_inner_size(420.0, 520.0)
        .center()
        .on_navigation(is_ui_url)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build();
    if let Err(e) = res {
        eprintln!("[körpü] pəncərə açılmadı: {e}");
    }
}

fn open_approve_window(app: &AppHandle, label: &str, req: &ApprovalRequest) -> tauri::Result<()> {
    let t = texts();
    let title = format!("{} — {}", req.bot_name, t.approve_title);
    let w = WebviewWindowBuilder::new(app, label, WebviewUrl::CustomProtocol(ui_url("approve.html")))
        .title(title)
        .inner_size(520.0, if req.preview.is_some() || req.items.len() > 1 { 600.0 } else { 420.0 })
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        // Həmişə üstdə: tətbiq tray-də gizli olsa da istifadəçi görsün.
        .always_on_top(true)
        .focused(true)
        .center()
        .on_navigation(is_ui_url)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build()?;
    let _ = w.set_focus();
    Ok(())
}

// =====================================================================
// Komandalar (yalnız capabilities/bridge.json-dakı pəncərələrə açıqdır)
// =====================================================================

fn require_bridge(w: &WebviewWindow) -> Result<(), String> {
    if w.label() == BRIDGE_LABEL {
        Ok(())
    } else {
        Err("forbidden".into())
    }
}

fn get_bridge(ui: &BridgeUi) -> Result<&Bridge, String> {
    ui.bridge.as_ref().ok_or_else(|| "disabled".to_string())
}

fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|s| s.trim().chars().take(60).collect::<String>())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Windows PC".into())
}

#[tauri::command]
pub fn bridge_state(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>) -> Result<Value, String> {
    require_bridge(&window)?;
    let Some(b) = ui.bridge.as_ref() else {
        return Ok(json!({ "enabled": false }));
    };
    let folders: Vec<Value> = b
        .folders()
        .into_iter()
        .map(|f| json!({ "name": f.name, "path": f.path.display().to_string() }))
        .collect();
    let activity: Vec<String> = ui.host.activity.lock().unwrap().iter().cloned().collect();
    Ok(json!({
        "enabled": true,
        "status": b.status(),
        "device_name": b.device_name(),
        "default_name": default_device_name(),
        "folders": folders,
        "read_policy": b.read_policy(),
        "trust": b.trust(),
        "activity": activity,
        "version": window.app_handle().package_info().version.to_string(),
    }))
}

#[tauri::command]
pub async fn bridge_pair(
    window: WebviewWindow,
    ui: tauri::State<'_, BridgeUi>,
    code: String,
    name: String,
) -> Result<String, String> {
    require_bridge(&window)?;
    let b = get_bridge(&ui)?.clone();
    let name = {
        let n = name.trim();
        if n.is_empty() { default_device_name() } else { n.chars().take(60).collect() }
    };
    b.pair(code.trim(), &name).await.map_err(|e| match e {
        PairError::Code(c) => c,
        PairError::Network(_) => "network".into(),
    })
}

#[tauri::command]
pub async fn bridge_unlink(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>) -> Result<(), String> {
    require_bridge(&window)?;
    let b = get_bridge(&ui)?.clone();
    b.unlink().await;
    Ok(())
}

/// Qovluq seçimi doğma Windows pəncərəsi ilədir — sayt/JS yolu özü yaza
/// bilməz, yalnız istifadəçinin seçdiyi qovluq əlavə olunur.
#[tauri::command]
pub async fn bridge_add_folder(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>) -> Result<Value, String> {
    require_bridge(&window)?;
    let b = get_bridge(&ui)?.clone();
    let app = window.app_handle().clone();
    let title = texts().pick_folder.to_string();
    let w2 = window.clone();
    // blocking_pick_folder əsas axında çağırıla bilməz — ayrıca axın.
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().set_title(title).set_parent(&w2).blocking_pick_folder()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(fp) = picked else {
        return Ok(Value::Null); // istifadəçi ləğv etdi
    };
    let path = fp.into_path().map_err(|e| e.to_string())?;
    match b.add_folder(&path) {
        Ok(f) => Ok(json!({ "name": f.name, "path": f.path.display().to_string() })),
        Err(FolderError::AlreadyAdded) => Err("already_added".into()),
        Err(FolderError::TooBroad) => Err("too_broad".into()),
        Err(FolderError::NotADirectory) => Err("not_a_directory".into()),
        Err(FolderError::Io(e)) => Err(e),
    }
}

#[tauri::command]
pub fn bridge_remove_folder(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>, name: String) -> Result<bool, String> {
    require_bridge(&window)?;
    get_bridge(&ui)?.remove_folder(&name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn bridge_set_read_policy(
    window: WebviewWindow,
    ui: tauri::State<'_, BridgeUi>,
    policy: ReadPolicy,
) -> Result<(), String> {
    require_bridge(&window)?;
    get_bridge(&ui)?.set_read_policy(policy).map_err(|e| e.to_string())
}

/// "Soruşmadan işlə" — yalnız bu sessiya üçün (tətbiq bağlananda sönür).
#[tauri::command]
pub fn bridge_set_trust(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>, on: bool) -> Result<(), String> {
    require_bridge(&window)?;
    let b = get_bridge(&ui)?;
    b.set_trust(on);
    ui.host.activity(if on { "⚡ soruşmadan işlə: AÇIQ".into() } else { "soruşmadan işlə: bağlı".into() });
    Ok(())
}

/// Təsdiq pəncərəsi öz işini soruşur — yalnız öz adına bağlı olanı alır.
#[tauri::command]
pub fn approval_get(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>) -> Result<Value, String> {
    let map = ui.host.pending.lock().unwrap();
    let p = map.get(window.label()).ok_or("not_found")?;
    serde_json::to_value(&p.req).map_err(|e| e.to_string())
}

/// Qərar. Pəncərənin adı = işin açarı: başqa pəncərə (o cümlədən sayt)
/// bu işə qərar verə bilməz, hətta job_id-ni bilsə belə.
#[tauri::command]
pub fn approval_decide(window: WebviewWindow, ui: tauri::State<'_, BridgeUi>, allow: bool) -> Result<(), String> {
    let tx = {
        let mut map = ui.host.pending.lock().unwrap();
        let p = map.get_mut(window.label()).ok_or("not_found")?;
        p.tx.take()
    };
    if let Some(tx) = tx {
        let _ = tx.send(if allow { Decision::Allow } else { Decision::Deny });
    }
    // Pəncərəni WindowGuard bağlayır (future cavabı alan kimi).
    Ok(())
}
