// Keeps a console window from opening next to the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cakevpn_lib::run()
}
