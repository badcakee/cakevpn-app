//! What the app remembers between runs: the sign-in token (in the system
//! keychain) and a random id for this device (in the app's data folder).

use std::path::Path;

const KEYRING_SERVICE: &str = "CakeVPN";
const KEYRING_USER: &str = "device-token";

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()
}

pub fn token() -> Option<String> {
    entry()?.get_password().ok().filter(|t| !t.is_empty())
}

pub fn save_token(token: &str) -> Result<(), String> {
    entry()
        .ok_or("the system keychain is not available")?
        .set_password(token)
        .map_err(|e| format!("could not save the sign-in: {e}"))
}

pub fn clear_token() {
    if let Some(e) = entry() {
        let _ = e.delete_credential();
    }
}

/// A random id made once per install. It lets the server count wrong codes
/// per device instead of per network.
pub fn device_id(data_dir: &Path) -> String {
    let path = data_dir.join("device-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if id.len() >= 8 {
            return id;
        }
    }
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).expect("system random numbers");
    let id: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&path, &id);
    id
}

/// A name the panel shows next to the code, like "Mia-PC (Windows)".
pub fn device_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().trim_end_matches(".local").to_string();
    let os = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows",
        other => other,
    };
    let mut name = format!("{host} ({os})");
    name.truncate(60);
    name
}
