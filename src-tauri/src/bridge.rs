//! Masaüstü körpüsü — 2-ci mərhələ (fayllar).
//!
//! Nə edir: bot (`index.ts` → `computer` aləti) `dost_device_jobs` cədvəlinə
//! iş yazır; bu modul işi götürür, lazım olsa istifadəçidən təsdiq istəyir,
//! seçilmiş qovluqların İÇİNDƏ icra edir və nəticəni geri yazır.
//!
//! Baza ilə müqavilə `dost_31_devices.sql`-dadır (funksiya parametrləri
//! `p_` ön şəkilçili — PostgREST adlı arqumentlə çağırır, ad səhv olsa 404).
//! İş/nəticə formaları `index.ts` → `runComputer` başlığındakı cədvəldir.
//!
//! Niyə pəncərə kodu burada yoxdur: bu fayl Tauri-dən asılı deyil, ona görə
//! Linux-da adi `cargo test` ilə sınanır. Təsdiq pəncərəsi və statusun
//! göstərilməsi `Host` trait-i ilə `lib.rs`-ə (növbəti addım) verilir.
//!
//! Təhlükəsizlik qatları (hər biri ayrıca işləyir):
//!   1. Token yalnız cihaz RPC-lərini açır; işi yalnız service role yazır.
//!   2. Yol yalnız seçilmiş qovluqların içində açılır: `..`, disk hərfi,
//!      `\`, `:` (NTFS axınları), Windows cihaz adları rədd; simvolik link /
//!      junction ilə qaçış `canonicalize` ilə tutulur.
//!   3. Dəyişən hər şey (yazma, köçürmə, silmə, arxiv, açma) təsdiqlə; oxuma
//!      ayara görə. "Soruşmadan işlə" (yalnız bu sessiya) təsdiqi söndürür,
//!      amma proqram işə salmaq (open .exe) HƏMİŞƏ soruşulur.
//!   4. Silmə həmişə Zibil qutusuna (geri qaytarıla bilir); üzərinə yazılan
//!      fayl da Zibil qutusuna gedir.
//!   5. Vaxtı keçmiş iş icra edilmir (təsdiq pəncərəsi açıq qalsa da) —
//!      bot artıq "alınmadı" deyib, gec yazılan fayl sürpriz olardı.
//!   6. İcra olunan fayl tipləri (.exe, .bat, .ps1 …) yazılmır.

use std::collections::HashSet;
use std::fs;
use std::future::Future;
use std::io::{self, Read, Write as _};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

// Əlavə əməliyyatlar (sənəd mətni, ZIP, disk analizi, bufer, sistem, Zibil qutusu).
#[path = "bridge_extra.rs"]
mod extra;

// =====================================================================
// 1. Konfiqurasiya və xarici interfeys
// =====================================================================

/// Supabase ünvanı və anon (publishable) açarı yığım vaxtı verilir
/// (GitHub Actions → Variables: `DOST_SUPABASE_URL`, `DOST_SUPABASE_ANON_KEY`).
/// Niyə koda yazılmır: layihə dəyişəndə kod dəyişməsin. Anon açarı gizli
/// deyil (saytda da açıqdır), amma `service_role` açarı HEÇ VAXT bura qoyulmamalıdır.
pub const BUILD_SUPABASE_URL: Option<&str> = option_env!("DOST_SUPABASE_URL");
pub const BUILD_SUPABASE_ANON_KEY: Option<&str> = option_env!("DOST_SUPABASE_ANON_KEY");

#[derive(Clone, Debug)]
pub struct Config {
    /// `https://<ref>.supabase.co` (sonunda `/` olmadan).
    pub base_url: String,
    pub anon_key: String,
    /// Token və qovluq siyahısının saxlandığı qovluq (Windows-da %APPDATA%\pro.kalbi.dost).
    pub data_dir: PathBuf,
    pub app_version: String,
    /// Ehtiyat sorğu: Realtime siqnalı çatmasa da iş ən gec bu qədər gecikir.
    pub poll_every: Duration,
    /// Baza 60 san-dan bir heartbeat gözləyir ("onlayn" = son ~150 san).
    pub heartbeat_every: Duration,
    /// Realtime-ı sönmək olar (sınaqlarda və problem olanda).
    pub realtime: bool,
}

impl Config {
    pub fn new(base_url: &str, anon_key: &str, data_dir: PathBuf, app_version: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            anon_key: anon_key.trim().to_string(),
            data_dir,
            app_version: app_version.to_string(),
            poll_every: Duration::from_secs(30),
            heartbeat_every: Duration::from_secs(60),
            realtime: true,
        }
    }

    /// Yığım vaxtı verilmiş dəyərlərdən. Yoxdursa `None` — körpü sönülü qalır,
    /// tətbiqin qalanı (sayt, tray, updater) adi işləyir.
    pub fn from_build(data_dir: PathBuf, app_version: &str) -> Option<Self> {
        let url = BUILD_SUPABASE_URL?.trim();
        let key = BUILD_SUPABASE_ANON_KEY?.trim();
        if url.is_empty() || key.is_empty() {
            return None;
        }
        Some(Self::new(url, key, data_dir, app_version))
    }
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Təsdiq pəncərəsinə gedən məlumat. Hamısı insan oxuyacağı formadadır.
#[derive(Clone, Debug, Serialize)]
pub struct ApprovalRequest {
    pub job_id: String,
    /// "read" | "write" | "move" | "copy" | "delete" | "mkdir"
    /// (list/search təsdiqsizdir — yalnız adlar görünür).
    pub op: String,
    pub bot_name: String,
    /// Müqavilə formasında yol: "Downloads/hesabat.txt".
    pub path: String,
    /// Diskdəki tam yol — istifadəçi nəyə icazə verdiyini dəqiq görsün.
    pub full_path: String,
    /// Yazmada: "create" | "overwrite" | "append".
    pub mode: Option<String>,
    /// Yazmada: mətnin əvvəli (≤ 600 simvol) və ümumi uzunluğu.
    pub preview: Option<String>,
    pub content_chars: Option<usize>,
    /// Çoxlu əməliyyatda (move/copy/delete): "a.png → Şəkillər/a.png" sətirləri
    /// (ən çox 200 — pəncərədə hamısı göstərilməsə də sayı `count`-dadır).
    pub items: Vec<String>,
    pub count: usize,
    /// move/copy: mövcud faylın üzərinə yazılacaq (köhnəsi Zibil qutusuna gedir).
    pub overwrite: bool,
    /// open: proqram işə salınacaq (.exe, .bat …) — pəncərə qırmızı xəbərdarlıq göstərir.
    /// Belə əməliyyat "soruşmadan işlə" rejimində də HƏMİŞƏ soruşulur.
    pub danger: bool,
    /// Pəncərə bu qədər sonra özü bağlanmalıdır (bot artıq gözləmir).
    pub expires_in_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Bot tərəfdən qoşulmayıb — qoşma kodu ekranı göstərilməlidir.
    Unpaired,
    /// Tokeni var, server ilə əlaqə qurulur.
    Connecting,
    /// Hazırdır. `live` = Realtime siqnalı da gəlir (yoxsa yalnız 30 san sorğu).
    Online { name: String, live: bool },
    /// İnternet / server yoxdur; özü yenidən cəhd edir.
    Offline,
    /// Saytda (Ayarlar → Cihazlarım) ayrılıb — yenidən qoşma lazımdır.
    Revoked,
}

/// Tətbiqin pəncərə tərəfi (`lib.rs`) bunu həyata keçirir.
pub trait Host: Send + Sync + 'static {
    /// Təsdiq pəncərəsini göstər. Future vaxt bitəndə atılır (drop) —
    /// pəncərəni `expires_in_ms` sonra özün də bağla.
    fn approve(&self, req: ApprovalRequest) -> BoxFuture<Decision>;
    /// Status dəyişəndə (yalnız dəyişəndə) çağırılır.
    fn status(&self, status: Status);
    /// İcra olunmuş iş haqqında qısa qeyd (fəaliyyət siyahısı üçün, istəyə görə).
    fn activity(&self, _line: String) {}
    /// Faylı öz proqramında və ya linki brauzerdə aç (lib.rs → opener plagini).
    fn open(&self, _target: &str, _is_url: bool) -> Result<(), String> {
        Err("unsupported".into())
    }
    /// Windows bildirişi (lib.rs → notification plagini).
    fn notify(&self, _title: &str, _body: &str) -> Result<(), String> {
        Err("unsupported".into())
    }
}

/// Oxuma (read) üçün təsdiq siyasəti. Yazma həmişə soruşulur.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReadPolicy {
    /// Hər faylın məzmununu oxumazdan əvvəl soruş (standart: ilk dəfə
    /// istifadə edənə ən təhlükəsizi).
    #[default]
    Ask,
    /// Seçilmiş qovluqlarda oxuma təsdiqsiz.
    Allow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Botun gördüyü ad (yolun ilk hissəsi). Unikal, böyük-kiçik hərfə həssas deyil.
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    /// Bazanın kodu: invalid_code, expired_code, device_limit, try_later …
    Code(String),
    Network(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FolderError {
    NotADirectory,
    AlreadyAdded,
    /// Diskin kökü (C:\) və ya istifadəçi qovluğunun özü — çox geniş.
    TooBroad,
    Io(String),
}

// =====================================================================
// 2. Saxlanılan vəziyyət
// =====================================================================

/// Diskdə `bridge.json`. Niyə adi fayl (Windows Credential Manager yox):
/// token yalnız bu cihazın işlərini götürməyə yarayır, fayl istifadəçinin
/// öz profilindədir (%APPDATA%) — başqa Windows istifadəçisi oxuya bilmir.
/// Daha güclü qoruma lazım olsa DPAPI sonra əlavə oluna bilər.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    folders: Vec<Folder>,
    #[serde(default)]
    read_policy: ReadPolicy,
}

const STATE_FILE: &str = "bridge.json";

fn load_saved(dir: &Path) -> Saved {
    match fs::read(dir.join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Saved::default(),
    }
}

/// Atomik yazma: əvvəl müvəqqəti fayl, sonra rename. Elektrik kəsilsə
/// yarımçıq JSON qalmasın (tokeni itirmək = yenidən qoşma).
fn store_saved(dir: &Path, s: &Saved) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(STATE_FILE))
}

// =====================================================================
// 3. Körpü
// =====================================================================

pub struct Bridge {
    inner: Arc<Inner>,
}

impl Clone for Bridge {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

struct Inner {
    cfg: Config,
    host: Arc<dyn Host>,
    http: reqwest::Client,
    saved: Mutex<Saved>,
    /// "İndi işləri yoxla" — Realtime siqnalı, qoşulma, ayırma.
    wake: Notify,
    /// Server saatı − yerli saat (ms). `expires_at` serverin saatı ilədir;
    /// kompüterin saatı səhv olsa da vaxt yoxlaması düz işləsin.
    skew_ms: AtomicI64,
    last_status: Mutex<Option<Status>>,
    /// Hazırda icra olunan işlər — eyni iş iki dəfə başlamasın.
    in_flight: Mutex<HashSet<String>>,
    /// "Soruşmadan işlə" — yalnız yaddaşda (diskə yazılmır): tətbiq yenidən
    /// açılanda həmişə təsdiqli rejimə qayıdır. Unudulmuş açar həftələrlə
    /// açıq qalmasın deyə qəsdən belədir.
    trust: std::sync::atomic::AtomicBool,
}

/// Bir RPC cavabı və ya şəbəkə xətası.
#[derive(Debug)]
enum NetError {
    Transport(String),
    Http(u16, String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Transport(e) => write!(f, "şəbəkə: {e}"),
            NetError::Http(c, b) => write!(f, "HTTP {c}: {}", truncate_chars(b, 200)),
        }
    }
}

