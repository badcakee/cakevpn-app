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

pub async fn sign_out(token: &str) {
    let _ = client().post(format!("{API_BASE}/cakevpn/api/v1/signout")).bearer_auth(token).send().await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn api_base_is_a_full_https_url() {
        assert!(super::API_BASE.starts_with("https://"), "API_BASE = {:?}", super::API_BASE);
    }
}
