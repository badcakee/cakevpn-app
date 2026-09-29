# CakeVPN app

Windows and macOS app for CakeVPN. People type their 5-character code, press Connect, and all
their traffic goes through VLESS + Reality + XTLS-Vision to the nearest or least busy location.

## How it fits together

| Part | What it does |
|---|---|
| `src/` | The window: code entry, Connect button, locations, usage, warnings (TypeScript) |
| `src-tauri/` | The app: talks to the panel API and to the helper, reads the Wi-Fi name (Rust, Tauri 2) |
| `helper/` | `cakevpn-helper`, a background service with admin rights that runs sing-box in TUN mode and measures Wi-Fi and tunnel quality |
| `proto/` | The messages the app and helper exchange |

- **Windows:** the installer registers `cakevpn-helper` as a Windows service (it runs as SYSTEM).
- **macOS:** the app installs it as a LaunchDaemon on first run, which asks for the admin password once.
- **Local channel:** the app talks to the helper over a named pipe on Windows and `/var/run/cakevpn-helper.sock` on macOS.

The panel side lives in the 3x-ui fork: `sub/cakeController.go` (app API) and
`web/service/cake*.go`.

## Building

GitHub Actions builds both installers: **Actions → Build CakeVPN → Run workflow**.
Download them from the run's artifacts:
- `CakeVPN-Windows`: an `.exe` installer
- `CakeVPN-macOS`: a `.dmg` for Apple Silicon and Intel

The app talks to `https://147.135.128.62:2096` by default. To use another address, set the repository
variable `CAKEVPN_API` (Settings → Secrets and variables → Actions → Variables).

The installers are not signed:
- **Windows:** SmartScreen shows "Windows protected your PC". Choose *More info → Run anyway*.
- **macOS:** open the app once, then *System Settings → Privacy & Security → Open Anyway*.

### Local checks (Linux)

```bash
cargo test -p cakevpn-proto -p cakevpn-helper -p cakevpn   # needs libwebkit2gtk-4.1-dev for the app crate
npm ci && npm run build
```

The helper can run without touching routes for testing:

```bash
CAKEVPN_HELPER_SOCKET=/tmp/h.sock CAKEVPN_HELPER_DIR=/tmp/h CAKEVPN_HELPER_LOCAL_PORT=11096 \
  target/release/cakevpn-helper run   # sing-box must sit next to the binary
```

## Licenses

sing-box (GPL-3.0-or-later) ships as a separate program next to the app, unmodified, from
https://github.com/SagerNet/sing-box/releases. If you hand the app to others, also point them to
that source.