impl Bridge {
    pub fn new(cfg: Config, host: Arc<dyn Host>) -> Self {
        let saved = load_saved(&cfg.data_dir);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(format!("KalBiDost/{}", cfg.app_version))
            .build()
            .expect("HTTP klienti yaranmalıdır");
        Self {
            inner: Arc::new(Inner {
                cfg,
                host,
                http,
                saved: Mutex::new(saved),
                wake: Notify::new(),
                skew_ms: AtomicI64::new(0),
                last_status: Mutex::new(None),
                in_flight: Mutex::new(HashSet::new()),
                trust: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    pub fn is_paired(&self) -> bool {
        self.inner.saved.lock().unwrap().token.is_some()
    }

    pub fn device_name(&self) -> Option<String> {
        self.inner.saved.lock().unwrap().name.clone()
    }

    pub fn status(&self) -> Status {
        self.inner
            .last_status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(if self.is_paired() { Status::Connecting } else { Status::Unpaired })
    }

    pub fn folders(&self) -> Vec<Folder> {
        self.inner.saved.lock().unwrap().folders.clone()
    }

    pub fn read_policy(&self) -> ReadPolicy {
        self.inner.saved.lock().unwrap().read_policy
    }

    /// "Soruşmadan işlə" (bu sessiya üçün). Açıq olanda heç bir əməliyyat
    /// təsdiq istəmir; qovluq sərhədi, icra olunan fayl qadağası və silmənin
    /// Zibil qutusuna getməsi yenə qüvvədədir.
    pub fn trust(&self) -> bool {
        self.inner.trust.load(Ordering::Relaxed)
    }

    pub fn set_trust(&self, on: bool) {
        self.inner.trust.store(on, Ordering::Relaxed);
    }

    pub fn set_read_policy(&self, p: ReadPolicy) -> io::Result<()> {
        self.inner.update_saved(|s| s.read_policy = p)
    }

    /// Qovluq əlavə et (qovluq seçmə pəncərəsindən gələn yol).
    /// Ad qovluğun öz adıdır; təkrarlanırsa "Ad (2)".
    pub fn add_folder(&self, path: &Path) -> Result<Folder, FolderError> {
        let canon = fs::canonicalize(path).map_err(|e| FolderError::Io(e.to_string()))?;
        if !canon.is_dir() {
            return Err(FolderError::NotADirectory);
        }
        if is_too_broad(&canon) {
            return Err(FolderError::TooBroad);
        }
        let base = canon
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| validate_segment(n).is_ok())
            .unwrap_or_else(|| "Qovluq".to_string());

        let mut result = None;
        let mut err = None;
        self.inner
            .update_saved(|s| {
                if s.folders.iter().any(|f| same_path(&f.path, &canon)) {
                    err = Some(FolderError::AlreadyAdded);
                    return;
                }
                let mut name = base.clone();
                let mut n = 2;
                while s.folders.iter().any(|f| fold_name(&f.name) == fold_name(&name)) {
                    name = format!("{base} ({n})");
                    n += 1;
                }
                let f = Folder { name, path: canon.clone() };
                s.folders.push(f.clone());
                result = Some(f);
            })
            .map_err(|e| FolderError::Io(e.to_string()))?;
        match (result, err) {
            (Some(f), _) => Ok(f),
            (None, Some(e)) => Err(e),
            (None, None) => Err(FolderError::Io("naməlum".into())),
        }
    }

    pub fn remove_folder(&self, name: &str) -> io::Result<bool> {
        let mut removed = false;
        self.inner.update_saved(|s| {
            let before = s.folders.len();
            s.folders.retain(|f| fold_name(&f.name) != fold_name(name));
            removed = s.folders.len() != before;
        })?;
        Ok(removed)
    }

    /// Qoşma kodu ilə qoşul (saytda Ayarlar → Cihazlarım → kod).
    /// Uğurlu olsa cihazın adını qaytarır.
    pub async fn pair(&self, code: &str, device_name: &str) -> Result<String, PairError> {
        let body = json!({
            "p_code": code,
            "p_name": device_name,
            "p_platform": platform(),
            "p_version": self.inner.cfg.app_version,
        });
        let v = self
            .inner
            .rpc("dost_device_pair_claim", body)
            .await
            .map_err(|e| PairError::Network(e.to_string()))?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = v.get("error").and_then(Value::as_str).unwrap_or("unknown").to_string();
            return Err(PairError::Code(code));
        }
        let token = v.get("token").and_then(Value::as_str).map(str::to_string);
        let topic = v.get("topic").and_then(Value::as_str).map(str::to_string);
        let (Some(token), Some(topic)) = (token, topic) else {
            return Err(PairError::Code("bad_response".into()));
        };
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(device_name)
            .to_string();
        let device_id = v.get("device_id").and_then(Value::as_str).map(str::to_string);
        self.inner
            .update_saved(|s| {
                s.token = Some(token);
                s.topic = Some(topic);
                s.name = Some(name.clone());
                s.device_id = device_id;
            })
            .map_err(|e| PairError::Code(format!("save_failed: {e}")))?;
        // Pəncərə dərhal "qoşulur" göstərsin (köhnə Revoked/Unpaired yox).
        self.inner.set_status(Status::Connecting);
        // Əsas dövr qoşulmanı gözləyir — oyat.
        self.inner.wake.notify_one();
        Ok(name)
    }

    /// "Bu kompüteri ayır" — serverdə token ölür, yerli token silinir.
    /// Qovluq siyahısı qalır (istifadəçinin öz seçimidir, yenidən qoşulanda lazım olacaq).
    pub async fn unlink(&self) {
        let token = self.inner.saved.lock().unwrap().token.clone();
        if let Some(t) = token {
            // Server əlçatmaz olsa da yerli token silinir; serverdəki cihaz
            // saytdan ayrıla bilər və heartbeat olmadan "oflayn" görünür.
            let _ = self.inner.rpc("dost_device_unlink", json!({ "p_token": t })).await;
        }
        let _ = self.inner.update_saved(|s| {
            s.token = None;
            s.topic = None;
            s.device_id = None;
            s.name = None;
        });
        self.inner.set_status(Status::Unpaired);
        self.inner.wake.notify_one();
    }

    /// Əsas dövr. Tətbiq açıq olduqca işləyir: `tauri::async_runtime::spawn(bridge.run())`.
    pub async fn run(self) {
        let inner = self.inner;
        loop {
            // --- Qoşulmayıbsa: kod gözlə ---
            let Some((token, topic)) = inner.credentials() else {
                if !matches!(inner.current_status(), Some(Status::Revoked)) {
                    inner.set_status(Status::Unpaired);
                }
                inner.wake.notified().await;
                continue;
            };

            inner.set_status(Status::Connecting);
            let live = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let rt = if inner.cfg.realtime {
                Some(tokio::spawn(realtime_loop(inner.clone(), topic.clone(), live.clone())))
            } else {
                None
            };

            let outcome = session_loop(&inner, &token, &live).await;

            if let Some(h) = rt {
                h.abort();
            }
            match outcome {
                SessionEnd::Revoked => {
                    let _ = inner.update_saved(|s| {
                        s.token = None;
                        s.topic = None;
                        s.device_id = None;
                        s.name = None;
                    });
                    inner.set_status(Status::Revoked);
                }
                SessionEnd::TokenChanged => {}
            }
        }
    }
}

enum SessionEnd {
    Revoked,
    /// Token silinib (unlink) və ya dəyişib (yeni qoşma) — yenidən başla.
    TokenChanged,
}

/// Bir token ilə iş dövrü: heartbeat + sorğu (siqnal və ya taymer ilə).
async fn session_loop(
    inner: &Arc<Inner>,
    token: &str,
    live: &Arc<std::sync::atomic::AtomicBool>,
) -> SessionEnd {
    let mut next_beat = tokio::time::Instant::now();
    let mut next_poll = tokio::time::Instant::now();
    let mut fail_streak: u32 = 0;
    loop {
        if inner.credentials().map(|c| c.0).as_deref() != Some(token) {
            return SessionEnd::TokenChanged;
        }

        let now = tokio::time::Instant::now();
        if now >= next_beat {
            match inner.rpc("dost_device_heartbeat", json!({
                "p_token": token,
                "p_version": inner.cfg.app_version,
            }))
            .await
            {
                Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {
                    fail_streak = 0;
                    let name = v
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| inner.saved.lock().unwrap().name.clone())
                        .unwrap_or_default();
                    // Saytda ad dəyişibsə yerli adı da yenilə.
                    if inner.saved.lock().unwrap().name.as_deref() != Some(name.as_str()) {
                        let n = name.clone();
                        let _ = inner.update_saved(|s| s.name = Some(n));
                    }
                    inner.set_status(Status::Online {
                        name,
                        live: live.load(Ordering::Relaxed),
                    });
                    next_beat = now + inner.cfg.heartbeat_every;
                }
                Ok(v) if v.get("error").and_then(Value::as_str) == Some("revoked") => {
                    return SessionEnd::Revoked;
                }
                Ok(_) | Err(_) => {
                    // Şəbəkə yoxdur və ya gözlənilməz cavab: artan fasilə ilə
                    // təkrar (2, 4, 8 … 60 san) — serveri boş yerə yükləməyək.
                    fail_streak = fail_streak.saturating_add(1);
                    inner.set_status(Status::Offline);
                    let back = Duration::from_secs((1u64 << fail_streak.min(5)).min(60));
                    next_beat = now + back;
                    next_poll = next_poll.max(now + back);
                }
            }
        }

        let now = tokio::time::Instant::now();
        if now >= next_poll && fail_streak == 0 {
            match poll_jobs(inner, token).await {
                PollResult::Revoked => return SessionEnd::Revoked,
                PollResult::Ok | PollResult::Failed => {}
            }
            next_poll = tokio::time::Instant::now() + inner.cfg.poll_every;
        }

        // Online statusunda "live" dəyişibsə göstər (Realtime qoşuldu/qopdu).
        if let Some(Status::Online { name, live: was }) = inner.current_status() {
            let now_live = live.load(Ordering::Relaxed);
            if was != now_live {
                inner.set_status(Status::Online { name, live: now_live });
            }
        }

        let wake_at = next_beat.min(next_poll);
        tokio::select! {
            _ = inner.wake.notified() => {
                // Siqnal: dərhal sorğu (amma şəbəkə xətası fasiləsini pozmadan).
                if fail_streak == 0 {
                    next_poll = tokio::time::Instant::now();
                }
            }
            _ = tokio::time::sleep_until(wake_at) => {}
        }
    }
}

enum PollResult {
    Ok,
    Failed,
    Revoked,
}

async fn poll_jobs(inner: &Arc<Inner>, token: &str) -> PollResult {
    let v = match inner.rpc("dost_device_jobs_next", json!({ "p_token": token })).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[körpü] jobs_next: {e}");
            return PollResult::Failed;
        }
    };
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        if v.get("error").and_then(Value::as_str) == Some("revoked") {
            return PollResult::Revoked;
        }
        return PollResult::Failed;
    }
    let jobs = v.get("jobs").and_then(Value::as_array).cloned().unwrap_or_default();
    for raw in jobs {
        let Ok(job) = serde_json::from_value::<Job>(raw) else {
            continue;
        };
        if !inner.in_flight.lock().unwrap().insert(job.id.clone()) {
            continue;
        }
        // Hər iş ayrıca task: biri təsdiq gözləyəndə digər (məs. siyahı) dayanmasın.
        let inner2 = inner.clone();
        let token = token.to_string();
        tokio::spawn(async move {
            let id = job.id.clone();
            handle_job(&inner2, &token, job).await;
            inner2.in_flight.lock().unwrap().remove(&id);
        });
    }
    PollResult::Ok
}

#[derive(Debug, Deserialize)]
struct Job {
    id: String,
    op: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    bot_name: Option<String>,
    expires_at: String,
}

/// Nəticə yazılmazdan əvvəl lazım olan ehtiyat vaxt: yazma faylı dəyişdirdi,
/// amma `job_finish` vaxtı keçdiyinə görə rədd olundu → bot "alınmadı" deyər,
/// fayl isə dəyişib. Ona görə vaxtın sonuna bu qədər qalanda icra başlamır.
const FINISH_MARGIN: Duration = Duration::from_secs(2);

