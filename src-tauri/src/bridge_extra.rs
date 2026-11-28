//! Körpünün əlavə əməliyyatları (v0.3.0): sənəd oxuma (PDF/Word/Excel/PowerPoint),
//! ZIP, disk analizi, dublikatlar, mübadilə buferi, sistem məlumatı, Zibil qutusu.
//!
//! `bridge.rs`-in alt moduludur (`mod extra;`) — yol yoxlaması (`resolve`,
//! `ensure_inside`), xəta kodları və limitlər eynidir. NİYƏ ayrıca fayl:
//! `bridge.rs` artıq böyükdür; burada yalnız "nə edir" var, "kim icazə verir"
//! (təsdiq, sessiya rejimi) `bridge.rs`-dədir.
//!
//! Bütün funksiyalar bloklayıcıdır — `spawn_blocking` içində çağırılır.
//! Ağır işlərin (axtarış, hash, arxiv) həm say, həm vaxt limiti var: bot
//! cavabı ~45 saniyə gözləyir, gec bitən iş heç kimə lazım deyil.

use super::*;
use std::collections::HashMap;
use std::hash::Hasher;

/// Sənəd faylları üçün ölçü limiti: 50 MB-dan böyük PDF/Excel-i yaddaşa
/// açmaq tətbiqi yavaşladar, mətnin onsuz da yalnız əvvəli lazımdır.
const DOC_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Ağır əməliyyatların vaxt büdcəsi (bot gözləməsi 45 san-dır).
const HEAVY_BUDGET: Duration = Duration::from_secs(25);
const ZIP_MAX_ENTRIES: usize = 20_000;
/// Açılmış arxivin ümumi ölçü limiti ("zip bombası" — kiçik arxiv, nəhəng məzmun).
const UNZIP_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const CLIPBOARD_MAX_CHARS: usize = 100_000;

// =====================================================================
// Sənəd mətni (read əməliyyatının davamı)
// =====================================================================

/// Mətn çıxarıla bilən sənəd növü: (növ, mətn). Növ tanınmırsa `None` —
/// çağıran adi mətn/binary yoluna qayıdır.
pub(super) fn document_text(path: &Path, size: u64) -> Option<(&'static str, Result<String, OpError>)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let kind: &'static str = match ext.as_str() {
        "pdf" => "pdf",
        "docx" => "docx",
        "pptx" => "pptx",
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => "spreadsheet",
        _ => return None,
    };
    if size > DOC_MAX_BYTES {
        return Some((kind, Err(OpError::TooLarge)));
    }
    let r = match kind {
        "pdf" => pdf_text(path),
        "docx" => ooxml_text(path, "word/document.xml", "w"),
        "pptx" => pptx_text(path),
        _ => sheet_text(path),
    };
    Some((kind, r))
}

/// PDF: pdf-extract bəzi xarab/şifrəli fayllarda panic edə bilər — tətbiq
/// çökməsin deyə catch_unwind. Skan edilmiş PDF-də mətn olmur → boş sətir.
fn pdf_text(path: &Path) -> Result<String, OpError> {
    let p = path.to_path_buf();
    match std::panic::catch_unwind(move || pdf_extract::extract_text(&p)) {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(OpError::Other(format!("pdf: {}", truncate_chars(&e.to_string(), 150)))),
        Err(_) => Err(OpError::Other("pdf: oxunmadı".into())),
    }
}

/// docx/pptx ZIP içində XML-dir. Mətn `<w:t>`/`<a:t>` teqlərindədir; abzas
/// sonu `</w:p>`/`</a:p>`. Tam XML təhlilçisi lazım deyil — teq skanı kifayətdir.
fn ooxml_text(path: &Path, member: &str, ns: &str) -> Result<String, OpError> {
    let f = fs::File::open(path).map_err(io_err)?;
    let mut z = zip::ZipArchive::new(f).map_err(|_| OpError::Other("zip: xarab sənəd".into()))?;
    let mut xml = String::new();
    z.by_name(member)
        .map_err(|_| OpError::Unsupported)?
        .take(DOC_MAX_BYTES)
        .read_to_string(&mut xml)
        .map_err(io_err)?;
    Ok(xml_runs(&xml, ns))
}

