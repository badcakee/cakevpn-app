//! Who may use the helper's data folder on Windows (C:\ProgramData\CakeVPN).
//!
//! A folder made in ProgramData lets every user of the computer read its
//! files and add new ones. This one holds the tunnel settings (the sign-in to
//! the VPN server and the password of sing-box's control port), and the
//! helper, which runs as SYSTEM, writes there and starts sing-box in it. So
//! it belongs to SYSTEM and administrators only. Users may still look into
//! it and read helper-crash.log, which the app shows when the helper has
//! trouble.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT};

/// Owned by administrators; full access for SYSTEM and administrators,
/// passed on to everything made inside; users may only list the folder itself.
const FOLDER: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a9;;;BU)";
/// A file users may read too.
const READABLE: &str = "O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;BU)";
/// The files the helper makes again for every connection. Older helpers
/// made them readable by everyone.
const REMADE: [&str; 5] = ["config.json", "ads.srs", crate::skip::FILE, "skip-addresses.json.new", "sing-box.log"];

/// Makes the folder (if needed) SYSTEM's and administrators' only. Run when
/// the helper starts, before anything is written there.
pub fn lock(dir: &Path) -> Result<(), String> {
    // A link would have the helper write wherever it points.
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0) {
        std::fs::remove_dir(dir).or_else(|_| std::fs::remove_file(dir)).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    lock_folder(dir)?;
    // Nobody else can add anything now. Of what is already there, links go,
    // and so do the files that are made again (the new ones then take the
    // folder's permissions). The crash log and the update folder stay.
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        let attributes = meta.file_attributes();
        let link = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let remade = attributes & FILE_ATTRIBUTE_DIRECTORY == 0 && REMADE.iter().any(|name| entry.file_name() == *name);
        if !link && !remade {
            continue;
        }
        let _ = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            std::fs::remove_dir(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
    }
    Ok(())
}

/// Lets users read one file of the folder.
pub fn readable_by_users(file: &Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    };
    let wide = wide(file);
    let descriptor = Descriptor::new(READABLE)?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let (mut present, mut defaulted) = (0, 0);
    unsafe {
        if GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let what = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        let result = SetNamedSecurityInfoW(wide.as_ptr(), SE_FILE_OBJECT, what, owner, std::ptr::null_mut(), dacl, std::ptr::null());
        if result != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(result as i32).to_string());
        }
    }
    Ok(())
}

/// Gives the folder itself the permissions in FOLDER. Through a handle that
/// is the folder and not a link, and without passing them down to what is
/// inside (a link in there would carry them elsewhere).
fn lock_folder(dir: &Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    };
    let wide = wide(dir);
    let descriptor = Descriptor::new(FOLDER)?;
    unsafe {
        // No FILE_SHARE_DELETE: the folder can't be swapped while it is open.
        let handle = CreateFileW(
            wide.as_ptr(),
            READ_CONTROL | WRITE_DAC | WRITE_OWNER,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let result = if GetFileInformationByHandle(handle, &mut info) == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            Err("it is not a plain folder".to_string())
        } else if SetKernelObjectSecurity(handle, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, descriptor.0) == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        };
        CloseHandle(handle);
        result
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// A security descriptor made from SDDL, freed when dropped.
struct Descriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn new(sddl: &str) -> Result<Descriptor, String> {
        use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        let sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Descriptor(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0 as _) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner and permissions of `path` in SDDL.
    fn sddl_of(path: &Path) -> String {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
        let wide = wide(path);
        let what = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut text: *mut u16 = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe {
            let n = std::ptr::null_mut();
            assert_eq!(GetNamedSecurityInfoW(wide.as_ptr(), SE_FILE_OBJECT, what, n, n, n as _, n as _, &mut descriptor), 0);
            assert_ne!(ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor, 1, what, &mut text, &mut len), 0);
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len as usize)).trim_end_matches('\0').to_string();
            LocalFree(text as _);
            LocalFree(descriptor as _);
            sddl
        }
    }

    /// With CAKEVPN_FOLDER_TEST=1, as an administrator (setting the owner needs
    /// it), on real Windows: Wine keeps no permissions, only Unix file modes.
    #[test]
    fn only_system_and_administrators_get_in() {
        if std::env::var("CAKEVPN_FOLDER_TEST").is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("cakevpn-folder-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("update")).unwrap();
        // Left by an older helper, readable by everyone.
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        std::fs::write(dir.join("helper-crash.log"), "1 crash\n").unwrap();
        lock(&dir).unwrap();
        assert!(!dir.join("config.json").exists(), "the old tunnel settings are removed");
        assert!(dir.join("helper-crash.log").exists() && dir.join("update").exists());
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        std::fs::write(dir.join("sing-box.log"), "").unwrap();
        readable_by_users(&dir.join("helper-crash.log")).unwrap();

        let folder = sddl_of(&dir);
        println!("folder: {folder}");
        assert!(folder.starts_with("O:BA") && folder.contains("D:P"), "{folder}");
        for name in ["config.json", "sing-box.log"] {
            let file = sddl_of(&dir.join(name));
            println!("{name}: {file}");
            assert!(file.contains(";SY)") && file.contains(";BA)"), "{name}: {file}");
            for user in [";BU)", ";AU)", ";WD)", ";CO)", ";IU)"] {
                assert!(!file.contains(user), "{name} lets {user} in: {file}");
            }
        }
        let log = sddl_of(&dir.join("helper-crash.log"));
        println!("helper-crash.log: {log}");
        assert!(log.contains(";BU)"), "the app can't read the crash log: {log}");
        // A second start changes nothing that is needed.
        lock(&dir).unwrap();
        assert!(dir.join("helper-crash.log").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
