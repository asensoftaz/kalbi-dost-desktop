// Release yığımında Windows konsol pəncərəsi açılmasın deyə "windows" alt sistemi.
// Debug-da konsol qalır ki, log-ları görə bilək.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kalbi_dost_lib::run();
}