/// pptx: hər slayd ayrıca XML (ppt/slides/slideN.xml) — nömrə sırası ilə.
fn pptx_text(path: &Path) -> Result<String, OpError> {
    let f = fs::File::open(path).map_err(io_err)?;
    let mut z = zip::ZipArchive::new(f).map_err(|_| OpError::Other("zip: xarab sənəd".into()))?;
    let mut slides: Vec<(u32, String)> = Vec::new();
    for i in 0..z.len() {
        let Ok(mut e) = z.by_index(i) else { continue };
        let Ok(name) = e.name().map(|n| n.to_string()) else { continue };
        let Some(num) = name
            .strip_prefix("ppt/slides/slide")
            .and_then(|r| r.strip_suffix(".xml"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let mut xml = String::new();
        if (&mut e).take(DOC_MAX_BYTES).read_to_string(&mut xml).is_ok() {
            slides.push((num, xml_runs(&xml, "a")));
        }
    }
    slides.sort_by_key(|s| s.0);
    Ok(slides
        .into_iter()
        .map(|(n, t)| format!("--- Slayd {n} ---\n{}", t.trim()))
        .collect::<Vec<_>>()
        .join("\n\n"))
}

fn xml_runs(xml: &str, ns: &str) -> String {
    let open = format!("<{ns}:t");
    let close = format!("</{ns}:t>");
    let para = format!("</{ns}:p>");
    let mut out = String::new();
    let mut i = 0;
    while i < xml.len() {
        let rest = &xml[i..];
        let next_t = rest.find(&open);
        let next_p = rest.find(&para);
        match (next_t, next_p) {
            (Some(t), p) if p.is_none_or(|p| t < p) => {
                // "<w:t" sonrası ">" və ya " xml:space=...>" ola bilər; "<w:tab"-ı ayırırıq.
                let after = &rest[t + open.len()..];
                if !(after.starts_with('>') || after.starts_with(' ')) {
                    i += t + open.len();
                    continue;
                }
                let Some(gt) = after.find('>') else { break };
                let body = &after[gt + 1..];
                let Some(end) = body.find(&close) else { break };
                out.push_str(&xml_unescape(&body[..end]));
                i += t + open.len() + gt + 1 + end + close.len();
            }
            (_, Some(p)) => {
                out.push('\n');
                i += p + para.len();
            }
            _ => break,
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('&') {
        out.push_str(&rest[..a]);
        let tail = &rest[a..];
        let Some(semi) = tail.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// Excel/ODS: hər vərəq TSV kimi (tab ilə ayrılmış). Model cədvəli belə
/// asan oxuyur və cəm/orta hesablaya bilir.
fn sheet_text(path: &Path) -> Result<String, OpError> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| OpError::Other(format!("excel: {}", truncate_chars(&e.to_string(), 150))))?;
    let mut out = String::new();
    for name in wb.sheet_names().to_owned() {
        let Ok(range) = wb.worksheet_range(&name) else { continue };
        out.push_str(&format!("--- Vərəq: {name} ---\n"));
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| c.to_string().replace(['\t', '\n'], " ")).collect();
            // Tam boş sətirləri atırıq — böyük cədvəllərdə yer tutmasın.
            if cells.iter().any(|c| !c.is_empty()) {
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
            if out.len() > READ_MAX_CHARS * 4 {
                return Ok(out);
            }
        }
        out.push('\n');
    }
    Ok(out)
}

// =====================================================================
// ZIP
// =====================================================================

/// Seçilmiş fayl/qovluqları bir arxivə yığır. Hədəf .zip olmalıdır və mövcud
/// olmamalıdır (üzərinə yazmırıq). Arxivdəki adlar elementin öz adından başlayır.
pub(super) fn op_zip(folders: &[Folder], paths: &[String], to: &str) -> Result<Value, OpError> {
    let (tfolder, trel, tabs) = resolve_item(folders, to)?;
    if !trel.last().unwrap().to_ascii_lowercase().ends_with(".zip") {
        return Err(OpError::Unsupported);
    }
    if fs::symlink_metadata(&tabs).is_ok() {
        return Err(OpError::Exists);
    }
    if let Some(parent) = tabs.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
        ensure_inside(&tfolder.path, parent)?;
    }
    let started = Instant::now();
    let out = fs::File::create(&tabs).map_err(io_err)?;
    let mut zw = zip::ZipWriter::new(out);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut files = 0usize;
    let mut bytes = 0u64;
    let res: Result<(), OpError> = (|| {
        for p in paths {
            let (_, _, abs) = resolve_item(folders, p)?;
            // Arxivin özünü arxivə qatmayaq (hədəf mənbə qovluğunun içindədirsə).
            let base = abs.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut stack = vec![abs.clone()];
            while let Some(cur) = stack.pop() {
                if started.elapsed() > HEAVY_BUDGET || files >= ZIP_MAX_ENTRIES {
                    return Err(OpError::TooLarge);
                }
                let meta = fs::symlink_metadata(&cur).map_err(io_err)?;
                if meta.file_type().is_symlink() || cur == tabs {
                    continue;
                }
                let name = cur
                    .strip_prefix(&base)
                    .unwrap_or(&cur)
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                if meta.is_dir() {
                    zw.add_directory(format!("{name}/"), opts).map_err(|e| OpError::Other(e.to_string()))?;
                    for e in fs::read_dir(&cur).map_err(io_err)?.flatten() {
                        stack.push(e.path());
                    }
                } else {
                    zw.start_file(name, opts).map_err(|e| OpError::Other(e.to_string()))?;
                    let mut f = fs::File::open(&cur).map_err(io_err)?;
                    bytes += io::copy(&mut f, &mut zw).map_err(io_err)?;
                    files += 1;
                }
            }
        }
        Ok(())
    })();
    match res.and_then(|_| zw.finish().map(|_| ()).map_err(|e| OpError::Other(e.to_string()))) {
        Ok(()) => Ok(json!({ "path": contract_path(&tfolder, &trel), "files": files, "bytes": bytes })),
        Err(e) => {
            // Yarımçıq arxiv qalmasın.
            let _ = fs::remove_file(&tabs);
            Err(e)
        }
    }
}

/// Arxivi qovluğa açır. Qoruma: "zip slip" (../ ilə kənara yazma) — yalnız
/// `enclosed_name`; ölçü və say limiti (zip bombası); icra olunan fayllar
/// açılmır (bot .exe yarada bilməz qaydası burada da qüvvədədir) — `skipped`.
pub(super) fn op_unzip(folders: &[Folder], path: &str, to: &str) -> Result<Value, OpError> {
    let (_, _, src) = resolve_item(folders, path)?;
    let (dfolder, drel, dst) = resolve_item(folders, to)?;
    if dst.starts_with(&src) {
        return Err(OpError::Unsupported);
    }
    fs::create_dir_all(&dst).map_err(io_err)?;
    ensure_inside(&dfolder.path, &dst)?;
    let f = fs::File::open(&src).map_err(io_err)?;
    let mut z = zip::ZipArchive::new(f).map_err(|_| OpError::Other("zip: arxiv oxunmadı".into()))?;
    if z.len() > ZIP_MAX_ENTRIES {
        return Err(OpError::TooLarge);
    }
    let started = Instant::now();
    let (mut files, mut total) = (0usize, 0u64);
    let mut skipped: Vec<String> = Vec::new();
    let mut exists = 0usize;
    for i in 0..z.len() {
        if started.elapsed() > HEAVY_BUDGET {
            return Err(OpError::TooLarge);
        }
        let mut e = z.by_index(i).map_err(|e| OpError::Other(e.to_string()))?;
        let Some(rel) = e.enclosed_name() else {
            skipped.push(e.name().map(|n| n.to_string()).unwrap_or_default());
            continue;
        };
        let out = dst.join(&rel);
        if e.is_dir() {
            fs::create_dir_all(&out).map_err(io_err)?;
            continue;
        }
        let fname = rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if is_executable_name(&fname) || validate_segment(&fname).is_err() {
            skipped.push(rel.to_string_lossy().to_string());
            continue;
        }
        total += e.size();
        if total > UNZIP_MAX_BYTES {
            return Err(OpError::TooLarge);
        }
        if out.exists() {
            exists += 1; // mövcud faylın üzərinə yazmırıq
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
            ensure_inside(&dfolder.path, parent)?;
        }
        let mut w = fs::File::create(&out).map_err(io_err)?;
        // Başlıqdakı ölçüyə güvənmirik: real yazılan bayt da limitlə kəsilir.
        io::copy(&mut (&mut e).take(UNZIP_MAX_BYTES), &mut w).map_err(io_err)?;
        files += 1;
    }
    let mut out = json!({ "to": contract_path(&dfolder, &drel), "files": files });
    if !skipped.is_empty() {
        out["skipped"] = json!(skipped.into_iter().take(50).collect::<Vec<_>>());
    }
    if exists > 0 {
        out["exists"] = json!(exists);
    }
    Ok(out)
}

// =====================================================================
// Disk analizi
// =====================================================================

/// Qovluqları gəzir (simvolik linklərə girmir), hər fayl üçün `visit` çağırır.
/// Qaytarır: truncated (limitə çatdı).
fn walk(folders: &[Folder], r: &Resolved, mut visit: impl FnMut(&Folder, &[String], &Path, &fs::Metadata)) -> Result<bool, OpError> {
    let starts: Vec<(Folder, Vec<String>, PathBuf)> = match r {
        Resolved::Roots => folders.iter().map(|f| (f.clone(), vec![], f.path.clone())).collect(),
        Resolved::Path { folder, rel, abs } => vec![(folder.clone(), rel.clone(), abs.clone())],
    };
    let started = Instant::now();
    let mut seen = 0usize;
    let mut stack: Vec<(Folder, Vec<String>, PathBuf)> = starts;
    while let Some((folder, rel, dir)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > SEARCH_MAX_VISITS * 2 || started.elapsed() > HEAVY_BUDGET {
                return Ok(true);
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let mut crel = rel.clone();
            crel.push(e.file_name().to_string_lossy().to_string());
            if ft.is_dir() {
                stack.push((folder.clone(), crel, e.path()));
            } else if let Ok(m) = e.metadata() {
                visit(&folder, &crel, &e.path(), &m);
            }
        }
    }
    Ok(false)
}

/// Qovluğun ölçüsü, ən böyük fayllar, uzantılara görə bölgü.
pub(super) fn op_stats(folders: &[Folder], r: &Resolved, top: usize) -> Result<Value, OpError> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut largest: Vec<(u64, String)> = Vec::new();
    let mut by_ext: HashMap<String, (u64, u64)> = HashMap::new();
    let truncated = walk(folders, r, |folder, rel, _abs, m| {
        let size = m.len();
        files += 1;
        bytes += size;
        let ext = ext_of(rel.last().map(String::as_str).unwrap_or(""));
        let e = by_ext.entry(if ext.is_empty() { "(yox)".into() } else { ext }).or_default();
        e.0 += 1;
        e.1 += size;
        // Kiçik "top" siyahısı: yalnız lazım olanları saxlayırıq (bütün faylları yox).
        if largest.len() < top || largest.last().is_some_and(|l| size > l.0) {
            largest.push((size, contract_path(folder, rel)));
            largest.sort_by_key(|x| std::cmp::Reverse(x.0));
            largest.truncate(top);
        }
    })?;
    let mut exts: Vec<(String, (u64, u64))> = by_ext.into_iter().collect();
    exts.sort_by_key(|x| std::cmp::Reverse(x.1 .1));
    let mut out = json!({
        "files": files,
        "bytes": bytes,
        "largest": largest.iter().map(|(s, p)| json!({ "path": p, "size": s })).collect::<Vec<_>>(),
        "by_ext": exts.iter().take(25).map(|(e, (c, b))| json!({ "ext": e, "count": c, "bytes": b })).collect::<Vec<_>>(),
    });
    if truncated {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Eyni məzmunlu fayllar: əvvəl ölçüyə görə qruplaşdırılır (ucuz), sonra
/// yalnız eyni ölçülülərin məzmunu hash-lənir. Boş fayllar və 1 KB-dan
/// kiçiklər nəzərə alınmır (çox və mənasız).
pub(super) fn op_duplicates(folders: &[Folder], r: &Resolved) -> Result<Value, OpError> {
    let mut by_size: HashMap<u64, Vec<(String, PathBuf)>> = HashMap::new();
    let mut truncated = walk(folders, r, |folder, rel, abs, m| {
        if m.len() >= 1024 {
            by_size.entry(m.len()).or_default().push((contract_path(folder, rel), abs.to_path_buf()));
        }
    })?;
    let started = Instant::now();
    let mut groups: Vec<(u64, Vec<String>)> = Vec::new();
    let mut candidates: Vec<(u64, Vec<(String, PathBuf)>)> = by_size.into_iter().filter(|(_, v)| v.len() > 1).collect();
    // Böyük fayllar əvvəl — ən çox yer qazandıranlar.
    candidates.sort_by_key(|x| std::cmp::Reverse(x.0));
    'outer: for (size, list) in candidates {
        let mut by_hash: HashMap<u64, Vec<String>> = HashMap::new();
        for (cp, abs) in list {
            if started.elapsed() > HEAVY_BUDGET {
                truncated = true;
                break 'outer;
            }
            if let Ok(h) = hash_file(&abs) {
                by_hash.entry(h).or_default().push(cp);
            }
        }
        for (_, paths) in by_hash {
            if paths.len() > 1 {
                groups.push((size, paths));
            }
        }
        if groups.len() >= 200 {
            truncated = true;
            break;
        }
    }
    let wasted: u64 = groups.iter().map(|(s, p)| s * (p.len() as u64 - 1)).sum();
    let mut out = json!({
        "groups": groups.iter().map(|(s, p)| json!({ "size": s, "paths": p })).collect::<Vec<_>>(),
        "wasted_bytes": wasted,
    });
    if truncated {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Məzmun hash-i (SipHash, std). Təhlükəsizlik üçün deyil — yalnız
/// "eyni fayldır?" sualı üçün; ölçü də eyni olduğundan toqquşma praktiki yoxdur.
fn hash_file(p: &Path) -> io::Result<u64> {
    let mut f = fs::File::open(p)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.write(&buf[..n]);
    }
    Ok(h.finish())
}

// =====================================================================
// Mübadilə buferi, sistem məlumatı
// =====================================================================

pub(super) fn op_clipboard_get() -> Result<Value, OpError> {
    let mut cb = arboard::Clipboard::new().map_err(|e| OpError::Other(format!("clipboard: {e}")))?;
    match cb.get_text() {
        Ok(t) => {
            let n = t.chars().count();
            let mut out = json!({ "text": truncate_chars(&t, CLIPBOARD_MAX_CHARS) });
            if n > CLIPBOARD_MAX_CHARS {
                out["truncated"] = json!(true);
            }
            Ok(out)
        }
        // Buferdə mətn yoxdursa (şəkil, boş) — xəta yox, boş nəticə.
        Err(_) => Ok(json!({ "text": "", "empty": true })),
    }
}

pub(super) fn op_clipboard_set(text: &str) -> Result<Value, OpError> {
    if text.chars().count() > CLIPBOARD_MAX_CHARS {
        return Err(OpError::TooLarge);
    }
    let mut cb = arboard::Clipboard::new().map_err(|e| OpError::Other(format!("clipboard: {e}")))?;
    cb.set_text(text.to_string()).map_err(|e| OpError::Other(format!("clipboard: {e}")))?;
    Ok(json!({ "chars": text.chars().count() }))
}

/// Ümumi məlumat: Windows versiyası, prosessor, RAM, disklərdə boş yer.
/// Kompüterin adı, istifadəçi adı, proseslər — qəsdən verilmir (şəxsi).
pub(super) fn op_system_info() -> Result<Value, OpError> {
    use sysinfo::{Disks, MemoryRefreshKind, RefreshKind, System};
    let mut out = json!({
        "os": System::long_os_version().unwrap_or_default(),
        "arch": System::cpu_arch(),
        "cores": System::physical_core_count().unwrap_or(0),
    });
    if let Ok(sys) = System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::everything())) {
        out["ram_total"] = json!(sys.total_memory());
        out["ram_available"] = json!(sys.available_memory());
    }
    if let Ok(disks) = Disks::new_with_refreshed_list() {
        out["disks"] = json!(disks
            .iter()
            .map(|d| json!({
                "mount": d.mount_point().display().to_string(),
                "total": d.total_space(),
                "available": d.available_space(),
                "removable": d.is_removable(),
            }))
            .collect::<Vec<_>>());
    }
    Ok(out)
}

// =====================================================================
// Zibil qutusu: siyahı və geri qaytarma
// =====================================================================

/// Zibil qutusundakı element seçilmiş qovluğun içindən silinibsə, onun
/// müqavilə yolunu qaytarır. Başqa yerdən silinənlər bota göstərilmir.
fn trash_contract_path(folders: &[Folder], original: &Path) -> Option<String> {
    for f in folders {
        let root = fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
        // Orijinal yol artıq mövcud deyil — canonicalize alınmaz; valideyni yoxlayırıq.
        let parent = original.parent()?;
        let pc = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        let full = pc.join(original.file_name()?);
        if let Ok(rel) = full.strip_prefix(&root) {
            let segs: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
            if segs.is_empty() {
                return None;
            }
            return Some(contract_path(f, &segs));
        }
    }
    None
}

pub(super) fn op_trash_list(folders: &[Folder]) -> Result<Value, OpError> {
    let items = trash::os_limited::list().map_err(|e| OpError::Other(truncate_chars(&e.to_string(), 200)))?;
    let mut list: Vec<(i64, String)> = items
        .iter()
        .filter_map(|it| trash_contract_path(folders, &it.original_path()).map(|p| (it.time_deleted, p)))
        .collect();
    list.sort_by_key(|x| std::cmp::Reverse(x.0));
    let total = list.len();
    let mut out = json!({
        "items": list.iter().take(500).map(|(t, p)| json!({
            "path": p,
            "deleted_at": DateTime::<Utc>::from_timestamp(*t, 0).map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true)),
        })).collect::<Vec<_>>(),
    });
    if total > 500 {
        out["truncated"] = json!(true);
    }
    Ok(out)
}