async fn handle_job(inner: &Arc<Inner>, token: &str, job: Job) {
    let Some(deadline) = inner.local_deadline(&job.expires_at) else {
        // Tarix oxunmursa təhlükəsiz tərəf: icra etmə, xəta yaz.
        inner.finish(token, &job.id, "error", None, Some("bad_expires_at")).await;
        return;
    };
    if Instant::now() + FINISH_MARGIN >= deadline {
        return; // vaxtı keçib — baza özü `expired` edəcək
    }

    let folders = inner.saved.lock().unwrap().folders.clone();
    let policy = inner.saved.lock().unwrap().read_policy;
    let op = match Op::parse(&job.op, &job.args) {
        Ok(op) => op,
        Err(e) => {
            inner.finish(token, &job.id, "error", None, Some(e.code())).await;
            return;
        }
    };

    // Bütün yollar əvvəlcədən yoxlanır: qovluqdan kənar yol üçün istifadəçini
    // təsdiq pəncərəsi ilə narahat etməyə dəyməz. Çoxlu əməliyyatda bir yol
    // səhvdirsə heç biri icra olunmur — yarımçıq "səliqə" qalmasın.
    let pre = match precheck(&folders, &op) {
        Ok(p) => p,
        Err(e) => {
            inner.finish(token, &job.id, "error", None, Some(&e)).await;
            return;
        }
    };

    let trust = inner.trust.load(Ordering::Relaxed);
    // Proqram işə salmaq ən riskli addımdır — rejimdən asılı olmayaraq soruşulur.
    let always = op.is_dangerous();
    let needs_approval = always
        || (!trust
            && match &op {
                // Oxuma xarakterli: istifadəçinin ayarına görə.
                Op::Read { .. } | Op::ClipboardGet => policy == ReadPolicy::Ask,
                // Yalnız adlar/ölçülər/ümumi məlumat — məzmun yox.
                Op::List { .. } | Op::Search { .. } | Op::Stats { .. } | Op::Duplicates { .. }
                | Op::SystemInfo | Op::TrashList | Op::Notify { .. } => false,
                // Nəyisə dəyişən hər şey.
                Op::Write { .. } | Op::Move { .. } | Op::Copy { .. } | Op::Delete { .. } | Op::Mkdir { .. }
                | Op::Open { .. } | Op::Zip { .. } | Op::Unzip { .. } | Op::ClipboardSet { .. }
                | Op::TrashRestore { .. } => true,
            });

    if needs_approval {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let req = approval_request(&job, &op, pre, remaining);
        let fut = inner.host.approve(req);
        match tokio::time::timeout(remaining, fut).await {
            Ok(Decision::Allow) => {}
            Ok(Decision::Deny) => {
                inner.host.activity(format!("✕ {}", op.label()));
                inner.finish(token, &job.id, "denied", None, None).await;
                return;
            }
            Err(_) => return, // pəncərə vaxtında cavablanmadı — icra yoxdur
        }
        // Təsdiq vaxtın sonuna yaxın gəlibsə — icra etmə (bax FINISH_MARGIN).
        if Instant::now() + FINISH_MARGIN >= deadline {
            return;
        }
    }

    // Disk əməliyyatı bloklayıcıdır — tokio axınını tutmasın.
    let op2 = op.clone();
    let host = inner.host.clone();
    let res = tokio::task::spawn_blocking(move || execute(&folders, &op2, host.as_ref()))
        .await
        .unwrap_or_else(|_| Err(OpError::Other("panic".into())));

    match res {
        Ok(result) => {
            inner.host.activity(format!("{}{}", if trust { "✓⚡ " } else { "✓ " }, op.label()));
            inner.finish(token, &job.id, "done", Some(result), None).await;
        }
        Err(e) => {
            inner.host.activity(format!("! {} · {}", op.label(), e.code()));
            inner.finish(token, &job.id, "error", None, Some(e.code())).await;
        }
    }
}

/// Təsdiqdən əvvəlki yoxlamanın nəticəsi: pəncərədə nə göstəriləcək.
#[derive(Debug)]
struct Precheck {
    full_path: String,
    items: Vec<String>,
    count: usize,
}

/// Bütün yolları icradan əvvəl yoxlayır. Xəta mətni: "<kod> · <yol>" —
/// bot hansı yolun səhv olduğunu görsün və düzəltsin.
fn precheck(folders: &[Folder], op: &Op) -> Result<Precheck, String> {
    let err = |e: OpError, p: &str| format!("{} · {}", e.code(), p);
    // Çoxlu əməliyyatda element: qovluğun özü ola bilməz (rel boş deyil).
    let inner_path = |p: &str| -> Result<PathBuf, String> {
        match resolve(folders, p).map_err(|e| err(e, p))? {
            Resolved::Path { rel, abs, .. } if !rel.is_empty() => Ok(abs),
            _ => Err(err(OpError::Unsupported, p)),
        }
    };
    match op {
        Op::List { path } | Op::Read { path, .. } | Op::Search { path, .. } | Op::Write { path, .. } => {
            let full = match resolve(folders, path).map_err(|e| err(e, path))? {
                Resolved::Roots => String::new(),
                Resolved::Path { abs, .. } => abs.display().to_string(),
            };
            Ok(Precheck { full_path: full, items: vec![], count: 1 })
        }
        Op::Mkdir { path } => {
            let abs = inner_path(path)?;
            Ok(Precheck { full_path: abs.display().to_string(), items: vec![], count: 1 })
        }
        Op::Delete { paths } => {
            for p in paths {
                inner_path(p)?;
            }
            Ok(Precheck {
                full_path: String::new(),
                items: paths.iter().take(APPROVAL_ITEMS).cloned().collect(),
                count: paths.len(),
            })
        }
        Op::Open { target, url: true } => {
            if !(target.starts_with("https://") || target.starts_with("http://")) || target.len() > 2000 {
                return Err(err(OpError::Unsupported, target));
            }
            Ok(Precheck { full_path: target.clone(), items: vec![], count: 1 })
        }
        Op::Open { target, url: false } => {
            let abs = inner_path(target)?;
            Ok(Precheck { full_path: abs.display().to_string(), items: vec![], count: 1 })
        }
        Op::Zip { paths, to } => {
            for p in paths {
                inner_path(p)?;
            }
            let t = inner_path(to)?;
            Ok(Precheck {
                full_path: t.display().to_string(),
                items: paths.iter().take(APPROVAL_ITEMS).cloned().collect(),
                count: paths.len(),
            })
        }
        Op::Unzip { path, to } => {
            inner_path(path)?;
            let t = inner_path(to)?;
            Ok(Precheck { full_path: t.display().to_string(), items: vec![format!("{path}  →  {to}")], count: 1 })
        }
        Op::Stats { path, .. } | Op::Duplicates { path } => {
            let full = match resolve(folders, path).map_err(|e| err(e, path))? {
                Resolved::Roots => String::new(),
                Resolved::Path { abs, .. } => abs.display().to_string(),
            };
            Ok(Precheck { full_path: full, items: vec![], count: 1 })
        }
        Op::TrashRestore { paths } => {
            // Fayl hələ Zibil qutusundadır — burada yalnız sözlə yoxlama
            // (seçilmiş qovluğun adı ilə başlayır, "..", disk hərfi yoxdur).
            for p in paths {
                inner_path(p).or_else(|e| if e.starts_with("not_found") { Ok(PathBuf::new()) } else { Err(e) })?;
            }
            Ok(Precheck { full_path: String::new(), items: paths.iter().take(APPROVAL_ITEMS).cloned().collect(), count: paths.len() })
        }
        Op::ClipboardGet | Op::ClipboardSet { .. } | Op::Notify { .. } | Op::SystemInfo | Op::TrashList => {
            Ok(Precheck { full_path: String::new(), items: vec![], count: 1 })
        }
        Op::Move { items, .. } | Op::Copy { items, .. } => {
            for (from, to) in items {
                let f = inner_path(from)?;
                let t = inner_path(to)?;
                // Qovluğu öz içinə köçürmək/kopyalamaq sonsuz dövrədir.
                if t.starts_with(&f) {
                    return Err(err(OpError::Unsupported, to));
                }
            }
            Ok(Precheck {
                full_path: String::new(),
                items: items.iter().take(APPROVAL_ITEMS).map(|(f, t)| format!("{f}  →  {t}")).collect(),
                count: items.len(),
            })
        }
    }
}

const APPROVAL_ITEMS: usize = 200;

fn approval_request(job: &Job, op: &Op, pre: Precheck, remaining: Duration) -> ApprovalRequest {
    let (mode, preview, chars) = match op {
        Op::Write { content, mode, .. } => (
            Some(mode.as_str().to_string()),
            Some(truncate_chars(content, 600)),
            Some(content.chars().count()),
        ),
        Op::ClipboardSet { text } => (None, Some(truncate_chars(text, 600)), Some(text.chars().count())),
        _ => (None, None, None),
    };
    let overwrite = matches!(op, Op::Move { overwrite: true, .. } | Op::Copy { overwrite: true, .. });
    let danger = op.is_dangerous();
    ApprovalRequest {
        job_id: job.id.clone(),
        op: op.name().to_string(),
        bot_name: job.bot_name.clone().unwrap_or_else(|| "KalBi Dost".into()),
        path: op.path().to_string(),
        full_path: pre.full_path,
        mode,
        preview,
        content_chars: chars,
        items: pre.items,
        count: pre.count,
        overwrite,
        danger,
        expires_in_ms: remaining.saturating_sub(FINISH_MARGIN).as_millis() as u64,
    }
}

impl Inner {
    fn credentials(&self) -> Option<(String, String)> {
        let s = self.saved.lock().unwrap();
        Some((s.token.clone()?, s.topic.clone()?))
    }

    fn update_saved(&self, f: impl FnOnce(&mut Saved)) -> io::Result<()> {
        let mut s = self.saved.lock().unwrap();
        f(&mut s);
        store_saved(&self.cfg.data_dir, &s)
    }

    fn current_status(&self) -> Option<Status> {
        self.last_status.lock().unwrap().clone()
    }

    fn set_status(&self, st: Status) {
        let mut last = self.last_status.lock().unwrap();
        if last.as_ref() == Some(&st) {
            return;
        }
        *last = Some(st.clone());
        drop(last);
        self.host.status(st);
    }

    /// PostgREST RPC. Funksiyalar xətanı `{ok:false, error}` kimi qaytarır
    /// (exception yox), ona görə 2xx olmayan cavab — əsl problem (şəbəkə,
    /// funksiya yoxdur, açar səhvdir).
    async fn rpc(&self, name: &str, body: Value) -> Result<Value, NetError> {
        let url = format!("{}/rest/v1/rpc/{name}", self.cfg.base_url);
        let mut req = self
            .http
            .post(url)
            .header("apikey", &self.cfg.anon_key)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        // Köhnə anon açarı JWT-dir və Authorization-da da gedir. Yeni
        // "sb_publishable_…" açarı JWT deyil — onu Authorization-a qoymaq
        // olmaz (şlüz JWT kimi yoxlamağa çalışır), yalnız apikey kifayətdir.
        if self.cfg.anon_key.starts_with("eyJ") {
            req = req.header("Authorization", format!("Bearer {}", self.cfg.anon_key));
        }
        let resp = req.send().await.map_err(|e| NetError::Transport(e.to_string()))?;
        if let Some(d) = resp.headers().get("date").and_then(|h| h.to_str().ok()) {
            if let Some(skew) = skew_from_date(d, Utc::now()) {
                self.skew_ms.store(skew, Ordering::Relaxed);
            }
        }
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| NetError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(NetError::Http(status, text));
        }
        serde_json::from_str(&text).map_err(|e| NetError::Transport(format!("json: {e}")))
    }

    async fn finish(
        &self,
        token: &str,
        job_id: &str,
        status: &str,
        result: Option<Value>,
        error: Option<&str>,
    ) {
        let body = json!({
            "p_token": token,
            "p_job_id": job_id,
            "p_status": status,
            "p_result": result,
            "p_error": error,
        });
        // Bir dəfə təkrar: şəbəkə qısa qırılıbsa nəticə itməsin. `not_running`
        // (vaxtı keçib / artıq bağlanıb) təkrarlanmır — bot onsuz da gözləmir.
        for attempt in 0..2 {
            match self.rpc("dost_device_job_finish", body.clone()).await {
                Ok(v) => {
                    if v.get("ok").and_then(Value::as_bool) != Some(true) {
                        eprintln!("[körpü] job_finish {job_id}: {v}");
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("[körpü] job_finish {job_id} (cəhd {}): {e}", attempt + 1);
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
            }
        }
    }

    /// `expires_at` (serverin saatı) → yerli `Instant`.
    fn local_deadline(&self, expires_at: &str) -> Option<Instant> {
        let exp = DateTime::parse_from_rfc3339(expires_at).ok()?.with_timezone(&Utc);
        let server_now = Utc::now() + chrono::Duration::milliseconds(self.skew_ms.load(Ordering::Relaxed));
        // Date başlığı saniyə dəqiqliyindədir — 1 san ehtiyat çıxırıq.
        let left_ms = (exp - server_now).num_milliseconds() - 1000;
        if left_ms <= 0 {
            return Some(Instant::now());
        }
        Some(Instant::now() + Duration::from_millis(left_ms as u64))
    }
}

/// HTTP `Date` başlığından saat fərqi (ms). 2 san-dan kiçik fərq sıfır sayılır
/// (başlığın öz dəqiqliyi saniyədir, səs-küyü vaxt hesabına qatmayaq).
fn skew_from_date(date: &str, local_now: DateTime<Utc>) -> Option<i64> {
    let server = DateTime::parse_from_rfc2822(date).ok()?.with_timezone(&Utc);
    let skew = (server - local_now).num_milliseconds();
    Some(if skew.abs() < 2000 { 0 } else { skew })
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else {
        std::env::consts::OS
    }
}

// =====================================================================
// 4. Realtime (Phoenix websocket protokolu)
// =====================================================================
//
// Baza yeni iş yazanda `realtime.send({job_id}, 'job', topic, false)` göndərir.
// Bu ictimai broadcast kanalıdır; klient "realtime:<topic>" mövzusuna qoşulur
// (supabase-js `channel(topic)` da belə edir). Siqnalın içinə baxmırıq —
// hər hansı broadcast "işləri yoxla" deməkdir; işin özü token ilə gəlir.
// Saxta siqnal göndərən yalnız boş sorğuya səbəb olar.
//
// Supabase → Realtime → Settings-də "private only" açıqdırsa qoşulma rədd
// olunur: o zaman yalnız 30 san sorğu işləyir (status: live=false).

fn realtime_url(base_url: &str, key: &str) -> String {
    let ws = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base_url.to_string()
    };
    format!("{ws}/realtime/v1/websocket?apikey={key}&vsn=1.0.0")
}

