//! Talks to the panel's CakeVPN API (`/cakevpn/api/v1` on the subscription port).

use cakevpn_proto::ConnectParams;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Set CAKEVPN_API when building to point the app at another server.
/// CI passes an empty value when the setting is unset, which also means the default.
pub const API_BASE: &str = match option_env!("CAKEVPN_API") {
    Some(url) if !url.is_empty() => url,
    _ => "https://147.135.128.62:2096",
};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// 0 means no speed cap.
    pub mbps: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Usage {
    pub month: String,
    pub bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Load {
    pub percent: u32,
    pub level: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub id: String,
    pub name: String,
    pub country: String,
    pub online: bool,
    pub load: Option<Load>,
    /// Kept out of what the window sees; only the tunnel needs it.
    #[serde(skip_serializing)]
    pub connect: Option<ConnectParams>,
    /// The speed test can run against this location.
    #[serde(default)]
    pub speed_test: bool,
}

/// A message from the panel for every app.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Announcement {
    /// Changes with every new message, so a closed one stays closed.
    pub id: i64,
    pub text: String,
    /// "info" or "warning".
    pub kind: String,
}

/// One day of the usage graph.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DayUsage {
    pub day: String,
    pub bytes: u64,
}

/// Where and how long to run a speed test. The server signs the addresses
/// and limits how much they can be used.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpeedTicket {
    pub down: String,
    pub up: String,
    pub seconds: u64,
    pub up_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub code: String,
    pub joined: bool,
    pub created_at: i64,
}

/// Each friend who signs in with one of your invites adds `mbps_per_friend`
/// to your speed, for up to `max_friends` friends.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Referral {
    pub mbps_per_friend: u32,
    pub max_friends: u32,
    pub friends: u32,
    pub bonus_mbps: u32,
    pub invites: Vec<Invite>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub plan: Plan,
    /// The plan speed with the invite bonus; 0 means no cap.
    #[serde(default)]
    pub speed_mbps: u32,
    pub usage: Usage,
    pub locations: Vec<Location>,
    #[serde(default)]
    pub referral: Referral,
    #[serde(default)]
    pub ips: Ips,
    #[serde(default)]
    pub announcement: Option<Announcement>,
}

/// Where this user's traffic leaves from; empty when unknown or shared.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Ips {
    pub ipv4: String,
    pub ipv6: String,
}

