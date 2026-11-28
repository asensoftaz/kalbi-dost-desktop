// Yığım köməkçisi — GitHub Actions-da (Windows) işləyir.
//
// Niyə PowerShell yox, Node: eyni skript Linux-da da işə salınıb sınana
// bilir (makeappx addımından başqa), Actions-da isə Node onsuz da qurulur.
//
// Rejimlər:
//   node scripts/package.mjs check      — uzun yığımdan ƏVVƏL: versiya, sirlər
//   node scripts/package.mjs ci-config  — updater açıq açarını konfiqa yazır
//   node scripts/package.mjs collect    — .exe + .sig → dist/ və latest.json
//   node scripts/package.mjs msix       — Store üçün .msix paketi
//
// Hamısı repo kökündən çağırılır.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const ROOT = process.cwd();
const TAURI_DIR = path.join(ROOT, "src-tauri");
const CONF = JSON.parse(fs.readFileSync(path.join(TAURI_DIR, "tauri.conf.json"), "utf8"));
const VERSION = CONF.version;
const DIST = path.join(ROOT, "dist");

// Fayl adlarında boşluq olmasın: GitHub Releases boşluğu nöqtəyə çevirir və
// latest.json-dakı link faktiki fayl adı ilə tutuşmaz.
const FILE_BASE = "KalBi-Dost";
const SETUP_NAME = `${FILE_BASE}_${VERSION}_x64-setup.exe`;
// Sabit ad: saytdakı "Tətbiqi endir" düyməsi .../releases/latest/download/KalBi-Dost-Setup.exe
// linkinə gedir və hər versiyada dəyişdirilməsi lazım olmur.
const SETUP_STABLE = `${FILE_BASE}-Setup.exe`;

function fail(msg) {
  // ::error:: — Actions bunu iş səhifəsində qırmızı xəbərdarlıq kimi göstərir.
  console.log(`::error::${msg}`);
  process.exit(1);
}

function writeUtf8NoBom(file, text) {
  // BOM-suz yazırıq: latest.json-un əvvəlindəki BOM updater-in JSON
  // oxuyucusunu sındıra bilər.
  fs.writeFileSync(file, text, { encoding: "utf8" });
}

// Sınaq rejimi: teq deyil (əl ilə işə salınıb) VƏ imza açarı hələ yoxdur.
// Niyə lazımdır: açarlar qurulmamışdan əvvəl də Windows yığımının özünü
// (kompilyasiya, NSIS, makeappx) yoxlaya bilək. Bu rejimdə updater imzası
// və latest.json yaranmır, MSIX isə saxta kimliklə yığılır — Store-a
// göndərmək üçün deyil, yalnız manifestin keçdiyini görmək üçün.
// Buraxılışda (teq və ya "publish" seçimi) bu rejim heç vaxt açılmır: orada açar mütləqdir.
const IS_RELEASE = process.env.GITHUB_REF_TYPE === "tag" || process.env.PUBLISH === "true";
const TEST_MODE = !IS_RELEASE && !process.env.TAURI_SIGNING_PRIVATE_KEY;

function repoSlug() {
  return process.env.GITHUB_REPOSITORY || "asensoftaz/kalbi-dost-desktop";
}