/// Yol üzrə ən son silinmiş nüsxəni geri qaytarır. Yerində eyni adlı fayl
/// varsa qaytarılmır (`exists`) — mövcud faylın üzərinə yazmırıq.
pub(super) fn op_trash_restore(folders: &[Folder], paths: &[String]) -> Result<Value, OpError> {
    let items = trash::os_limited::list().map_err(|e| OpError::Other(truncate_chars(&e.to_string(), 200)))?;
    let mut restored = Vec::new();
    let mut failed = Vec::new();
    for p in paths {
        let want = fold_name(p);
        let pick = items
            .iter()
            .filter(|it| trash_contract_path(folders, &it.original_path()).is_some_and(|cp| fold_name(&cp) == want))
            .max_by_key(|it| it.time_deleted)
            .cloned();
        let Some(it) = pick else {
            failed.push(json!({ "path": p, "error": "not_found" }));
            continue;
        };
        if it.original_path().exists() {
            failed.push(json!({ "path": p, "error": "exists" }));
            continue;
        }
        match trash::os_limited::restore_all([it]) {
            Ok(()) => restored.push(json!(p)),
            Err(e) => failed.push(json!({ "path": p, "error": truncate_chars(&e.to_string(), 150) })),
        }
    }
    let mut out = json!({ "restored": restored });
    if !failed.is_empty() {
        out["failed"] = json!(failed);
    }
    Ok(out)
}
