# KalBi Dost — Windows tətbiqi

Tauri 2 qabığı: `https://dost.kalbi.pro`-nu öz pəncərəsində açır, bağlananda tray-ə kiçilir.
Store-dan kənar `.exe` versiyası özünü GitHub Releases vasitəsilə yeniləyir; Store (MSIX) versiyasını Store özü yeniləyir.

**1-ci mərhələ.** Sayta heç bir yerli imkan (fayl, terminal, ekran) açılmayıb, `capabilities/` qovluğu qəsdən yoxdur. Körpü 2-ci mərhələdə gələcək.

## Quruluş

| Fayl | Nə üçündür |
|---|---|
| `src-tauri/src/lib.rs` | pəncərə, tray, bağlananda gizlənmə, yeni pəncərə linkləri → adi brauzer |
| `src-tauri/src/update.rs` | yeniləmə yoxlanışı (açılışdan 15 san sonra, sonra 6 saatdan bir, və tray-dən əl ilə) |
| `src-tauri/src/store.rs` | MSIX paketindən işləyirmi → updater söndürülür |
| `src-tauri/src/i18n.rs` | tray və yeniləmə mətnləri (az/tr/ru/en/zh, sistem dilinə görə) |
| `msix/AppxManifest.xml` | Store paketi manifesti (şablon) |
| `scripts/package.mjs` | CI köməkçisi: yoxlama, latest.json, MSIX |
| `.github/workflows/release.yml` | Windows yığımı və buraxılış |
| `app-icon.png` | **tək ikon mənbəyi** (1024×1024). Bütün ikonlar CI-da bundan yaradılır |

## Bir dəfəlik quraşdırma

### 0. Repo açıq (public) olmalıdır
Updater `latest.json`-u, saytdakı düymə isə `.exe`-ni girişsiz endirir. Gizli (private) repo-nun buraxılış faylları girişsiz açılmır — yeniləmə və endirmə işləməz.

### 1. Yeniləmə imza açarı (öz komputerinizdə)
PowerShell-də:
```powershell
npx @tauri-apps/cli signer generate -w "$env:USERPROFILE\.tauri\kalbi-dost.key"
```
Parol soruşacaq — yadda saxlayın. İki fayl yaranır: `kalbi-dost.key` (gizli) və `kalbi-dost.key.pub` (açıq).

> ⚠ Gizli açarı və parolu itirsəniz, artıq quraşdırılmış `.exe`-lər bir daha yenilənə bilməz. Açarın ehtiyat nüsxəsini saxlayın.

GitHub → repo → **Settings → Secrets and variables → Actions**:

| Harada | Ad | Dəyər |
|---|---|---|
| Secrets | `TAURI_SIGNING_PRIVATE_KEY` | `kalbi-dost.key` faylının **məzmunu** |
| Secrets | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | parol |
| Variables | `TAURI_UPDATER_PUBKEY` | `kalbi-dost.key.pub` faylının məzmunu |

### 2. Microsoft Store kimliyi (MSIX üçün)
Partner Center → **Apps and games → New product → MSIX or PWA app** (⚠ "EXE or MSI app" yox — o yol pullu kod imzası tələb edir) → adı rezerv edin (məs. "KalBi Dost").
Sonra məhsulun **Product management → Product identity** səhifəsindən üç dəyəri **Variables**-a köçürün:

| Ad | Product identity-dəki sahə | Nümunə |
|---|---|---|
| `MSIX_IDENTITY_NAME` | Package/Identity/Name | `12345Asensoft.KalBiDost` |
| `MSIX_PUBLISHER` | Package/Identity/Publisher | `CN=XXXXXXXX-XXXX-...` |
| `MSIX_PUBLISHER_DISPLAY_NAME` | Package/Properties/PublisherDisplayName | `Asensoft` |

Bu dəyişənlər yoxdursa, yığım yenə keçir, sadəcə `.msix` yaradılmır.

## Buraxılış

1. `src-tauri/tauri.conf.json` → `"version"`-u artırın (sadə `X.Y.Z`).
2. Commit edib push edin, sonra iki yoldan biri:
   - **Actions → release → Run workflow → "Buraxılış et" işarələ → Run** — teqi özü yaradır;
   - və ya yerli: `git tag v0.1.1 && git push --tags`.
3. Actions yığır (~10–15 dəq, sonrakılar keş hesabına daha tez) və Release yaradır.

Teq versiya ilə uyğun gəlmirsə və ya sirlər yoxdursa, yığım ilk dəqiqədə aydın səbəblə dayanır.

Sınaq üçün buraxılışsız yığım: **Actions → release → Run workflow** — fayllar iş səhifəsində "Artifacts" altında olur.

## Store-a göndərmək

Partner Center → məhsul → **Submission** → Packages bölməsinə `KalBi-Dost_<v>_x64.msix` faylını yükləyin.
Hər yeni göndərişdə versiya əvvəlkindən böyük olmalıdır (teq artırıldıqca avtomatik belə olur).

**runFullTrust izahı** (Submission options → Restricted capabilities, hazır mətn):
> KalBi Dost is a desktop client built with Tauri and Microsoft Edge WebView2. It runs as a standard Win32 desktop process, which requires runFullTrust. The app shows a system tray icon so the assistant stays available when the window is closed. Future versions will let the user explicitly allow the assistant to read files in folders the user selects; every such action is confirmed by the user in a native dialog.

WebView2 runtime Windows 11-də və yenilənmiş Windows 10-da əvvəlcədən var; MSIX onu ayrıca quraşdırmır.

## MSIX-i yerli sınamaq (imzasız)

Store rejimini (updater-in sönməsi, tray-də "Yeniləmələri yoxla" olmaması) Store-a göndərmədən sınamaq üçün:
1. Windows **Settings → System → For developers → Developer Mode** — açın.
2. Actions iş səhifəsindən `kalbi-dost-msix-stage-...` artifaktını endirib açın.
3. Həmin qovluqda PowerShell:
   ```powershell
   Add-AppxPackage -Register .\AppxManifest.xml
   ```
4. Start menyusunda "KalBi Dost" görünür. Silmək: `Get-AppxPackage *KalBiDost* | Remove-AppxPackage`.

## Sayt üçün

- Tətbiqin içində sayt `window.kalbiDesktop` obyektini görür: `{ platform: 'windows', version: '0.1.0', store: false }`. "Tətbiqi endir" düyməsini gizlətmək üçün bunu yoxlayın.
- Sabit endirmə linki (Store hazır olana qədər):
  `https://github.com/asensoftaz/kalbi-dost-desktop/releases/latest/download/KalBi-Dost-Setup.exe`
- `target="_blank"` linklər tətbiqdə yox, istifadəçinin adi brauzerində açılır.

## Yerli yığım (istəyə görə)

```bash
npm ci
npx tauri dev      # debug, konsol ilə
# Açarsız yerli quraşdırıcı: updater imzasını söndürürük, yoxsa yığım
# "gizli açar yoxdur" deyib dayanır.
npx tauri build --config '{"bundle":{"createUpdaterArtifacts":false}}'
```
