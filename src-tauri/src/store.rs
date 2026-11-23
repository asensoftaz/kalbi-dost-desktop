//! Tətbiq Microsoft Store (MSIX) paketindən işləyirmi?
//!
//! Niyə lazımdır: Store versiyasını Store özü yeniləyir. Əgər MSIX içində
//! Tauri updater-i işə salsaq, o NSIS quraşdırıcısını endirib işlədəcək və
//! tətbiqin ikinci (Store-dan kənar) nüsxəsi yaranacaq. Ona görə Store-da
//! updater tamamilə söndürülür.
//!
//! Niyə qovluq yolu yox, Windows API: `C:\Program Files\WindowsApps\` yolu
//! dəyişə bilər (məs. başqa diskə köçürülmüş Store tətbiqləri). Paket
//! kimliyinin olub-olmaması isə dəqiq cavabdır.

#[cfg(windows)]
pub fn is_packaged() -> bool {
    use windows_sys::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

    let mut len: u32 = 0;
    // Bufer vermədən çağırırıq: paket varsa ERROR_INSUFFICIENT_BUFFER qaytarır
    // (ad uzunluğunu bildirmək üçün), paket yoxdursa APPMODEL_ERROR_NO_PACKAGE.
    let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
    // Yalnız "paket yoxdur" cavabı updater-i açır. Başqa istənilən kod
    // (gözlənilməz olsa belə) "paketlidir" sayılır: səhvən updater-i Store
    // içində işə salmaq, səhvən söndürməkdən daha pisdir.
    rc != APPMODEL_ERROR_NO_PACKAGE
}

#[cfg(not(windows))]
pub fn is_packaged() -> bool {
    // Windows-dan kənarda MSIX yoxdur (yalnız Linux-da yoxlama yığımı üçün).
    false
}
