//! Installs a CakeVPN update for the app, with the helper's own rights, so
//! Windows doesn't ask for permission (the app would need an administrator
//! to install for everyone on the computer).
//!
//! The helper runs as SYSTEM, so it only runs an installer that (1) carries
//! a valid signature from CakeVPN's update key, the same check the app's
//! updater makes, and (2) is not older than the helper itself. It copies the
//! checked bytes into a folder only SYSTEM and administrators can write,
//! and runs that copy, so nothing can be swapped in between.

/// The minisign public key the release workflow signs updates with (the
/// "pubkey" in src-tauri/tauri.conf.json).
const UPDATE_KEY: &str = "RWRq0HoFflggzM3fBpQjCIYo5O09PUpVqRvB26rewjwH5LsRbKg5Imt3";
/// No CakeVPN installer is this big; a bigger file isn't read at all.
#[cfg(windows)]
const MAX_INSTALLER_BYTES: u64 = 300 * 1024 * 1024;

/// "1.8.3" from a signature's trusted comment ("timestamp:… file:… version:1.8.3").
fn signed_version(trusted_comment: &str) -> Option<&str> {
    trusted_comment.split('\t').find_map(|part| part.strip_prefix("version:")).map(str::trim)
}

fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) < parts(b)
}

/// Checks an installer against the update key and returns the version it
/// was signed as. `signature` is the base64 text of a .sig file, as in latest.json.
pub fn verify(bytes: &[u8], signature: &str, own_version: &str) -> Result<String, String> {
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .ok_or("the update's signature is unreadable")?;
    let key = minisign_verify::PublicKey::from_base64(UPDATE_KEY).map_err(|e| e.to_string())?;
    let signature = minisign_verify::Signature::decode(&text).map_err(|_| "the update's signature is unreadable".to_string())?;
    key.verify(bytes, &signature, true).map_err(|_| "the update isn't signed by CakeVPN".to_string())?;
    let version = signed_version(signature.trusted_comment()).ok_or("the update's signature names no version")?;
    if older(version, own_version) {
        return Err(format!("the update ({version}) is older than this CakeVPN ({own_version})"));
    }
    Ok(version.to_string())
}

/// Checks the installer at `path` and starts it silently. Returns once it
/// runs; the installer then stops the helper itself, replaces CakeVPN and
/// starts the new helper.
#[cfg(windows)]
pub fn install(data_dir: &std::path::Path, path: &str, signature: &str) -> Result<String, String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    let mut bytes = vec![];
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_INSTALLER_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read the update: {e}"))?;
    if bytes.len() as u64 > MAX_INSTALLER_BYTES {
        return Err("the update is too big".into());
    }
    let version = verify(&bytes, signature, env!("CARGO_PKG_VERSION"))?;

    let dir = data_dir.join("update");
    private_dir(&dir)?;
    let installer = dir.join("cakevpn-update-setup.exe");
    std::fs::write(&installer, &bytes).map_err(|e| format!("cannot keep the update: {e}"))?;
    // Started through cmd so the installer isn't the helper's child: the
    // installer stops the helper (and its children) before replacing it.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    std::process::Command::new("cmd.exe")
        .args(["/c", "start", "\"\"", "/b"])
        .arg(&installer)
        .args(["/S", "/UPDATE"])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("cannot start the update: {e}"))?;
    Ok(version)
}

/// A fresh folder only SYSTEM and administrators can write. Whatever was
/// there before is removed first: it may have been made by someone else.
#[cfg(windows)]
fn private_dir(dir: &std::path::Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|_| "An update is already being installed.".to_string())?;
    }
    // Protected (nothing inherited): full access for SYSTEM and administrators only.
    let sddl: Vec<u16> = "D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)".encode_utf16().chain(std::iter::once(0)).collect();
    let wide: Vec<u16> = dir.as_os_str().encode_wide_lossy();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // Fails when the folder exists again: then someone raced us, and nothing runs.
        let made = CreateDirectoryW(wide.as_ptr(), &attributes);
        LocalFree(descriptor as _);
        if made == 0 {
            return Err(format!("cannot make the update folder: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(windows)]
trait EncodeWide {
    fn encode_wide_lossy(&self) -> Vec<u16>;
}

#[cfg(windows)]
impl EncodeWide for std::ffi::OsStr {
    fn encode_wide_lossy(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_signed_version() {
        assert_eq!(signed_version("timestamp:1790853228\tfile:CakeVPN_1.8.3_x64-setup.exe\tversion:1.8.3"), Some("1.8.3"));
        assert_eq!(signed_version("timestamp:1\tfile:x.exe"), None);
        assert!(older("1.8.3", "1.8.10") && !older("1.9.0", "1.8.10") && !older("1.8.5", "1.8.5"));
    }

    /// With CAKEVPN_UPDATE_TEST=<folder holding CakeVPN_1.8.3_x64-setup.exe and its .sig>.
    #[test]
    fn accepts_a_real_cakevpn_installer_only() {
        let Ok(dir) = std::env::var("CAKEVPN_UPDATE_TEST") else { return };
        let bytes = std::fs::read(format!("{dir}/CakeVPN_1.8.3_x64-setup.exe")).unwrap();
        let sig = std::fs::read_to_string(format!("{dir}/CakeVPN_1.8.3_x64-setup.exe.sig")).unwrap();
        assert_eq!(verify(&bytes, &sig, "1.8.0"), Ok("1.8.3".to_string()));
        assert!(verify(&bytes, &sig, "1.8.4").unwrap_err().contains("older"), "an older installer must not replace a newer CakeVPN");
        let mut changed = bytes.clone();
        changed[bytes.len() / 2] ^= 1;
        assert_eq!(verify(&changed, &sig, "1.8.0"), Err("the update isn't signed by CakeVPN".to_string()));
    }

    #[test]
    fn refuses_what_isnt_signed_by_cakevpn() {
        // A real CakeVPN signature, for other bytes than these.
        let sig = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVScTBIb0ZmbGdnekc2TU5vT0N6ZlNTL3orRWpKQnVia3VVTVMzb0wzWXE4V0h3NHFXYUF0Y1hCREYrbVNubFF0UmFnY0QxeVNieHRPRlUzclhQaWRMaDUyNGxJdWhYU0FrPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkwODUzMjI4CWZpbGU6Q2FrZVZQTl8xLjguM194NjQtc2V0dXAuZXhlCXZlcnNpb246MS44LjMKd3NaVXVQSG9QVzF0SEhkLzVYVG1KOGF5VWc3UUtOQXUvSWNGT1lKMzJHZGZBZDRBN3VoYmNjZVMzNWxLZWMzSDl3SkluWjNHYnQ5YlNPWFpQMWhDRHc9PQo=";
        assert_eq!(verify(b"not the installer", sig, "1.8.0"), Err("the update isn't signed by CakeVPN".to_string()));
        assert!(verify(b"x", "not base64!", "1.8.0").is_err());
    }
}
