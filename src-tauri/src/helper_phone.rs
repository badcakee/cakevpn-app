//! On a phone there is no separate helper: the tunnel runs inside the app
//! (see phone.rs), and requests go to it directly, with the same answers a
//! computer's helper would give.

use cakevpn_proto::{Request, Response};

/// Why the tunnel could not be asked: it isn't set up yet.
pub const HELPER_MISSING: &str = "helper_missing";

pub async fn ask(request: Request) -> Result<Response, String> {
    let tunnel = crate::phone::tunnel().ok_or_else(|| HELPER_MISSING.to_string())?;
    Ok(tunnel.answer(request).await)
}