fn join_frame(topic: &str, r: u64) -> String {
    json!({
        "topic": format!("realtime:{topic}"),
        "event": "phx_join",
        "payload": {
            "config": {
                "broadcast": { "ack": false, "self": false },
                "presence": { "key": "" },
                "private": false
            }
        },
        "ref": r.to_string(),
        "join_ref": r.to_string()
    })
    .to_string()
}

fn heartbeat_frame(r: u64) -> String {
    json!({ "topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": r.to_string() }).to_string()
}

#[derive(Debug, PartialEq, Eq)]
enum RtEvent {
    Signal,
    Joined,
    JoinFailed(String),
    Other,
}

fn parse_rt(text: &str, topic: &str, join_ref: &str) -> RtEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return RtEvent::Other;
    };
    let full = format!("realtime:{topic}");
    if v.get("topic").and_then(Value::as_str) != Some(full.as_str()) {
        return RtEvent::Other;
    }
    match v.get("event").and_then(Value::as_str) {
        Some("broadcast") => RtEvent::Signal,
        Some("phx_reply") if v.get("ref").and_then(Value::as_str) == Some(join_ref) => {
            let st = v.pointer("/payload/status").and_then(Value::as_str).unwrap_or("");
            if st == "ok" {
                RtEvent::Joined
            } else {
                RtEvent::JoinFailed(v.pointer("/payload/response").map(|r| r.to_string()).unwrap_or_default())
            }
        }
        // Server kanalı bağladı (məs. ayar dəyişdi) — yenidən qoşulmaq lazımdır.
        Some("phx_error") | Some("phx_close") => RtEvent::JoinFailed("closed".into()),
        _ => RtEvent::Other,
    }
}

fn tls_connector() -> Option<tokio_tungstenite::Connector> {
    // reqwest ilə eyni yol: Windows-un öz sertifikat anbarı (platform verifier).
    // Antivirus / korporativ proxy öz sertifikatını sistemə qoyubsa da işləyir.
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .with_platform_verifier()
        .ok()?
        .with_no_client_auth();
    Some(tokio_tungstenite::Connector::Rustls(Arc::new(cfg)))
}

