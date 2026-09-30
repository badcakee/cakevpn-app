//! Local channel between the app and the helper: a named pipe on Windows and a
//! Unix socket elsewhere. One JSON request per line, one JSON response per line.

use crate::tunnel::Tunnel;
use cakevpn_proto::{Request, Response};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Longest request line accepted; a connect request is well under 1 KB.
/// Room for a connect request with full skip lists.
const MAX_LINE: usize = 64 * 1024;

async fn handle(tunnel: &Arc<Tunnel>, line: &str) -> Response {
    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Response { ok: false, error: Some(format!("bad request: {e}")), status: tunnel.status().await, pings: None }
        }
    };
    let mut pings = None;
    let result = match request {
        Request::Connect { params } => tunnel.connect(params).await,
        Request::Disconnect => {
            tunnel.disconnect().await;
            Ok(())
        }
        Request::Status => Ok(()),
        Request::Ping { targets } => tunnel.ping(targets).await.map(|measured| pings = Some(measured)),
    };
    let status = tunnel.status().await;
    match result {
        Ok(()) => Response { ok: true, error: None, status, pings },
        Err(e) => Response { ok: false, error: Some(e), status, pings: None },
    }
}

async fn serve_client<S: AsyncRead + AsyncWrite + Unpin>(tunnel: Arc<Tunnel>, stream: S) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        match (&mut reader).take(MAX_LINE as u64).read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            return; // too long or cut off
        }
        let response = handle(&tunnel, line.trim()).await;
        let mut out = serde_json::to_vec(&response).unwrap_or_default();
        out.push(b'\n');
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

#[cfg(unix)]
pub async fn listen(tunnel: Arc<Tunnel>, path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    // The app runs as the signed-in user. It can only ask for connect,
    // disconnect and status, and connect takes checked connection data only.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(serve_client(Arc::clone(&tunnel), stream));
    }
}

#[cfg(windows)]
pub async fn listen(tunnel: Arc<Tunnel>, path: &str) -> std::io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let security = pipe_security::Attributes::new()?;
    let mut first = true;
    loop {
        // first_pipe_instance stops another program from grabbing the name before us.
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .create_with_security_attributes_raw(path, security.as_ptr())?
        };
        first = false;
        server.connect().await?;
        tokio::spawn(serve_client(Arc::clone(&tunnel), server));
    }
}

#[cfg(windows)]
mod pipe_security {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    /// SYSTEM and Administrators get full access; signed-in users may read and write.
    const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";

    pub struct Attributes {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: Box<SECURITY_ATTRIBUTES>,
    }

    unsafe impl Send for Attributes {}
    unsafe impl Sync for Attributes {}

    impl Attributes {
        pub fn new() -> std::io::Result<Self> {
            let wide: Vec<u16> = SDDL.encode_utf16().chain(std::iter::once(0)).collect();
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), 1, &mut descriptor, std::ptr::null_mut())
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let attributes = Box::new(SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            });
            Ok(Attributes { descriptor, attributes })
        }

        pub fn as_ptr(&self) -> *mut c_void {
            &*self.attributes as *const SECURITY_ATTRIBUTES as *mut c_void
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.descriptor as _);
            }
        }
    }
}