// ── check ────────────────────────────────────────────────────────────────
function check() {
  if (!/^\d+\.\d+\.\d+$/.test(VERSION)) {
    // Store MSIX versiyası X.Y.Z.0 olmalıdır; "-beta" kimi əlavələr orada keçmir.
    fail(`tauri.conf.json versiyası sadə X.Y.Z olmalıdır, indi: "${VERSION}"`);
  }
  if (process.env.GITHUB_REF_TYPE === "tag") {
    const tag = process.env.GITHUB_REF_NAME;
    if (tag !== `v${VERSION}`) {
      fail(`Teq "${tag}" ilə tauri.conf.json versiyası "${VERSION}" uyğun gəlmir. Teq v${VERSION} olmalıdır.`);
    }
  }
  if (TEST_MODE) {
    console.log("::warning::SINAQ REJİMİ: imza açarı yoxdur — updater faylları yaranmayacaq, MSIX saxta kimliklə yığılacaq. Bu fayllar paylanmaq üçün deyil.");
    console.log(`Yoxlama keçdi (sınaq). Versiya ${VERSION}.`);
    return;
  }
  if (!process.env.TAURI_SIGNING_PRIVATE_KEY) {
    fail("TAURI_SIGNING_PRIVATE_KEY sirri yoxdur (Settings → Secrets and variables → Actions → Secrets). README-yə baxın.");
  }
  if (!process.env.TAURI_UPDATER_PUBKEY) {
    fail("TAURI_UPDATER_PUBKEY dəyişəni yoxdur (Settings → Secrets and variables → Actions → Variables). README-yə baxın.");
  }
  // Körpü dəyişənləri yoxdursa tətbiq yığılır, amma "Kompüter körpüsü"
  // söndürülmüş olur — səssiz keçməsin deyə xəbərdarlıq.
  if (!process.env.DOST_SUPABASE_URL || !process.env.DOST_SUPABASE_ANON_KEY) {
    console.log("::warning::DOST_SUPABASE_URL / DOST_SUPABASE_ANON_KEY dəyişəni yoxdur (Settings → Secrets and variables → Actions → Variables) — körpü bu yığımda SÖNÜLÜ olacaq.");
  }
  const msix = msixIdentity();
  if (!msix) {
    console.log("::warning::Store kimliyi yoxdur (msix/identity.json) — Store paketi (.msix) bu dəfə yığılmayacaq, yalnız .exe.");
  }
  console.log(`Yoxlama keçdi. Versiya ${VERSION}.`);
}

// ── ci-config ────────────────────────────────────────────────────────────
// Açıq açar repoda saxlanmır, Actions dəyişənindən gəlir: açar dəyişsə
// (məs. itsə və yenisi yaradılsa) kodu dəyişmək lazım olmasın.
// Tauri `--config` faylını əsas konfiqin üstünə birləşdirir.
function ciConfig() {
  const out = path.join(TAURI_DIR, "tauri.ci.conf.json");
  if (TEST_MODE) {
    // Açar olmadan createUpdaterArtifacts açıq qalsa, Tauri yığımı dayandırır.
    writeUtf8NoBom(out, JSON.stringify({ bundle: { createUpdaterArtifacts: false } }, null, 2) + "\n");
    console.log(`Yazıldı (sınaq): ${path.relative(ROOT, out)}`);
    return;
  }
  const pubkey = (process.env.TAURI_UPDATER_PUBKEY || "").trim();
  if (!pubkey) fail("TAURI_UPDATER_PUBKEY boşdur.");
  writeUtf8NoBom(out, JSON.stringify({ plugins: { updater: { pubkey } } }, null, 2) + "\n");
  console.log(`Yazıldı: ${path.relative(ROOT, out)}`);
}