async fn realtime_loop(inner: Arc<Inner>, topic: String, live: Arc<std::sync::atomic::AtomicBool>) {
    let url = realtime_url(&inner.cfg.base_url, &inner.cfg.anon_key);
    let mut backoff = 1u64;
    loop {
        let connector = if url.starts_with("wss://") { tls_connector() } else { None };
        let conn = tokio_tungstenite::connect_async_tls_with_config(url.as_str(), None, false, connector).await;
        let Ok((ws, _)) = conn else {
            live.store(false, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
            continue;
        };
        let (mut tx, mut rx) = ws.split();
        let mut r: u64 = 1;
        let join_ref = r.to_string();
        if tx.send(Message::Text(join_frame(&topic, r).into())).await.is_err() {
            continue;
        }
        let mut beat = tokio::time::interval(Duration::from_secs(25));
        beat.tick().await; // ilk tick dərhal gəlir — onu udmaq
        let mut failed_join = false;
        loop {
            tokio::select! {
                msg = rx.next() => {
                    match msg {
                        Some(Ok(Message::Text(t))) => match parse_rt(&t, &topic, &join_ref) {
                            RtEvent::Signal => inner.wake.notify_one(),
                            RtEvent::Joined => {
                                live.store(true, Ordering::Relaxed);
                                backoff = 1;
                                // Qopuq ikən gələn siqnallar itib — bir dəfə yoxla.
                                inner.wake.notify_one();
                            }
                            RtEvent::JoinFailed(why) => {
                                eprintln!("[körpü] realtime kanalı: {why}");
                                failed_join = true;
                                break;
                            }
                            RtEvent::Other => {}
                        },
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {}
                    }
                }
                _ = beat.tick() => {
                    r += 1;
                    if tx.send(Message::Text(heartbeat_frame(r).into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        live.store(false, Ordering::Relaxed);
        inner.wake.notify_one(); // status yenilənsin (live=false)
        // Kanal rədd olunubsa ("private only") tez-tez təkrarlamağın mənası
        // yoxdur — 5 dəqiqədən bir yoxla; ayar düzələndə özü qoşulacaq.
        let wait = if failed_join { 300 } else { backoff };
        tokio::time::sleep(Duration::from_secs(wait)).await;
        backoff = (backoff * 2).min(60);
    }
}

// =====================================================================
// 5. Fayl əməliyyatları (müqavilə: list / read / search / write)
// =====================================================================

const READ_DEFAULT_CHARS: usize = 9000;
const READ_MAX_CHARS: usize = 100_000;
const SEARCH_DEFAULT_LIMIT: usize = 100;
const SEARCH_MAX_LIMIT: usize = 500;
/// Axtarışda baxılan element limiti — bütün diski gəzməsin.
const SEARCH_MAX_VISITS: usize = 100_000;
const SEARCH_MAX_DEPTH: usize = 16;
const LIST_MAX_ENTRIES: usize = 1000;
const WRITE_MAX_CHARS: usize = 500_000;
const PATH_MAX_CHARS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum WriteMode {
    Create,
    Overwrite,
    Append,
}

impl WriteMode {
    fn as_str(&self) -> &'static str {
        match self {
            WriteMode::Create => "create",
            WriteMode::Overwrite => "overwrite",
            WriteMode::Append => "append",
        }
    }
}

#[derive(Clone, Debug)]
enum Op {
    List { path: String },
    Read { path: String, max_chars: usize },
    Search { path: String, query: String, limit: usize },
    Write { path: String, content: String, mode: WriteMode },
    /// Bir işdə çoxlu köçürmə (səliqəyə salma bir təsdiqlə). Hədəfin ara
    /// qovluqları avtomatik yaranır.
    Move { items: Vec<(String, String)>, overwrite: bool },
    Copy { items: Vec<(String, String)>, overwrite: bool },
    /// Həmişə Zibil qutusuna (geri qaytarıla bilir), həmişəlik silmə yoxdur.
    Delete { paths: Vec<String> },
    Mkdir { path: String },
    /// Faylı öz proqramında açmaq (url=false) və ya linki brauzerdə (url=true).
    Open { target: String, url: bool },
    Zip { paths: Vec<String>, to: String },
    Unzip { path: String, to: String },
    /// Ölçü, ən böyük fayllar, uzantılar.
    Stats { path: String, top: usize },
    Duplicates { path: String },
    ClipboardGet,
    ClipboardSet { text: String },
    Notify { title: String, body: String },
    SystemInfo,
    TrashList,
    TrashRestore { paths: Vec<String> },
}

/// "paths": [...] və ya tək "path" — model hər ikisini yaza bilər.
fn parse_paths(args: &Value, single: &str) -> Result<Vec<String>, OpError> {
    let mut paths: Vec<String> = match args.get("paths").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec![],
    };
    if paths.is_empty() && !single.is_empty() {
        paths.push(single.to_string());
    }
    if paths.is_empty() {
        return Err(OpError::Unsupported);
    }
    if paths.len() > MULTI_MAX {
        return Err(OpError::TooLarge);
    }
    if paths.iter().any(|p| p.chars().count() > PATH_MAX_CHARS) {
        return Err(OpError::BadPath);
    }
    Ok(paths)
}

/// Bir işdə ən çox bu qədər element: model səhvən minlərlə əməliyyat
/// göndərsə də təsdiq pəncərəsi və disk yükü idarə oluna bilsin.
const MULTI_MAX: usize = 500;

fn parse_pairs(args: &Value) -> Result<Vec<(String, String)>, OpError> {
    let arr = args.get("items").and_then(Value::as_array).ok_or(OpError::Unsupported)?;
    if arr.is_empty() {
        return Err(OpError::Unsupported);
    }
    if arr.len() > MULTI_MAX {
        return Err(OpError::TooLarge);
    }
    arr.iter()
        .map(|it| {
            let f = it.get("from").and_then(Value::as_str).unwrap_or("");
            let t = it.get("to").and_then(Value::as_str).unwrap_or("");
            if f.is_empty() || t.is_empty() || f.chars().count() > PATH_MAX_CHARS || t.chars().count() > PATH_MAX_CHARS {
                return Err(OpError::BadPath);
            }
            Ok((f.to_string(), t.to_string()))
        })
        .collect()
}

impl Op {
    fn parse(op: &str, args: &Value) -> Result<Op, OpError> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| args.get(k).and_then(Value::as_u64).map(|x| x as usize);
        let path = s("path");
        if path.chars().count() > PATH_MAX_CHARS {
            return Err(OpError::BadPath);
        }
        Ok(match op {
            "list" => Op::List { path },
            "read" => Op::Read {
                path,
                max_chars: n("max_chars").unwrap_or(READ_DEFAULT_CHARS).clamp(1, READ_MAX_CHARS),
            },
            "search" => Op::Search {
                path,
                query: s("query"),
                limit: n("limit").unwrap_or(SEARCH_DEFAULT_LIMIT).clamp(1, SEARCH_MAX_LIMIT),
            },
            "write" => {
                let mode = match args.get("mode").and_then(Value::as_str).unwrap_or("create") {
                    "create" => WriteMode::Create,
                    "overwrite" => WriteMode::Overwrite,
                    "append" => WriteMode::Append,
                    _ => return Err(OpError::Unsupported),
                };
                // content yoxdursa boş fayl deyil, xəta: model səhvən boş
                // overwrite göndərsə faylın məzmunu silinməsin.
                let Some(content) = args.get("content").and_then(Value::as_str) else {
                    return Err(OpError::Unsupported);
                };
                Op::Write { path, content: content.to_string(), mode }
            }
            "move" | "copy" => {
                let items = parse_pairs(args)?;
                let overwrite = args.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
                if op == "move" { Op::Move { items, overwrite } } else { Op::Copy { items, overwrite } }
            }
            "delete" => Op::Delete { paths: parse_paths(args, &path)? },
            "mkdir" => Op::Mkdir { path },
            "open" => {
                let url = s("url");
                if !url.is_empty() { Op::Open { target: url, url: true } } else { Op::Open { target: path, url: false } }
            }
            "zip" => Op::Zip { paths: parse_paths(args, "")?, to: s("to") },
            "unzip" => Op::Unzip { path, to: s("to") },
            "stats" => Op::Stats { path, top: n("top").unwrap_or(20).clamp(1, 100) },
            "duplicates" => Op::Duplicates { path },
            "clipboard_get" => Op::ClipboardGet,
            "clipboard_set" => Op::ClipboardSet { text: s("text") },
            "notify" => Op::Notify {
                title: truncate_chars(&s("title"), 100),
                body: truncate_chars(&s("body"), 400),
            },
            "system_info" => Op::SystemInfo,
            "trash_list" => Op::TrashList,
            "trash_restore" => Op::TrashRestore { paths: parse_paths(args, &path)? },
            _ => return Err(OpError::Unsupported),
        })
    }

    fn name(&self) -> &'static str {
        match self {
            Op::List { .. } => "list",
            Op::Read { .. } => "read",
            Op::Search { .. } => "search",
            Op::Write { .. } => "write",
            Op::Move { .. } => "move",
            Op::Copy { .. } => "copy",
            Op::Delete { .. } => "delete",
            Op::Mkdir { .. } => "mkdir",
            Op::Open { .. } => "open",
            Op::Zip { .. } => "zip",
            Op::Unzip { .. } => "unzip",
            Op::Stats { .. } => "stats",
            Op::Duplicates { .. } => "duplicates",
            Op::ClipboardGet => "clipboard_get",
            Op::ClipboardSet { .. } => "clipboard_set",
            Op::Notify { .. } => "notify",
            Op::SystemInfo => "system_info",
            Op::TrashList => "trash_list",
            Op::TrashRestore { .. } => "trash_restore",
        }
    }

    /// Proqram işə salan əməliyyat: icra olunan faylı açmaq. Həmişə soruşulur.
    fn is_dangerous(&self) -> bool {
        matches!(self, Op::Open { target, url: false } if is_executable_name(target))
    }

    /// Əsas yol (tək əməliyyatda) və ya birinci element (çoxluda).
    fn path(&self) -> &str {
        match self {
            Op::List { path } | Op::Read { path, .. } | Op::Search { path, .. } | Op::Write { path, .. } | Op::Mkdir { path }
            | Op::Unzip { path, .. } | Op::Stats { path, .. } | Op::Duplicates { path } => path,
            Op::Open { target, .. } => target,
            Op::Move { items, .. } | Op::Copy { items, .. } => items.first().map(|x| x.0.as_str()).unwrap_or(""),
            Op::Delete { paths } | Op::Zip { paths, .. } | Op::TrashRestore { paths } => {
                paths.first().map(String::as_str).unwrap_or("")
            }
            Op::ClipboardGet | Op::ClipboardSet { .. } | Op::Notify { .. } | Op::SystemInfo | Op::TrashList => "",
        }
    }

    /// Fəaliyyət siyahısı üçün qısa sətir: "move · 12 fayl" və ya "read · Docs/a.txt".
    fn label(&self) -> String {
        let n = match self {
            Op::Move { items, .. } | Op::Copy { items, .. } => items.len(),
            Op::Delete { paths } | Op::Zip { paths, .. } | Op::TrashRestore { paths } => paths.len(),
            _ => 1,
        };
        if n > 1 {
            format!("{} · {} × ({}…)", self.name(), n, self.path())
        } else {
            format!("{} · {}", self.name(), self.path())
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OpError {
    NotFound,
    OutsideFolders,
    Exists,
    TooLarge,
    Unsupported,
    NoFolders,
    BadPath,
    AccessDenied,
    Other(String),
}

impl OpError {
    /// Müqavilədəki qısa kodlar (index.ts bunları istifadəçi dilinə çevirir).
    fn code(&self) -> &str {
        match self {
            OpError::NotFound => "not_found",
            OpError::OutsideFolders => "outside_folders",
            OpError::Exists => "exists",
            OpError::TooLarge => "too_large",
            OpError::Unsupported => "unsupported",
            OpError::NoFolders => "no_folders",
            // Səhv yazılmış yol da qovluqdan kənar sayılır — bot üçün nəticə eynidir.
            OpError::BadPath => "outside_folders",
            OpError::AccessDenied => "access_denied",
            OpError::Other(s) => s,
        }
    }
}

fn io_err(e: io::Error) -> OpError {
    match e.kind() {
        io::ErrorKind::NotFound => OpError::NotFound,
        io::ErrorKind::PermissionDenied => OpError::AccessDenied,
        io::ErrorKind::AlreadyExists => OpError::Exists,
        _ => OpError::Other(truncate_chars(&e.to_string(), 200)),
    }
}

#[derive(Debug)]
enum Resolved {
    /// Boş yol — seçilmiş qovluqların siyahısı.
    Roots,
    Path {
        folder: Folder,
        /// Qovluqdan sonrakı hissələr (boş = qovluğun özü).
        rel: Vec<String>,
        abs: PathBuf,
    },
}

/// Windows-da fayl adında ola bilməyən simvollar + `/`. `:` həm disk hərfini,
/// həm NTFS alternativ axınlarını ("fayl.txt:gizli") bağlayır.
const BAD_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

fn validate_segment(seg: &str) -> Result<(), OpError> {
    if seg.is_empty() || seg == "." || seg == ".." {
        return Err(OpError::BadPath);
    }
    if seg.chars().any(|c| c.is_control() || BAD_CHARS.contains(&c)) {
        return Err(OpError::BadPath);
    }
    // Windows sondakı nöqtəni və boşluğu atır: "a.txt." = "a.txt".
    // Belə adlar ikili mənalıdır — rədd.
    if seg.ends_with('.') || seg.ends_with(' ') || seg.starts_with(' ') {
        return Err(OpError::BadPath);
    }
    // Cihaz adları (CON, NUL, COM1 …) — genişlənmə ilə də ("nul.txt").
    let stem = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(OpError::BadPath);
    }
    Ok(())
}

fn resolve(folders: &[Folder], path: &str) -> Result<Resolved, OpError> {
    let p = path.trim();
    if p.is_empty() {
        return if folders.is_empty() { Err(OpError::NoFolders) } else { Ok(Resolved::Roots) };
    }
    if folders.is_empty() {
        return Err(OpError::NoFolders);
    }
    if p.starts_with('/') || p.starts_with('~') || p.contains('\\') {
        return Err(OpError::BadPath);
    }
    // Sondakı bir "/" bağışlanır ("Downloads/"), qalan boş hissələr yox.
    let p = p.strip_suffix('/').unwrap_or(p);
    let segs: Vec<&str> = p.split('/').collect();
    for s in &segs {
        validate_segment(s)?;
    }
    let first = fold_name(segs[0]);
    let folder = folders
        .iter()
        .find(|f| fold_name(&f.name) == first)
        .cloned()
        .ok_or(OpError::OutsideFolders)?;
    let rel: Vec<String> = segs[1..].iter().map(|s| s.to_string()).collect();
    let mut abs = folder.path.clone();
    for s in &rel {
        abs.push(s);
    }
    // Sözlə yoxlama kifayət deyil: içəridəki simvolik link / junction
    // qovluqdan kənara apara bilər. Mövcud olan ən yaxın hissəni real yola
    // çevirib kökün içində olduğunu yoxlayırıq.
    ensure_inside(&folder.path, &abs)?;
    Ok(Resolved::Path { folder, rel, abs })
}

fn ensure_inside(root: &Path, target: &Path) -> Result<(), OpError> {
    let root_c = fs::canonicalize(root).map_err(|_| OpError::NotFound)?;
    let mut probe = target.to_path_buf();
    loop {
        match fs::canonicalize(&probe) {
            Ok(c) => {
                return if c.starts_with(&root_c) { Ok(()) } else { Err(OpError::OutsideFolders) };
            }
            Err(_) => {
                if !probe.pop() {
                    return Err(OpError::OutsideFolders);
                }
            }
        }
    }
}

/// Müqavilə formasında yol: "<qovluq adı>/alt/fayl".
fn contract_path(folder: &Folder, rel: &[String]) -> String {
    let mut s = folder.name.clone();
    for r in rel {
        s.push('/');
        s.push_str(r);
    }
    s
}

fn execute(folders: &[Folder], op: &Op, host: &dyn Host) -> Result<Value, OpError> {
    // Yolsuz əməliyyatlar (bufer, sistem, bildiriş, Zibil qutusu, link) —
    // qovluq seçilməsə də işləyir.
    match op {
        Op::ClipboardGet => return extra::op_clipboard_get(),
        Op::ClipboardSet { text } => return extra::op_clipboard_set(text),
        Op::SystemInfo => return extra::op_system_info(),
        Op::Notify { title, body } => {
            host.notify(title, body).map_err(OpError::Other)?;
            return Ok(json!({ "shown": true }));
        }
        Op::Open { target, url: true } => {
            host.open(target, true).map_err(OpError::Other)?;
            return Ok(json!({ "opened": target }));
        }
        Op::TrashList => return extra::op_trash_list(folders),
        Op::TrashRestore { paths } => return extra::op_trash_restore(folders, paths),
        Op::Zip { paths, to } => return extra::op_zip(folders, paths, to),
        Op::Unzip { path, to } => return extra::op_unzip(folders, path, to),
        _ => {}
    }
    let resolved = resolve(folders, op.path())?;
    match op {
        Op::List { .. } => op_list(folders, &resolved),
        Op::Read { max_chars, .. } => op_read(&resolved, *max_chars),
        Op::Search { query, limit, .. } => op_search(folders, &resolved, query, *limit),
        Op::Write { content, mode, .. } => op_write(&resolved, content, mode),
        Op::Move { items, overwrite } => op_transfer(folders, items, *overwrite, false),
        Op::Copy { items, overwrite } => op_transfer(folders, items, *overwrite, true),
        Op::Delete { paths } => op_delete(folders, paths),
        Op::Mkdir { .. } => op_mkdir(&resolved),
        Op::Stats { top, .. } => extra::op_stats(folders, &resolved, *top),
        Op::Duplicates { .. } => extra::op_duplicates(folders, &resolved),
        Op::Open { target, url: false } => {
            let Resolved::Path { abs, .. } = &resolved else { return Err(OpError::Unsupported) };
            fs::symlink_metadata(abs).map_err(io_err)?;
            host.open(&abs.display().to_string(), false).map_err(OpError::Other)?;
            Ok(json!({ "opened": target }))
        }
        // Yuxarıda qaytarılıb — bura çatmır.
        Op::ClipboardGet | Op::ClipboardSet { .. } | Op::SystemInfo | Op::Notify { .. } | Op::Open { .. }
        | Op::TrashList | Op::TrashRestore { .. } | Op::Zip { .. } | Op::Unzip { .. } => Err(OpError::Unsupported),
    }
}

fn iso(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn op_list(folders: &[Folder], r: &Resolved) -> Result<Value, OpError> {
    let Resolved::Path { abs, .. } = r else {
        let entries: Vec<Value> = folders.iter().map(|f| json!({ "name": f.name, "dir": true })).collect();
        return Ok(json!({ "entries": entries }));
    };
    let meta = fs::metadata(abs).map_err(io_err)?;
    if !meta.is_dir() {
        return Err(OpError::Unsupported);
    }
    let mut items: Vec<(bool, String, Value)> = Vec::new();
    let mut truncated = false;
    for entry in fs::read_dir(abs).map_err(io_err)? {
        let Ok(entry) = entry else { continue };
        if items.len() >= LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // fs::metadata linki izləyir: link qovluğa aparırsa "dir" görünür.
        let Ok(m) = fs::metadata(entry.path()) else { continue };
        let mut v = json!({ "name": name, "dir": m.is_dir() });
        if m.is_file() {
            v["size"] = json!(m.len());
        }
        if let Ok(t) = m.modified() {
            v["modified"] = json!(iso(t));
        }
        items.push((m.is_dir(), name.to_lowercase(), v));
    }
    // Əvvəl qovluqlar, sonra fayllar, əlifba ilə — model üçün oxunaqlı.
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let entries: Vec<Value> = items.into_iter().map(|x| x.2).collect();
    let mut out = json!({ "entries": entries });
    if truncated {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Mətn çıxarılmayan tiplər (2-ci mərhələdə PDF/Office oxunmur).
fn binary_kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "heic" | "avif" => "image",
        "zip" | "7z" | "rar" | "gz" | "tar" | "tgz" | "bz2" | "xz" => "archive",
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" => "office",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "audio",
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "wmv" => "video",
        "exe" | "dll" | "msi" | "sys" | "bin" | "iso" | "img" => "binary",
        _ => return None,
    })
}

fn op_read(r: &Resolved, max_chars: usize) -> Result<Value, OpError> {
    let Resolved::Path { abs, .. } = r else {
        return Err(OpError::Unsupported);
    };
    let meta = fs::metadata(abs).map_err(io_err)?;
    if meta.is_dir() {
        return Err(OpError::Unsupported);
    }
    let size = meta.len();
    // PDF, Word, PowerPoint, Excel — mətn çıxarılır (v0.3.0). Skan edilmiş PDF-də
    // mətn olmur: boş qayıdır, `kind` ilə bot bunu istifadəçiyə deyə bilir.
    if let Some((kind, doc)) = extra::document_text(abs, size) {
        let text = match doc {
            Ok(t) => t,
            Err(OpError::TooLarge) => return Err(OpError::TooLarge),
            // Xarab/şifrəli sənəd: xəta yox, "oxunmadı" — bot istifadəçiyə izah edə bilsin.
            Err(e) => return Ok(json!({ "size": size, "kind": kind, "unreadable": e.code() })),
        };
        let total = text.chars().count();
        let mut out = json!({ "text": truncate_chars(&text, max_chars), "size": size, "kind": kind });
        if total > max_chars {
            out["truncated"] = json!(true);
        }
        if text.trim().is_empty() {
            out["empty"] = json!(true);
        }
        return Ok(out);
    }
    if let Some(kind) = binary_kind(abs) {
        return Ok(json!({ "size": size, "kind": kind }));
    }
    // Yalnız lazım olan qədər oxu: UTF-8-də simvol ≤ 4 bayt (+ BOM).
    let want = (max_chars as u64) * 4 + 4;
    let mut buf = Vec::with_capacity(want.min(size) as usize);
    fs::File::open(abs)
        .map_err(io_err)?
        .take(want)
        .read_to_end(&mut buf)
        .map_err(io_err)?;
    let read_all = (buf.len() as u64) >= size;

    let Some(text) = decode_text(&buf, read_all) else {
        return Ok(json!({ "size": size, "kind": "binary" }));
    };
    let total_chars = text.chars().count();
    let truncated = total_chars > max_chars || !read_all;
    let text = if total_chars > max_chars { truncate_chars(&text, max_chars) } else { text };
    let mut out = json!({ "text": text, "size": size });
    if truncated {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Bayt → mətn. Windows Notepad faylları çox vaxt UTF-16 (BOM ilə) olur.
/// İkili fayl (NUL bayt) → None.
fn decode_text(buf: &[u8], complete: bool) -> Option<String> {
    if let Some(rest) = buf.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(utf8_prefix(rest, complete));
    }
    if buf.starts_with(&[0xFF, 0xFE]) || buf.starts_with(&[0xFE, 0xFF]) {
        let le = buf[0] == 0xFF;
        let units: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return Some(char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect());
    }
    let head = &buf[..buf.len().min(8192)];
    if head.contains(&0) {
        return None;
    }
    Some(utf8_prefix(buf, complete))
}

/// Faylın ortasından kəsəndə son simvol yarımçıq qala bilər — onu atırıq,
/// "�" ilə bitməsin. Qalan səhv baytlar "�" olur (köhnə kodlaşdırmalı fayl).
fn utf8_prefix(buf: &[u8], complete: bool) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_string(),
        Err(e) if !complete && e.error_len().is_none() => {
            String::from_utf8_lossy(&buf[..e.valid_up_to()]).to_string()
        }
        Err(_) => String::from_utf8_lossy(buf).to_string(),
    }
}

/// Ad müqayisəsi üçün sadələşdirmə: kiçik hərf, türk İ/ı fərqi silinir
/// ("İş" və "iş", "IŞ" eyni). index.ts-dəki deviceKey ilə eyni məntiq —
/// bot "is" yazsa da "İş" qovluğunu tapsın.
fn fold_name(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| *c != '\u{0307}') // "İ".to_lowercase() = "i" + birləşdirici nöqtə
        .map(|c| if c == 'ı' { 'i' } else { c })
        .collect()
}

/// Sadə şablon: `*` istənilən ardıcıllıq, `?` bir simvol.
fn wildcard(pat: &[char], s: &[char]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while i < s.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

struct Matcher {
    glob: Option<Vec<char>>,
    terms: Vec<String>,
}

impl Matcher {
    fn new(q: &str) -> Option<Matcher> {
        let q = fold_name(q.trim());
        if q.is_empty() {
            return None;
        }
        if q.contains('*') || q.contains('?') {
            return Some(Matcher { glob: Some(q.chars().collect()), terms: vec![] });
        }
        // Bir neçə söz: hamısı adda olmalıdır ("hesabat 2025" → "2025_hesabat.xlsx").
        Some(Matcher { glob: None, terms: q.split_whitespace().map(str::to_string).collect() })
    }

    fn hit(&self, name: &str) -> bool {
        let n = fold_name(name);
        match &self.glob {
            Some(g) => wildcard(g, &n.chars().collect::<Vec<_>>()),
            None => self.terms.iter().all(|t| n.contains(t.as_str())),
        }
    }
}

fn op_search(folders: &[Folder], r: &Resolved, query: &str, limit: usize) -> Result<Value, OpError> {
    let m = Matcher::new(query).ok_or(OpError::Unsupported)?;
    // Başlanğıc nöqtələri: boş yol → bütün seçilmiş qovluqlar.
    let starts: Vec<(Folder, Vec<String>, PathBuf)> = match r {
        Resolved::Roots => folders.iter().map(|f| (f.clone(), vec![], f.path.clone())).collect(),
        Resolved::Path { folder, rel, abs } => {
            if !fs::metadata(abs).map_err(io_err)?.is_dir() {
                return Err(OpError::Unsupported);
            }
            vec![(folder.clone(), rel.clone(), abs.clone())]
        }
    };

    let mut matches = Vec::new();
    let mut visits = 0usize;
    let mut truncated = false;
    // Enə doğru (BFS): üst qovluqdakı nəticələr əvvəl gəlsin.
    let mut queue: std::collections::VecDeque<(Folder, Vec<String>, PathBuf, usize)> =
        starts.into_iter().map(|(f, r, p)| (f, r, p, 0)).collect();
    'outer: while let Some((folder, rel, dir, depth)) = queue.pop_front() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            visits += 1;
            if visits > SEARCH_MAX_VISITS {
                truncated = true;
                break 'outer;
            }
            let Ok(ft) = entry.file_type() else { continue };
            // Simvolik linklərə / junction-lara girmirik: həm dövrə (loop),
            // həm qovluqdan kənara çıxış riski.
            if ft.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let mut child_rel = rel.clone();
            child_rel.push(name.clone());
            if m.hit(&name) {
                if matches.len() >= limit {
                    truncated = true;
                    break 'outer;
                }
                let mut v = json!({ "path": contract_path(&folder, &child_rel) });
                if let Ok(md) = entry.metadata() {
                    if md.is_file() {
                        v["size"] = json!(md.len());
                    } else {
                        v["dir"] = json!(true);
                    }
                    if let Ok(t) = md.modified() {
                        v["modified"] = json!(iso(t));
                    }
                }
                matches.push(v);
            }
            if ft.is_dir() && depth + 1 < SEARCH_MAX_DEPTH {
                queue.push_back((folder.clone(), child_rel, entry.path(), depth + 1));
            }
        }
    }
    let mut out = json!({ "matches": matches });
    if truncated {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Bot bu tipləri yazmır: seçilmiş qovluq "Startup" və ya skriptlərin
/// işlədiyi yer ola bilər — yazılan fayl özü işə düşə bilər. Təsdiq
/// pəncərəsində adı oxumayan istifadəçi üçün əlavə qat.
fn is_executable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "exe" | "com" | "bat" | "cmd" | "ps1" | "psm1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh"
            | "msi" | "msp" | "scr" | "pif" | "lnk" | "url" | "reg" | "hta" | "cpl" | "dll" | "jar"
            | "appref-ms" | "application" | "inf" | "sys"
    )
}

fn op_write(r: &Resolved, content: &str, mode: &WriteMode) -> Result<Value, OpError> {
    let Resolved::Path { folder, rel, abs } = r else {
        return Err(OpError::Unsupported);
    };
    if rel.is_empty() {
        return Err(OpError::Unsupported); // seçilmiş qovluğun özünə yazmaq olmaz
    }
    if content.chars().count() > WRITE_MAX_CHARS {
        return Err(OpError::TooLarge);
    }
    if is_executable_name(rel.last().unwrap()) {
        return Err(OpError::Unsupported);
    }
    let existed = match fs::symlink_metadata(abs) {
        Ok(m) => {
            if m.is_dir() {
                return Err(OpError::Unsupported);
            }
            // Mövcud link üzərinə yazmırıq: link qovluqdan kənara apara bilər
            // (resolve bunu tutur, amma yoxlama ilə yazma arasında link
            // dəyişdirilə bilər — ən sadə yol linkə ümumiyyətlə yazmamaqdır).
            if m.file_type().is_symlink() {
                return Err(OpError::OutsideFolders);
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_err(e)),
    };
    if existed && *mode == WriteMode::Create {
        return Err(OpError::Exists);
    }
    // Ara qovluqlar yoxdursa yaradılır ("Sənədlər/2026/yeni.txt"). resolve()
    // ən yaxın mövcud hissənin kökün içində olduğunu artıq yoxlayıb.
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
        ensure_inside(&folder.path, parent)?;
    }
    let mut opts = fs::OpenOptions::new();
    match mode {
        WriteMode::Create => opts.write(true).create_new(true),
        WriteMode::Overwrite => opts.write(true).create(true).truncate(true),
        WriteMode::Append => opts.append(true).create(true),
    };
    let mut f = opts.open(abs).map_err(io_err)?;
    f.write_all(content.as_bytes()).map_err(io_err)?;
    f.sync_all().map_err(io_err)?;
    let mut out = json!({ "path": contract_path(folder, rel), "bytes": content.len() });
    if !existed {
        out["created"] = json!(true);
    }
    Ok(out)
}

/// Qovluq kopyalamada fayl limiti — bot səhvən böyük qovluğu (məs. node_modules)
/// kopyalayıb diski doldurmasın.
const COPY_MAX_FILES: usize = 20_000;

/// Resolve edilmiş element (qovluğun özü ola bilməz — precheck bunu yoxlayıb).
fn resolve_item(folders: &[Folder], p: &str) -> Result<(Folder, Vec<String>, PathBuf), OpError> {
    match resolve(folders, p)? {
        Resolved::Path { folder, rel, abs } if !rel.is_empty() => Ok((folder, rel, abs)),
        _ => Err(OpError::Unsupported),
    }
}

fn ext_of(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// move/copy: hər element ayrıca; biri alınmasa digərləri davam edir və
/// nəticədə `failed` siyahısında görünür (bot istifadəçiyə deyə bilsin).
fn op_transfer(folders: &[Folder], items: &[(String, String)], overwrite: bool, copy: bool) -> Result<Value, OpError> {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (from, to) in items {
        match transfer_one(folders, from, to, overwrite, copy) {
            Ok(()) => done.push(json!({ "from": from, "to": to })),
            Err(e) => failed.push(json!({ "from": from, "error": e.code() })),
        }
    }
    let key = if copy { "copied" } else { "moved" };
    let mut out = json!({ key: done });
    if !failed.is_empty() {
        out["failed"] = json!(failed);
    }
    Ok(out)
}

fn transfer_one(folders: &[Folder], from: &str, to: &str, overwrite: bool, copy: bool) -> Result<(), OpError> {
    let (_, _, src) = resolve_item(folders, from)?;
    let (dfolder, drel, dst) = resolve_item(folders, to)?;
    let smeta = fs::symlink_metadata(&src).map_err(io_err)?;
    if dst.starts_with(&src) {
        return Err(OpError::Unsupported);
    }
    // Adı dəyişməklə icra olunan fayl yaratmaq olmaz ("qeyd.txt" → "qeyd.bat").
    // Mövcud .exe/.lnk-i eyni uzantı ilə köçürmək olar (masaüstündəki qısayollar).
    let dname = drel.last().unwrap();
    let sname = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if smeta.is_file() && is_executable_name(dname) && ext_of(dname) != ext_of(&sname) {
        return Err(OpError::Unsupported);
    }
    match fs::symlink_metadata(&dst) {
        Ok(dm) => {
            if !overwrite {
                return Err(OpError::Exists);
            }
            if dm.is_dir() || smeta.is_dir() {
                return Err(OpError::Unsupported); // qovluğu qovluğun üzərinə yazmırıq
            }
            // Üzərinə yazılan köhnə fayl itmir — Zibil qutusuna gedir.
            trash::delete(&dst).map_err(|e| OpError::Other(truncate_chars(&e.to_string(), 200)))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(e)),
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
        ensure_inside(&dfolder.path, parent)?;
    }
    if copy {
        if smeta.is_dir() {
            let mut n = 0usize;
            copy_dir(&src, &dst, &mut n)?;
        } else {
            fs::copy(&src, &dst).map_err(io_err)?;
        }
        return Ok(());
    }
    // Eyni diskdə rename ani və atomikdir. Başqa diskə (C: → D:) rename
    // alınmır — onda kopyalayıb mənbəni silirik.
    if fs::rename(&src, &dst).is_ok() {
        return Ok(());
    }
    if smeta.is_dir() {
        let mut n = 0usize;
        copy_dir(&src, &dst, &mut n)?;
        fs::remove_dir_all(&src).map_err(io_err)?;
    } else {
        fs::copy(&src, &dst).map_err(io_err)?;
        fs::remove_file(&src).map_err(io_err)?;
    }
    Ok(())
}

/// Qovluğu rekursiv kopyalayır. Simvolik linkləri atlayır (qovluqdan kənara
/// aparıb oradakı faylları içəri kopyalamasın).
fn copy_dir(src: &Path, dst: &Path, count: &mut usize) -> Result<(), OpError> {
    fs::create_dir_all(dst).map_err(io_err)?;
    for entry in fs::read_dir(src).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let ft = entry.file_type().map_err(io_err)?;
        if ft.is_symlink() {
            continue;
        }
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &to, count)?;
        } else {
            *count += 1;
            if *count > COPY_MAX_FILES {
                return Err(OpError::TooLarge);
            }
            fs::copy(entry.path(), &to).map_err(io_err)?;
        }
    }
    Ok(())
}

