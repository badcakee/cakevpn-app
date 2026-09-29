fn main() {
    println!("cargo:rerun-if-env-changed=CAKEVPN_API");
    tauri_build::build()
}
