//! What the app remembers between runs: the sign-in token (in the system
//! keychain), a random id for this device and a sealed copy of the account
//! (both in the app's data folder).

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
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

const ACCOUNT_FILE: &str = "account.sealed";

/// The saved account is sealed with a key made from the sign-in token, so it
/// can only be read on this computer while it is signed in.
fn account_key(token: &str) -> LessSafeKey {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("cakevpn-saved-account:{token}").as_bytes());
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, digest.as_ref()).expect("a 32-byte key"))
}

/// Keeps the account (with its connection details) for times when the
/// server can't be reached, such as networks that block its port.
pub fn save_account(data_dir: &Path, token: &str, account_json: &[u8]) -> Result<(), String> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let mut sealed = account_json.to_vec();
    account_key(token)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut sealed)
        .map_err(|_| "could not seal the saved account".to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let tmp = data_dir.join(format!("{ACCOUNT_FILE}.tmp"));
    std::fs::write(&tmp, &out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, data_dir.join(ACCOUNT_FILE)).map_err(|e| e.to_string())
}

/// The account saved by `save_account`, if it was saved with this token.
pub fn saved_account(data_dir: &Path, token: &str) -> Option<Vec<u8>> {
    let mut raw = std::fs::read(data_dir.join(ACCOUNT_FILE)).ok()?;
    if raw.len() <= NONCE_LEN {
        return None;
    }
    let mut sealed = raw.split_off(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(&raw).ok()?;
    let plain = account_key(token).open_in_place(nonce, Aad::empty(), &mut sealed).ok()?;
    Some(plain.to_vec())
}

pub fn forget_account(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(ACCOUNT_FILE));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cakevpn-store-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saved_account_needs_the_same_token() {
        let dir = temp_dir("token");
        save_account(&dir, "token-one", b"{\"plan\":1}").unwrap();
        assert_eq!(saved_account(&dir, "token-one").as_deref(), Some(&b"{\"plan\":1}"[..]));
        assert_eq!(saved_account(&dir, "token-two"), None);
        forget_account(&dir);
        assert_eq!(saved_account(&dir, "token-one"), None);
    }

    #[test]
    fn saved_account_is_not_readable_on_disk_and_detects_changes() {
        let dir = temp_dir("sealed");
        save_account(&dir, "t", b"uuid 24c7a93b-bbc7-4f9f-bccb-2bfe437c2fbb").unwrap();
        let path = dir.join(ACCOUNT_FILE);
        let mut raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(4).any(|w| w == b"uuid"));
        let last = raw.len() - 1;
        raw[last] ^= 1;
        std::fs::write(&path, &raw).unwrap();
        assert_eq!(saved_account(&dir, "t"), None);
    }
}
