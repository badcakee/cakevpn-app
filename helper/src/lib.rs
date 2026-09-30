//! The CakeVPN tunnel: sing-box, the checks of how the connection is doing,
//! and the answers to the app's requests.
//!
//! On a computer it runs as cakevpn-helper (src/main.rs), a program with the
//! admin rights a network interface needs, which the app asks over a local
//! channel. On Android the app runs it itself: Android hands the app the VPN
//! interface, and `tunnel::Attach` connects it to sing-box.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod dns;
#[cfg(not(target_os = "android"))]
pub mod ipc;
pub mod ping;
pub mod quality;
pub mod singbox;
pub mod tunnel;
