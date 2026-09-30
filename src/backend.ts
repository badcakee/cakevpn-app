// Typed wrappers around the Rust commands in src-tauri/src/lib.rs.
import { invoke } from "@tauri-apps/api/core";

export interface Plan {
  id: string;
  name: string;
  /** 0 means no speed cap. */
  mbps: number;
}

export interface Load {
  percent: number;
  level: "low" | "medium" | "high";
}

export interface Location {
  id: string;
  name: string;
  country: string;
  online: boolean;
  load: Load | null;
  /** The speed test can run against this location. */
  speedTest?: boolean;
}

/** A message from the panel for every app. `id` changes with every new message. */
export interface Announcement {
  id: number;
  text: string;
  kind: "info" | "warning";
}

export interface DayUsage {
  /** Like "2026-09-30". */
  day: string;
  bytes: number;
}

/** What the person switched on in Settings; sent along with every connect. */
export interface ConnectOptions {
  blockAds: boolean;
  killSwitch: boolean;
  /** Websites that skip the VPN, like "mybank.com". */
  bypassDomains: string[];
  /** Apps that skip the VPN, like "steam.exe". */
  bypassApps: string[];
}

export interface Invite {
  code: string;
  joined: boolean;
  createdAt: number;
}

export interface Account {
  plan: Plan;
  /** Plan speed plus the invite bonus; 0 means no cap. */
  speedMbps: number;
  usage: { month: string; bytes: number };
  locations: Location[];
  referral: {
    mbpsPerFriend: number;
    maxFriends: number;
    friends: number;
    bonusMbps: number;
    invites: Invite[];
  };
  /** Where this user's traffic leaves from; empty strings when shared or unknown. */
  ips?: { ipv4: string; ipv6: string };
  announcement?: Announcement | null;
}

export interface ApiError {
  error: string;
  message: string;
  retryAfter?: number;
  triesLeft?: number;
}

export type TunnelState = "disconnected" | "connecting" | "connected" | "failed";

export interface Status {
  version: string;
  state: TunnelState;
  error: string | null;
  connectedSince: number | null;
  upBytes: number;
  downBytes: number;
}

export interface Overview {
  helper: "ok" | "missing" | "outdated";
  status: Status | null;
  /** What is wrong and whose problem it is: the Wi-Fi, the internet connection, a busy location or the VPN. */
  banner: { kind: "wifi" | "internet" | "load" | "vpn"; message: string } | null;
  locationId: string | null;
  /** False while the window is hidden and CakeVPN sits in the tray. */
  windowVisible?: boolean;
}

export const backend = {
  /** `offline` means the server couldn't be reached and `account` is the copy saved earlier. */
  loadSession: () => invoke<{ signedIn: boolean; account: Account | null; offline?: boolean }>("load_session"),
  redeem: (code: string) => invoke<Account>("redeem", { code }),
  refreshAccount: () => invoke<Account>("refresh_account"),
  signOut: () => invoke<void>("sign_out"),
  connect: (locationId: string, options: ConnectOptions) => invoke<Status>("connect", { locationId, options }),
  disconnect: () => invoke<Status>("disconnect"),
  overview: () => invoke<Overview>("overview"),
  installHelper: () => invoke<void>("install_helper"),
  pingLocations: () => invoke<Record<string, number | null>>("ping_locations"),
  settingsInfo: () => invoke<{ version: string; autostart: boolean }>("settings_info"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  createInvite: () => invoke<Account>("create_invite"),
  deleteInvite: (code: string) => invoke<Account>("delete_invite", { code }),
  /** The last 30 days, oldest first. */
  usageHistory: () => invoke<DayUsage[]>("usage_history"),
  /** Starts a speed test (the server may refuse: once a minute, a few a day) and returns the download Mbps. */
  speedTestDownload: () => invoke<number>("speed_test_download"),
  /** The upload half of the test just started, in Mbps. */
  speedTestUpload: () => invoke<number>("speed_test_upload"),
  checkUpdate: () => invoke<{ version: string; notes: string | null } | null>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  /** How far the update download is; `total` is 0 while the size isn't known. */
  updateProgress: () => invoke<{ downloaded: number; total: number }>("update_progress"),
};

/** Tauri hands back command errors as the value the Rust side returned. */
export function asApiError(e: unknown): ApiError {
  if (e && typeof e === "object" && "message" in e) return e as ApiError;
  return { error: "error", message: String(e) };
}