/// Silmə = Zibil qutusu (Windows Recycle Bin). Həmişəlik silmə qəsdən yoxdur:
/// model səhv faylı seçsə də istifadəçi geri qaytara bilsin.
fn op_delete(folders: &[Folder], paths: &[String]) -> Result<Value, OpError> {
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for p in paths {
        let r = resolve_item(folders, p).and_then(|(_, _, abs)| {
            fs::symlink_metadata(&abs).map_err(io_err)?;
            trash::delete(&abs).map_err(|e| OpError::Other(truncate_chars(&e.to_string(), 200)))
        });
        match r {
            Ok(()) => deleted.push(json!(p)),
            Err(e) => failed.push(json!({ "path": p, "error": e.code() })),
        }
    }
    let mut out = json!({ "deleted": deleted, "trash": true });
    if !failed.is_empty() {
        out["failed"] = json!(failed);
    }
    Ok(out)
}

fn op_mkdir(r: &Resolved) -> Result<Value, OpError> {
    let Resolved::Path { folder, rel, abs } = r else {
        return Err(OpError::Unsupported);
    };
    if rel.is_empty() {
        return Err(OpError::Unsupported);
    }
    let existed = abs.is_dir();
    if abs.exists() && !existed {
        return Err(OpError::Exists); // eyni adda fayl var
    }
    fs::create_dir_all(abs).map_err(io_err)?;
    ensure_inside(&folder.path, abs)?;
    let mut out = json!({ "path": contract_path(folder, rel) });
    if !existed {
        out["created"] = json!(true);
    }
    Ok(out)
}