// ── collect ──────────────────────────────────────────────────────────────
function collect() {
  const nsisDir = path.join(TAURI_DIR, "target", "release", "bundle", "nsis");
  if (!fs.existsSync(nsisDir)) fail(`NSIS qovluğu tapılmadı: ${nsisDir}`);

  // Tauri adı "<productName>_<versiya>_x64-setup.exe" qoyur; productName-də
  // boşluq olduğuna görə adı təxmin etmirik, versiya və sonluqla axtarırıq.
  const setups = fs
    .readdirSync(nsisDir)
    .filter((f) => f.endsWith("-setup.exe") && f.includes(`_${VERSION}_`));
  if (setups.length !== 1) {
    fail(`${VERSION} üçün tam bir -setup.exe gözlənilirdi, tapıldı: ${JSON.stringify(setups)}`);
  }
  const setup = path.join(nsisDir, setups[0]);
  const sigFile = `${setup}.sig`;

  fs.mkdirSync(DIST, { recursive: true });
  if (TEST_MODE) {
    fs.copyFileSync(setup, path.join(DIST, SETUP_NAME));
    console.log(`dist/ hazırdır (sınaq, latest.json yoxdur): ${SETUP_NAME}`);
    return;
  }
  if (!fs.existsSync(sigFile)) {
    fail("İmza faylı (.sig) yoxdur — createUpdaterArtifacts işləməyib və ya açar verilməyib.");
  }

  fs.copyFileSync(setup, path.join(DIST, SETUP_NAME));
  fs.copyFileSync(setup, path.join(DIST, SETUP_STABLE));
  fs.copyFileSync(sigFile, path.join(DIST, `${SETUP_NAME}.sig`));

  // latest.json versiyalı fayla işarə edir, sabit ada yox: updater imzanı
  // konkret fayl üçün yoxlayır, "latest" linki isə yeni buraxılış çıxan an
  // başqa fayla keçə bilər.
  const url = `https://github.com/${repoSlug()}/releases/download/v${VERSION}/${SETUP_NAME}`;
  const manifest = {
    version: VERSION,
    notes: process.env.RELEASE_NOTES || "",
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    platforms: {
      "windows-x86_64": {
        signature: fs.readFileSync(sigFile, "utf8").trim(),
        url,
      },
    },
  };
  writeUtf8NoBom(path.join(DIST, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`dist/ hazırdır: ${SETUP_NAME}, ${SETUP_STABLE}, latest.json`);
}

// ── msix ─────────────────────────────────────────────────────────────────
// Store kimliyi. Mənbə: msix/identity.json (Partner Center → Product identity).
// Niyə repoda, sirlərdə yox: bu dəyərlər gizli deyil — hər MSIX paketinin manifestində
// açıq yazılır və Store səhifəsindən də görünür. Repoda olanda dəyişikliyin tarixi
// git-də qalır. Actions dəyişənləri (MSIX_*) verilibsə, onlar fayldan üstündür —
// məs. sınaq üçün başqa Store məhsulu ilə yığmaq lazım olsa.
function msixIdentity() {
  let file = {}
  const f = path.join(ROOT, "msix", "identity.json")
  if (fs.existsSync(f)) file = JSON.parse(fs.readFileSync(f, "utf8"))
  const name = (process.env.MSIX_IDENTITY_NAME || file.name || "").trim()
  const publisher = (process.env.MSIX_PUBLISHER || file.publisher || "").trim()
  const publisherDisplay = (process.env.MSIX_PUBLISHER_DISPLAY_NAME || file.publisherDisplayName || "").trim()
  if (!name || !publisher || !publisherDisplay) {
    // Sınaq rejimində saxta kimlik: makeappx manifesti yoxlasın deyə.
    if (TEST_MODE) return { name: "Test.KalBiDost", publisher: "CN=KalBiDostTest", publisherDisplay: "Test", test: true }
    return null
  }
  return { name, publisher, publisherDisplay }
}

function xmlEscape(s) {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function findMakeAppx() {
  // makeappx Windows SDK-nın bir hissəsidir və windows-latest runner-də
  // quraşdırılıb, amma versiya qovluğu dəyişir — ona görə ən təzəsini axtarırıq.
  const base = path.join(process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)", "Windows Kits", "10", "bin");
  if (!fs.existsSync(base)) return null;
  const versions = fs
    .readdirSync(base)
    .filter((d) => /^10\.\d+\.\d+\.\d+$/.test(d))
    .sort((a, b) => {
      const pa = a.split(".").map(Number);
      const pb = b.split(".").map(Number);
      for (let i = 0; i < 4; i++) if (pa[i] !== pb[i]) return pb[i] - pa[i];
      return 0;
    });
  for (const v of versions) {
    const exe = path.join(base, v, "x64", "makeappx.exe");
    if (fs.existsSync(exe)) return exe;
  }
  return null;
}

function msix() {
  const id = msixIdentity();
  if (!id) {
    console.log("Store kimliyi yoxdur — MSIX addımı ötürülür.");
    return;
  }
  if (!/^CN=/.test(id.publisher)) {
    fail(`MSIX_PUBLISHER "CN=" ilə başlamalıdır (Partner Center → Product identity → Package/Identity/Publisher). İndi: "${id.publisher}"`);
  }

  const exe = path.join(TAURI_DIR, "target", "release", "kalbi-dost.exe");
  const stage = path.join(ROOT, "target-msix", "stage");
  fs.rmSync(path.join(ROOT, "target-msix"), { recursive: true, force: true });
  fs.mkdirSync(path.join(stage, "Assets"), { recursive: true });

  if (fs.existsSync(exe)) {
    fs.copyFileSync(exe, path.join(stage, "kalbi-dost.exe"));
  } else if (process.platform === "win32") {
    fail(`Tətbiq faylı tapılmadı: ${exe}`);
  } else {
    // Linux-da skripti sınayarkən exe olmur — manifest və ikonlar yenə yoxlanılır.
    console.log("(Windows deyil: exe kopyalanmadı, yalnız manifest sınağı)");
  }

  const icons = ["StoreLogo.png", "Square150x150Logo.png", "Square44x44Logo.png", "Square71x71Logo.png"];
  for (const f of icons) {
    const src = path.join(TAURI_DIR, "icons", f);
    if (!fs.existsSync(src)) fail(`İkon yoxdur: ${src} (npx tauri icon app-icon.png işə salınıbmı?)`);
    fs.copyFileSync(src, path.join(stage, "Assets", f));
  }

  const vars = {
    IDENTITY_NAME: id.name,
    PUBLISHER: id.publisher,
    PUBLISHER_DISPLAY_NAME: id.publisherDisplay,
    // Store dördüncü rəqəmin 0 olmasını tələb edir (onu Store özü işlədir).
    VERSION: `${VERSION}.0`,
    DISPLAY_NAME: CONF.productName,
    DESCRIPTION: "KalBi Dost — your personal AI assistant on the desktop.",
  };
  let manifest = fs.readFileSync(path.join(ROOT, "msix", "AppxManifest.xml"), "utf8");
  manifest = manifest.replace(/\{\{([A-Z_]+)\}\}/g, (m, key) => {
    if (!(key in vars)) fail(`Manifestdə naməlum yer: ${m}`);
    return xmlEscape(vars[key]);
  });
  writeUtf8NoBom(path.join(stage, "AppxManifest.xml"), manifest);

  if (process.platform !== "win32") {
    console.log(`Manifest hazırdır: ${path.join(stage, "AppxManifest.xml")}`);
    return;
  }

  const makeappx = findMakeAppx();
  if (!makeappx) fail("makeappx.exe tapılmadı (Windows SDK).");
  fs.mkdirSync(DIST, { recursive: true });
  const out = path.join(DIST, `${FILE_BASE}_${VERSION}_x64${id.test ? "-TEST" : ""}.msix`);
  // /o — köhnə faylın üstünə yaz. Semantik yoxlamanı (/nv) SÖNDÜRMÜRÜK:
  // manifest səhvi Store-da günlərlə gözlədikdən sonra yox, burada çıxsın.
  const r = spawnSync(makeappx, ["pack", "/d", stage, "/p", out, "/o"], { stdio: "inherit" });
  if (r.status !== 0) fail(`makeappx xəta ilə bitdi (kod ${r.status}).`);
  console.log(`MSIX hazırdır: ${path.relative(ROOT, out)}`);
}

const mode = process.argv[2];
const modes = { check, "ci-config": ciConfig, collect, msix };
if (!modes[mode]) fail(`Naməlum rejim: "${mode}". Mümkün: ${Object.keys(modes).join(", ")}`);
modes[mode]();
