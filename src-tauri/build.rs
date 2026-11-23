// Tauri-nin yığım skripti: tauri.conf.json-u oxuyur, ikonları və Windows
// resurslarını (versiya məlumatı, manifest) .exe-yə yerləşdirir.
fn main() {
    tauri_build::build()
}