// =====================================================================
// 6. Kiçik köməkçilər
// =====================================================================

fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    if cfg!(windows) {
        fold_name(&ca.to_string_lossy()) == fold_name(&cb.to_string_lossy())
    } else {
        ca == cb
    }
}

/// Diskin kökü (C:\, /) və istifadəçi qovluğunun özü (C:\Users\Ad) çox
/// genişdir: içində AppData (brauzer parolları, tokenlər) var. Belə qovluq
/// seçilsə bot onları da görərdi. Alt qovluq (Sənədlər, Endirmələr) seçilməlidir.
fn is_too_broad(p: &Path) -> bool {
    let normal = p.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if normal == 0 {
        return true;
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    if let Some(h) = home {
        if same_path(p, &h) {
            return true;
        }
    }
    // C:\Users, C:\Windows, C:\Program Files kimi bir səviyyəli sistem qovluqları.
    if cfg!(windows) && normal == 1 {
        let n = p.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if matches!(n.as_str(), "users" | "windows" | "program files" | "program files (x86)" | "programdata") {
            return true;
        }
    }
    false
}

// =====================================================================
// 7. Sınaqlar (Linux-da: cargo test)
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    struct NullHost;
    impl Host for NullHost {
        fn approve(&self, _req: ApprovalRequest) -> BoxFuture<Decision> {
            Box::pin(async { Decision::Deny })
        }
        fn status(&self, _s: Status) {}
    }

    fn folder(dir: &Path, name: &str) -> Folder {
        let p = dir.join(name);
        fs::create_dir_all(&p).unwrap();
        Folder { name: name.to_string(), path: fs::canonicalize(p).unwrap() }
    }

    #[test]
    fn segment_rules() {
        for bad in ["..", ".", "", "a:b", "a\\b", "CON", "nul.txt", "com1", "LPT9.log", "x.", "x ", " x", "a*b", "a\u{0}"] {
            assert!(validate_segment(bad).is_err(), "{bad:?} rədd olunmalı idi");
        }
        for ok in ["hesabat.txt", "İş", "com10", "console.log", "a b", ".gizli"] {
            assert!(validate_segment(ok).is_ok(), "{ok:?} qəbul olunmalı idi");
        }
    }

    #[test]
    fn resolve_paths() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "Downloads"), folder(t.path(), "İş")];
        assert!(matches!(resolve(&f, "").unwrap(), Resolved::Roots));
        assert!(matches!(resolve(&[], ""), Err(OpError::NoFolders)));
        assert!(matches!(resolve(&f, "downloads/a.txt"), Ok(Resolved::Path { .. })));
        assert!(matches!(resolve(&f, "iş/a.txt"), Ok(Resolved::Path { .. })), "İ/i qatlanması");
        assert!(matches!(resolve(&f, "IŞ/a.txt"), Ok(Resolved::Path { .. })), "böyük hərf");
        assert!(matches!(resolve(&f, "Downloads/"), Ok(Resolved::Path { .. })));
        for bad in ["Downloads/../x", "/etc/passwd", "~/x", "C:/x", "Downloads\\x", "Downloads//x", "Other/x"] {
            let e = resolve(&f, bad).unwrap_err();
            assert!(matches!(e, OpError::BadPath | OpError::OutsideFolders), "{bad}: {e:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_blocked() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "D")];
        let outside = t.path().join("secret");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("p.txt"), "parol").unwrap();
        std::os::unix::fs::symlink(&outside, f[0].path.join("link")).unwrap();
        assert_eq!(resolve(&f, "D/link/p.txt").unwrap_err(), OpError::OutsideFolders);
        // Hələ olmayan fayl da link vasitəsilə kənara yazıla bilməz.
        assert_eq!(resolve(&f, "D/link/new.txt").unwrap_err(), OpError::OutsideFolders);
        // Axtarış linkə girmir.
        let out = execute(&f, &Op::Search { path: "D".into(), query: "p".into(), limit: 10 }, &NullHost).unwrap();
        assert_eq!(out["matches"].as_array().unwrap().len(), 0);
        // Fayl linkinin üzərinə yazılmır.
        std::os::unix::fs::symlink(outside.join("p.txt"), f[0].path.join("p.txt")).unwrap();
        let w = Op::Write { path: "D/p.txt".into(), content: "x".into(), mode: WriteMode::Overwrite };
        assert_eq!(execute(&f, &w, &NullHost).unwrap_err(), OpError::OutsideFolders);
        assert_eq!(fs::read_to_string(outside.join("p.txt")).unwrap(), "parol");
    }

    #[test]
    fn list_read_search_write() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "Docs")];
        fs::create_dir_all(f[0].path.join("Alt")).unwrap();
        fs::write(f[0].path.join("Hesabat 2025.txt"), "Salam, dünya! Şəki").unwrap();
        fs::write(f[0].path.join("Alt/plan.md"), "# Plan").unwrap();
        fs::write(f[0].path.join("doc.pdf"), b"%PDF-1.4").unwrap();
        fs::write(f[0].path.join("bin.dat"), [1u8, 0, 2, 3]).unwrap();

        // list (kök)
        let v = execute(&f, &Op::List { path: "".into() }, &NullHost).unwrap();
        assert_eq!(v["entries"][0]["name"], "Docs");
        // list (qovluq): əvvəl qovluq
        let v = execute(&f, &Op::List { path: "Docs".into() }, &NullHost).unwrap();
        let e = v["entries"].as_array().unwrap();
        assert_eq!(e[0]["name"], "Alt");
        assert_eq!(e[0]["dir"], true);
        assert!(e.iter().any(|x| x["name"] == "Hesabat 2025.txt" && x["size"].as_u64() == Some("Salam, dünya! Şəki".len() as u64)));
        assert!(e[1]["modified"].as_str().unwrap().ends_with('Z'));

        // read
        let v = execute(&f, &Op::Read { path: "Docs/Hesabat 2025.txt".into(), max_chars: 100 }, &NullHost).unwrap();
        assert_eq!(v["text"], "Salam, dünya! Şəki");
        assert!(v.get("truncated").is_none());
        let v = execute(&f, &Op::Read { path: "Docs/Hesabat 2025.txt".into(), max_chars: 5 }, &NullHost).unwrap();
        assert_eq!(v["text"], "Salam");
        assert_eq!(v["truncated"], true);
        let v = execute(&f, &Op::Read { path: "Docs/doc.pdf".into(), max_chars: 100 }, &NullHost).unwrap();
        assert_eq!(v["kind"], "pdf");
        assert!(v.get("text").is_none() && v.get("unreadable").is_some(), "xarab PDF: {v}");
        let v = execute(&f, &Op::Read { path: "Docs/bin.dat".into(), max_chars: 100 }, &NullHost).unwrap();
        assert_eq!(v["kind"], "binary");
        assert_eq!(
            execute(&f, &Op::Read { path: "Docs/yoxdur.txt".into(), max_chars: 9 }, &NullHost).unwrap_err(),
            OpError::NotFound
        );

        // search: söz, şablon, alt qovluq
        let v = execute(&f, &Op::Search { path: "".into(), query: "hesabat 2025".into(), limit: 10 }, &NullHost).unwrap();
        assert_eq!(v["matches"][0]["path"], "Docs/Hesabat 2025.txt");
        let v = execute(&f, &Op::Search { path: "Docs".into(), query: "*.md".into(), limit: 10 }, &NullHost).unwrap();
        assert_eq!(v["matches"][0]["path"], "Docs/Alt/plan.md");
        let v = execute(&f, &Op::Search { path: "".into(), query: "a".into(), limit: 1 }, &NullHost).unwrap();
        assert_eq!(v["truncated"], true);

        // write: create → exists → overwrite → append; ara qovluq yaranır
        let w = |p: &str, c: &str, m: WriteMode| execute(&f, &Op::Write { path: p.into(), content: c.into(), mode: m }, &NullHost);
        let v = w("Docs/Yeni/qeyd.txt", "bir", WriteMode::Create).unwrap();
        assert_eq!(v["path"], "Docs/Yeni/qeyd.txt");
        assert_eq!(v["created"], true);
        assert_eq!(w("Docs/Yeni/qeyd.txt", "x", WriteMode::Create).unwrap_err(), OpError::Exists);
        let v = w("Docs/Yeni/qeyd.txt", "iki", WriteMode::Overwrite).unwrap();
        assert!(v.get("created").is_none());
        w("Docs/Yeni/qeyd.txt", "+üç", WriteMode::Append).unwrap();
        assert_eq!(fs::read_to_string(f[0].path.join("Yeni/qeyd.txt")).unwrap(), "iki+üç");
        assert_eq!(w("Docs/run.bat", "x", WriteMode::Create).unwrap_err(), OpError::Unsupported);
        assert_eq!(w("Docs", "x", WriteMode::Create).unwrap_err(), OpError::Unsupported);
        let big = "a".repeat(WRITE_MAX_CHARS + 1);
        assert_eq!(w("Docs/big.txt", &big, WriteMode::Create).unwrap_err(), OpError::TooLarge);
    }

    #[test]
    fn decode_variants() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFsalam", true).unwrap(), "salam");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("Şə".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_text(&utf16, true).unwrap(), "Şə");
        // Yarımçıq son simvol atılır (fayl kəsilib oxunanda).
        let s = "aş".as_bytes();
        assert_eq!(decode_text(&s[..2], false).unwrap(), "a");
    }

    #[test]
    fn op_parse() {
        assert!(matches!(Op::parse("write", &json!({"path":"D/a"})), Err(OpError::Unsupported)));
        assert!(matches!(Op::parse("write", &json!({"path":"D/a","content":"x","mode":"zzz"})), Err(OpError::Unsupported)));
        assert!(matches!(Op::parse("rm", &json!({})), Err(OpError::Unsupported)));
        match Op::parse("read", &json!({"path":"D/a","max_chars":10_000_000})).unwrap() {
            Op::Read { max_chars, .. } => assert_eq!(max_chars, READ_MAX_CHARS),
            _ => panic!(),
        }
    }

    #[test]
    fn misc_helpers() {
        assert_eq!(fold_name("İŞ"), fold_name("iş"));
        assert_eq!(fold_name("Işıq"), "işiq");
        assert!(wildcard(&"*.pdf".chars().collect::<Vec<_>>(), &"a.b.pdf".chars().collect::<Vec<_>>()));
        assert!(!wildcard(&"*.pdf".chars().collect::<Vec<_>>(), &"a.pdfx".chars().collect::<Vec<_>>()));
        let now = DateTime::parse_from_rfc3339("2026-11-28T12:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(skew_from_date("Sat, 28 Nov 2026 12:00:30 GMT", now), Some(30_000));
        assert_eq!(skew_from_date("Sat, 28 Nov 2026 12:00:01 GMT", now), Some(0));
        assert_eq!(realtime_url("https://x.supabase.co", "k"), "wss://x.supabase.co/realtime/v1/websocket?apikey=k&vsn=1.0.0");
        let topic = "dost-dev:ab";
        let sig = r#"{"event":"broadcast","payload":{"event":"job","payload":{"job_id":"1"},"type":"broadcast"},"ref":null,"topic":"realtime:dost-dev:ab"}"#;
        assert_eq!(parse_rt(sig, topic, "1"), RtEvent::Signal);
        assert_eq!(parse_rt(&sig.replace("dost-dev:ab", "dost-dev:zz"), topic, "1"), RtEvent::Other);
        let ok = r#"{"event":"phx_reply","payload":{"status":"ok","response":{}},"ref":"1","topic":"realtime:dost-dev:ab"}"#;
        assert_eq!(parse_rt(ok, topic, "1"), RtEvent::Joined);
        let err = ok.replace("\"ok\"", "\"error\"");
        assert!(matches!(parse_rt(&err, topic, "1"), RtEvent::JoinFailed(_)));
        let j: Value = serde_json::from_str(&join_frame(topic, 1)).unwrap();
        assert_eq!(j["topic"], "realtime:dost-dev:ab");
        assert_eq!(j["payload"]["config"]["private"], false);
    }
    // ---------------- v0.3.0 əməliyyatları ----------------

    fn ex(f: &[Folder], op: &str, args: Value) -> Result<Value, OpError> {
        let op = Op::parse(op, &args)?;
        precheck(f, &op).map_err(OpError::Other)?;
        execute(f, &op, &NullHost)
    }

    #[test]
    fn move_copy_mkdir_batch() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "Masaüstü"), folder(t.path(), "Sənədlər")];
        for n in ["a.png", "b.jpg", "c.pdf", "qısayol.lnk"] {
            fs::write(f[0].path.join(n), n).unwrap();
        }
        // Səliqəyə salma: bir işdə çoxlu köçürmə, hədəf qovluqlar avtomatik.
        let v = ex(&f, "move", json!({"items": [
            {"from": "Masaüstü/a.png", "to": "Masaüstü/Şəkillər/a.png"},
            {"from": "Masaüstü/b.jpg", "to": "Masaüstü/Şəkillər/b.jpg"},
            {"from": "Masaüstü/c.pdf", "to": "Sənədlər/PDF/c.pdf"},
            {"from": "Masaüstü/qısayol.lnk", "to": "Masaüstü/Qısayollar/qısayol.lnk"},
            {"from": "Masaüstü/yoxdur.txt", "to": "Masaüstü/x/yoxdur.txt"}
        ]})).unwrap();
        assert_eq!(v["moved"].as_array().unwrap().len(), 4, "{v}");
        assert_eq!(v["failed"][0]["error"], "not_found");
        assert!(f[0].path.join("Şəkillər/b.jpg").exists() && f[1].path.join("PDF/c.pdf").exists());
        assert!(!f[0].path.join("a.png").exists());
        // Mövcud hədəf: overwrite olmadan "exists"
        fs::write(f[0].path.join("d.txt"), "yeni").unwrap();
        fs::write(f[0].path.join("Şəkillər/d.txt"), "köhnə").unwrap();
        let v = ex(&f, "move", json!({"items": [{"from": "Masaüstü/d.txt", "to": "Masaüstü/Şəkillər/d.txt"}]})).unwrap();
        assert_eq!(v["failed"][0]["error"], "exists");
        // Adı dəyişməklə .bat yaratmaq olmaz
        let v = ex(&f, "move", json!({"items": [{"from": "Masaüstü/d.txt", "to": "Masaüstü/d.bat"}]})).unwrap();
        assert_eq!(v["failed"][0]["error"], "unsupported");
        // Qovluğu öz içinə — ön yoxlamada rədd (heç biri icra olunmur)
        assert!(ex(&f, "move", json!({"items": [{"from": "Masaüstü/Şəkillər", "to": "Masaüstü/Şəkillər/alt"}]})).is_err());
        // Seçilmiş qovluğun özünü köçürmək olmaz
        assert!(ex(&f, "move", json!({"items": [{"from": "Masaüstü", "to": "Sənədlər/M"}]})).is_err());
        // Kopya: qovluq rekursiv
        let v = ex(&f, "copy", json!({"items": [{"from": "Masaüstü/Şəkillər", "to": "Sənədlər/Şəkil kopyası"}]})).unwrap();
        assert_eq!(v["copied"].as_array().unwrap().len(), 1);
        assert!(f[1].path.join("Şəkil kopyası/a.png").exists() && f[0].path.join("Şəkillər/a.png").exists());
        // mkdir
        let v = ex(&f, "mkdir", json!({"path": "Sənədlər/2026/Noyabr"})).unwrap();
        assert_eq!(v["created"], true);
        assert!(ex(&f, "mkdir", json!({"path": "Sənədlər"})).is_err(), "kökün özü");
        // Limit
        let many: Vec<Value> = (0..501).map(|i| json!({"from": format!("Masaüstü/{i}"), "to": format!("Masaüstü/x{i}")})).collect();
        assert!(matches!(Op::parse("move", &json!({"items": many})), Err(OpError::TooLarge)));
    }

    #[test]
    fn delete_to_trash_and_restore() {
        // Linux-da freedesktop Zibil qutusu ($XDG_DATA_HOME/Trash) — Windows-da Recycle Bin.
        let t = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", t.path().join("xdg"));
        let f = vec![folder(t.path(), "Desk")];
        fs::write(f[0].path.join("köhnə.txt"), "salam").unwrap();
        fs::create_dir_all(f[0].path.join("Boş qovluq")).unwrap();
        let v = ex(&f, "delete", json!({"paths": ["Desk/köhnə.txt", "Desk/Boş qovluq", "Desk/yoxdur"]})).unwrap();
        assert_eq!(v["deleted"].as_array().unwrap().len(), 2, "{v}");
        assert_eq!(v["trash"], true);
        assert!(!f[0].path.join("köhnə.txt").exists());
        assert!(ex(&f, "delete", json!({"paths": ["Desk"]})).is_err(), "seçilmiş qovluğun özü silinmir");
        let v = ex(&f, "trash_list", json!({})).unwrap();
        let items: Vec<String> = v["items"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_string()).collect();
        assert!(items.contains(&"Desk/köhnə.txt".to_string()), "{v}");
        let v = ex(&f, "trash_restore", json!({"paths": ["Desk/köhnə.txt"]})).unwrap();
        assert_eq!(v["restored"][0], "Desk/köhnə.txt", "{v}");
        assert_eq!(fs::read_to_string(f[0].path.join("köhnə.txt")).unwrap(), "salam");
    }

    #[test]
    fn zip_unzip_safety() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "D")];
        fs::create_dir_all(f[0].path.join("Layihə/alt")).unwrap();
        fs::write(f[0].path.join("Layihə/a.txt"), "aaa").unwrap();
        fs::write(f[0].path.join("Layihə/alt/b.txt"), "bbb").unwrap();
        let v = ex(&f, "zip", json!({"paths": ["D/Layihə"], "to": "D/arxiv.zip"})).unwrap();
        assert_eq!(v["files"], 2, "{v}");
        assert!(ex(&f, "zip", json!({"paths": ["D/Layihə"], "to": "D/arxiv.zip"})).is_err(), "mövcud arxiv");
        assert!(ex(&f, "zip", json!({"paths": ["D/Layihə"], "to": "D/arxiv.txt"})).is_err(), ".zip olmalıdır");
        let v = ex(&f, "unzip", json!({"path": "D/arxiv.zip", "to": "D/Açılmış"})).unwrap();
        assert_eq!(v["files"], 2);
        assert_eq!(fs::read_to_string(f[0].path.join("Açılmış/Layihə/alt/b.txt")).unwrap(), "bbb");
        // Təhlükəli arxiv: ../ ilə kənara yazma və .exe
        let evil = f[0].path.join("pis.zip");
        {
            let mut zw = zip::ZipWriter::new(fs::File::create(&evil).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            zw.start_file("../../qaçış.txt", o).unwrap();
            zw.write_all(b"x").unwrap();
            zw.start_file("virus.exe", o).unwrap();
            zw.write_all(b"MZ").unwrap();
            zw.start_file("normal.txt", o).unwrap();
            zw.write_all(b"ok").unwrap();
            zw.finish().unwrap();
        }
        let v = ex(&f, "unzip", json!({"path": "D/pis.zip", "to": "D/P"})).unwrap();
        assert_eq!(v["files"], 1, "{v}");
        assert_eq!(v["skipped"].as_array().unwrap().len(), 2);
        assert!(!t.path().join("qaçış.txt").exists() && !f[0].path.join("P/virus.exe").exists());
    }

    #[test]
    fn stats_duplicates_documents() {
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "D")];
        let big = vec![7u8; 5000];
        fs::write(f[0].path.join("x.bin"), &big).unwrap();
        fs::create_dir_all(f[0].path.join("alt")).unwrap();
        fs::write(f[0].path.join("alt/x kopya.bin"), &big).unwrap();
        fs::write(f[0].path.join("başqa.bin"), vec![8u8; 5000]).unwrap();
        fs::write(f[0].path.join("kiçik.txt"), "a").unwrap();
        let v = ex(&f, "stats", json!({"path": "D", "top": 2})).unwrap();
        assert_eq!(v["files"], 4);
        assert_eq!(v["bytes"], 15001);
        assert_eq!(v["largest"].as_array().unwrap().len(), 2);
        assert_eq!(v["by_ext"][0]["ext"], "bin");
        let v = ex(&f, "duplicates", json!({"path": ""})).unwrap();
        assert_eq!(v["groups"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["groups"][0]["paths"].as_array().unwrap().len(), 2);
        assert_eq!(v["wasted_bytes"], 5000);

        // docx və pptx: ZIP içində XML
        let mk = |name: &str, files: &[(&str, &str)]| {
            let mut zw = zip::ZipWriter::new(fs::File::create(f[0].path.join(name)).unwrap());
            for (n, c) in files {
                zw.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
                zw.write_all(c.as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        };
        mk("m.docx", &[("word/document.xml", r#"<w:document><w:body><w:p><w:r><w:t>Salam</w:t></w:r><w:r><w:t xml:space="preserve"> dünya &amp; Şəki</w:t></w:r><w:r><w:tab/></w:r></w:p><w:p><w:r><w:t>İkinci</w:t></w:r></w:p></w:body></w:document>"#)]);
        let v = ex(&f, "read", json!({"path": "D/m.docx"})).unwrap();
        assert_eq!(v["kind"], "docx");
        assert_eq!(v["text"].as_str().unwrap().trim(), "Salam dünya & Şəki\nİkinci", "{v}");
        mk("s.pptx", &[("ppt/slides/slide2.xml", "<p:sld><a:p><a:r><a:t>İki</a:t></a:r></a:p></p:sld>"),
                       ("ppt/slides/slide1.xml", "<p:sld><a:p><a:r><a:t>Bir</a:t></a:r></a:p></p:sld>")]);
        let v = ex(&f, "read", json!({"path": "D/s.pptx"})).unwrap();
        let txt = v["text"].as_str().unwrap();
        assert!(txt.find("Bir").unwrap() < txt.find("İki").unwrap(), "slayd sırası: {txt}");
        // Xarab docx → xəta yox, "unreadable"
        fs::write(f[0].path.join("xarab.docx"), "deyil").unwrap();
        let v = ex(&f, "read", json!({"path": "D/xarab.docx"})).unwrap();
        assert!(v.get("unreadable").is_some(), "{v}");
        // Sistem məlumatı — şəxsi sahə yoxdur
        let v = ex(&f, "system_info", json!({})).unwrap();
        assert!(v.get("ram_total").is_some() && v.get("disks").is_some());
        assert!(v.get("host").is_none() && v.get("user").is_none());
    }

    #[test]
    fn approval_flags() {
        let open_exe = Op::parse("open", &json!({"path": "D/setup.exe"})).unwrap();
        assert!(open_exe.is_dangerous());
        assert!(!Op::parse("open", &json!({"path": "D/a.pdf"})).unwrap().is_dangerous());
        let t = tempfile::tempdir().unwrap();
        let f = vec![folder(t.path(), "D")];
        assert!(precheck(&f, &Op::parse("open", &json!({"url": "javascript:alert(1)"})).unwrap()).is_err());
        assert!(precheck(&f, &Op::parse("open", &json!({"url": "file:///C:/x"})).unwrap()).is_err());
        assert!(precheck(&f, &Op::parse("open", &json!({"url": "https://kalbi.pro"})).unwrap()).is_ok());
        let e = precheck(&f, &Op::parse("delete", &json!({"paths": ["D/a", "Başqa/b"]})).unwrap()).unwrap_err();
        assert!(e.starts_with("outside_folders · Başqa/b"), "{e}");
    }

}
