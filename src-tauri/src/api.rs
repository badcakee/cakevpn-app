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
    /// Where the server is, for the map. Empty when the panel doesn't say.
    #[serde(default)]
    pub city: String,
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

/// A message from the panel for this person only, shown until they close it.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PersonalMessage {
    pub id: i64,
    pub text: String,
    /// "info" or "warning".
    pub kind: String,
    #[serde(default)]
    pub created_at: i64,
    /// In the inbox: when the person closed it (0 while it is still open).
    #[serde(default)]
    pub closed_at: i64,
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
    /// Newest first; empty from servers that don't send messages yet.
    #[serde(default)]
    pub messages: Vec<PersonalMessage>,
    /// Every recent message, closed ones too, newest first.
    #[serde(default)]
    pub inbox: Vec<PersonalMessage>,
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
    /// With "update_required": the CakeVPN version the panel requires.
    #[serde(default)]
    pub version: Option<String>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ApiError { error: code.into(), message: message.into(), retry_after: None, tries_left: None, version: None }
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

/// The person closed one of the panel's messages.
pub async fn close_message(token: &str, id: i64) -> Result<(), ApiError> {
    let body = serde_json::json!({ "id": id });
    let _: serde_json::Value =
        read(client().post(format!("{API_BASE}/cakevpn/api/v1/message/close")).bearer_auth(token).json(&body).send().await)
            .await?;
    Ok(())
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

/// A speed test uses this many connections at once, like other speed tests
/// do: one connection alone often can't fill a fast line.
const SPEED_STREAMS: usize = 4;

/// Downloads over several connections for up to `seconds` and returns the
/// speed in Mbps. The first half second is left out when there is enough
/// left, while the speed ramps up.
pub async fn speed_download(ticket: &SpeedTicket) -> Result<f64, ApiError> {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    let received = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let client = speed_client(ticket.seconds);
    let tasks: Vec<_> = (0..SPEED_STREAMS)
        .map(|_| {
            let (client, url) = (client.clone(), ticket.down.clone());
            let (received, finished) = (Arc::clone(&received), Arc::clone(&finished));
            tauri::async_runtime::spawn(async move {
                if let Ok(mut response) = client.get(&url).send().await {
                    if response.status().is_success() {
                        while let Ok(Some(chunk)) = response.chunk().await {
                            received.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        }
                    }
                }
                finished.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();

    let limit = Duration::from_secs(ticket.seconds);
    let asked = Instant::now();
    let mut started: Option<Instant> = None;
    let mut warm: Option<(f64, f64)> = None; // seconds and bytes when the warm-up ended
    let (total, elapsed) = loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let total = received.load(Ordering::Relaxed) as f64;
        if started.is_none() && total > 0.0 {
            started = Some(Instant::now());
        }
        let elapsed = started.map(|s| s.elapsed()).unwrap_or_default();
        if warm.is_none() && elapsed >= Duration::from_millis(500) {
            warm = Some((elapsed.as_secs_f64(), total));
        }
        let all_done = finished.load(Ordering::Relaxed) == SPEED_STREAMS;
        let nothing_came = started.is_none() && asked.elapsed() >= Duration::from_secs(10);
        if all_done || elapsed >= limit || nothing_came {
            break (total, elapsed.as_secs_f64());
        }
    };
    for task in &tasks {
        task.abort();
    }
    if total == 0.0 {
        return Err(speed_failed());
    }
    Ok(match warm {
        Some((at, bytes)) if elapsed - at >= 1.0 => mbps(total - bytes, elapsed - at),
        _ => mbps(total, elapsed.max(0.05)),
    })
}

/// What the server measured for one upload.
#[derive(Deserialize)]
struct Uploaded {
    bytes: u64,
    ms: u64,
}

/// How long each upload connection keeps sending.
const UPLOAD_SECONDS: u64 = 6;

/// A request body that keeps sending until `seconds` have passed or `most`
/// bytes went out, without holding it all in memory.
fn upload_body(most: u64, seconds: u64) -> reqwest::Body {
    // Not all zeros, so nothing on the way can shrink it.
    let mut block = vec![0u8; 64 << 10];
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for chunk in block.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    let block = bytes::Bytes::from(block);
    let started = std::time::Instant::now();
    let stream = futures_util::stream::unfold(0u64, move |sent| {
        let block = block.clone();
        async move {
            if sent >= most || started.elapsed() >= Duration::from_secs(seconds) {
                return None;
            }
            let part = block.slice(..(most - sent).min(block.len() as u64) as usize);
            let sent = sent + part.len() as u64;
            Some((Ok::<_, std::io::Error>(part), sent))
        }
    });
    reqwest::Body::wrap_stream(stream)
}

async fn upload(client: reqwest::Client, url: String, most: u64) -> Option<Uploaded> {
    let response = client.post(&url).body(upload_body(most, UPLOAD_SECONDS)).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// Uploads over several connections for a few seconds and returns the speed
/// in Mbps, as the server measured it.
pub async fn speed_upload(ticket: &SpeedTicket) -> Result<f64, ApiError> {
    let client = speed_client(UPLOAD_SECONDS);
    let each = ticket.up_bytes / SPEED_STREAMS as u64;
    let tasks: Vec<_> = (0..SPEED_STREAMS)
        .map(|_| tauri::async_runtime::spawn(upload(client.clone(), ticket.up.clone(), each)))
        .collect();
    let (mut bytes, mut longest) = (0u64, 0u64);
    for task in tasks {
        if let Ok(Some(done)) = task.await {
            bytes += done.bytes;
            longest = longest.max(done.ms);
        }
    }
    if bytes == 0 {
        return Err(speed_failed());
    }
    Ok(mbps(bytes as f64, longest.max(1) as f64 / 1000.0))
}

pub async fn sign_out(token: &str) {
    let _ = client().post(format!("{API_BASE}/cakevpn/api/v1/signout")).bearer_auth(token).send().await;
}

#[cfg(test)]
mod tests {
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
