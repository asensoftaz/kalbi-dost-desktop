// Tauri-nin yığım skripti: tauri.conf.json-u oxuyur, ikonları və Windows
// resurslarını (versiya məlumatı, manifest) .exe-yə yerləşdirir.
//
// Körpü komandaları burada ACL siyahısına salınır. NİYƏ vacibdir: Tauri
// frontendDist-in (https://dost.kalbi.pro) altındakı səhifələri "lokal"
// sayır. Siyahı olmasaydı, tətbiq komandaları hər lokal mənbəyə — yəni
// sayta da — açıq olardı. Siyahı olanda hər komanda yalnız
// capabilities/bridge.json-da adı çəkilən pəncərələrdə işləyir.
// Yeni komanda əlavə edəndə: həm bura, həm lib.rs → generate_handler!,
// həm də capabilities/bridge.json → "allow-<ad-defisli>".
fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "bridge_state",
            "bridge_pair",
            "bridge_unlink",
            "bridge_add_folder",
            "bridge_remove_folder",
            "bridge_set_read_policy",
            "bridge_set_trust",
            "approval_get",
            "approval_decide",
        ])),
    )
    .expect("tauri-build alınmadı");
    // ui/*.html include_str! ilə yığılır — dəyişəndə yenidən yığılsın.
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-env-changed=DOST_SUPABASE_URL");
    println!("cargo:rerun-if-env-changed=DOST_SUPABASE_ANON_KEY");
}