/// An error the window can show. `error` is a short code such as
/// `wrong_code`, `locked`, `signed_out` or `offline`.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
    pub message: String,
    #[serde(default)]
    pub retry_after: Option<u64>,
    #[serde(default)]
    pub tries_left: Option<u32>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ApiError { error: code.into(), message: message.into(), retry_after: None, tries_left: None }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("CakeVPN/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

async fn read<T: for<'de> Deserialize<'de>>(response: Result<reqwest::Response, reqwest::Error>) -> Result<T, ApiError> {
    let response = response.map_err(|e| {
        if e.is_builder() {
            // The request never left the computer: the server address built into the app is wrong.
            ApiError::new("bad_build", format!("This copy of CakeVPN has a wrong server address ({API_BASE:?}). Get a new download."))
        } else {
            ApiError::new("offline", "Can't reach CakeVPN. Check your internet connection and try again.")
        }
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|_| ApiError::new("offline", "The connection dropped. Try again."))?;
    if status.is_success() {
        return serde_json::from_slice(&body).map_err(|_| ApiError::new("bad_answer", "CakeVPN sent an answer this app doesn't understand. Update the app."));
    }
    Err(serde_json::from_slice(&body)
        .unwrap_or_else(|_| ApiError::new("server_error", format!("CakeVPN had a problem ({status}). Try again later."))))
}

#[derive(Deserialize)]
struct Token {
    token: String,
}

pub async fn redeem(code: &str, device_id: &str, device_name: &str) -> Result<String, ApiError> {
    let body = serde_json::json!({ "code": code, "deviceId": device_id, "deviceName": device_name });
    let token: Token = read(client().post(format!("{API_BASE}/cakevpn/api/v1/redeem")).json(&body).send().await).await?;
    Ok(token.token)
}

pub async fn account(token: &str) -> Result<Account, ApiError> {
    read(client().get(format!("{API_BASE}/cakevpn/api/v1/account")).bearer_auth(token).send().await).await
}

/// Makes an invite code for a friend.
pub async fn create_invite(token: &str) -> Result<Invite, ApiError> {
    read(client().post(format!("{API_BASE}/cakevpn/api/v1/invite")).bearer_auth(token).send().await).await
}

/// Takes back an invite code nobody has used yet.
pub async fn delete_invite(token: &str, code: &str) -> Result<(), ApiError> {
    let body = serde_json::json!({ "code": code });
    let _: serde_json::Value =
        read(client().post(format!("{API_BASE}/cakevpn/api/v1/invite/delete")).bearer_auth(token).json(&body).send().await)
            .await?;
    Ok(())
}

/// The last 30 days of traffic, oldest first.
pub async fn usage_history(token: &str) -> Result<Vec<DayUsage>, ApiError> {
    #[derive(Deserialize)]
    struct Days {
        days: Vec<DayUsage>,
    }
    let days: Days =
        read(client().get(format!("{API_BASE}/cakevpn/api/v1/usage")).bearer_auth(token).send().await).await?;
    Ok(days.days)
}

/// Asks for one speed test against a location. The server refuses when the
/// last test was under a minute ago or the day's tests are used up.
pub async fn speed_ticket(token: &str, location: &str) -> Result<SpeedTicket, ApiError> {
    let body = serde_json::json!({ "location": location });
    read(client().post(format!("{API_BASE}/cakevpn/api/v1/speedtest")).bearer_auth(token).json(&body).send().await).await
}

fn speed_client(seconds: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(seconds + 8))
        .user_agent(concat!("CakeVPN/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

fn speed_failed() -> ApiError {
    ApiError::new("speed_test", "The speed test didn't finish. Try again in a minute.")
}

fn mbps(bytes: f64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        0.0
    } else {
        bytes * 8.0 / seconds / 1_000_000.0
    }
}

/// Downloads for up to `seconds` and returns the speed in Mbps. The first
/// half second is left out when there is enough left, while the speed ramps up.
pub async fn speed_download(ticket: &SpeedTicket) -> Result<f64, ApiError> {
    let mut response =
        speed_client(ticket.seconds).get(&ticket.down).send().await.map_err(|_| speed_failed())?;
    if !response.status().is_success() {
        return Err(speed_failed());
    }
    let limit = Duration::from_secs(ticket.seconds);
    let mut started: Option<std::time::Instant> = None;
    let mut total = 0f64;
    let mut warm: Option<(f64, f64)> = None; // seconds and bytes when the warm-up ended
    while let Ok(Some(chunk)) = response.chunk().await {
        let start = *started.get_or_insert_with(std::time::Instant::now);
        total += chunk.len() as f64;
        let elapsed = start.elapsed();
        if warm.is_none() && elapsed >= Duration::from_millis(500) {
            warm = Some((elapsed.as_secs_f64(), total));
        }
        if elapsed >= limit {
            break;
        }
    }
    let elapsed = started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
    if total == 0.0 {
        return Err(speed_failed());
    }
    Ok(match warm {
        Some((at, bytes)) if elapsed - at >= 1.0 => mbps(total - bytes, elapsed - at),
        _ => mbps(total, elapsed),
    })
}

/// How much to upload after a small first upload showed `bytes_per_sec`:
/// about five seconds' worth, within what the ticket and memory allow.
fn upload_size(bytes_per_sec: f64, left: u64) -> usize {
    const MOST: u64 = 48 << 20;
    ((bytes_per_sec * 5.0) as u64).clamp(256 << 10, MOST).min(left) as usize
}

/// Uploads and returns the speed in Mbps, as measured by the server.
pub async fn speed_upload(ticket: &SpeedTicket) -> Result<f64, ApiError> {
    #[derive(Deserialize)]
    struct Measured {
        bytes: u64,
        ms: u64,
    }
    async fn send(ticket: &SpeedTicket, size: usize) -> Result<Measured, ApiError> {
        // Not all zeros, so nothing on the way can shrink it.
        let mut body = vec![0u8; size];
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for chunk in body.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
        }
        let response =
            speed_client(ticket.seconds).post(&ticket.up).body(body).send().await.map_err(|_| speed_failed())?;
        if !response.status().is_success() {
            return Err(speed_failed());
        }
        response.json().await.map_err(|_| speed_failed())
    }
    const FIRST: usize = 256 << 10;
    let first = send(ticket, FIRST.min(ticket.up_bytes as usize)).await?;
    let rate = first.bytes as f64 / (first.ms.max(1) as f64 / 1000.0);
    let left = ticket.up_bytes.saturating_sub(first.bytes);
    if left < FIRST as u64 {
        return Ok(mbps(first.bytes as f64, first.ms.max(1) as f64 / 1000.0));
    }
    let main = send(ticket, upload_size(rate, left)).await?;
    Ok(mbps(main.bytes as f64, main.ms.max(1) as f64 / 1000.0))
}

pub async fn sign_out(token: &str) {
    let _ = client().post(format!("{API_BASE}/cakevpn/api/v1/signout")).bearer_auth(token).send().await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn upload_size_fits_the_speed_and_the_ticket() {
        use super::upload_size;
        // A slow line still sends enough to measure.
        assert_eq!(upload_size(10_000.0, 30_000_000), 256 << 10);
        // 25 Mbps: about five seconds' worth.
        assert_eq!(upload_size(3_125_000.0, 30_000_000), 15_625_000);
        // A fast line is held to what the ticket has left, and to 48 MB.
        assert_eq!(upload_size(100_000_000.0, 20_000_000), 20_000_000);
        assert_eq!(upload_size(100_000_000.0, 80 << 20), 48 << 20);
    }

    /// Runs the real download and upload against a server when
    /// CAKEVPN_SPEED_DOWN and CAKEVPN_SPEED_UP hold a ticket's addresses.
    #[test]
    fn speed_test_against_a_server() {
        let (Ok(down), Ok(up)) = (std::env::var("CAKEVPN_SPEED_DOWN"), std::env::var("CAKEVPN_SPEED_UP")) else { return };
        let up_bytes = std::env::var("CAKEVPN_SPEED_UP_BYTES").ok().and_then(|b| b.parse().ok()).unwrap_or(30_000_000);
        let ticket = super::SpeedTicket { down, up, seconds: 8, up_bytes };
        let (d, u) = tauri::async_runtime::block_on(async {
            (super::speed_download(&ticket).await, super::speed_upload(&ticket).await)
        });
        println!("download {d:?} Mbps, upload {u:?} Mbps");
        assert!(d.unwrap() > 0.0);
        assert!(u.unwrap() > 0.0);
    }

    #[test]
    fn api_base_is_a_full_https_url() {
        assert!(super::API_BASE.starts_with("https://"), "API_BASE = {:?}", super::API_BASE);
    }
}
