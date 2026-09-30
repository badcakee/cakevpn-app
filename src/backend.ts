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
  banner: { kind: "wifi" | "load" | "slow"; message: string } | null;
  locationId: string | null;
}

export const backend = {
  /** `offline` means the server couldn't be reached and `account` is the copy saved earlier. */
  loadSession: () => invoke<{ signedIn: boolean; account: Account | null; offline?: boolean }>("load_session"),
  redeem: (code: string) => invoke<Account>("redeem", { code }),
  refreshAccount: () => invoke<Account>("refresh_account"),
  signOut: () => invoke<void>("sign_out"),
  connect: (locationId: string) => invoke<Status>("connect", { locationId }),
  disconnect: () => invoke<Status>("disconnect"),
  overview: () => invoke<Overview>("overview"),
  installHelper: () => invoke<void>("install_helper"),
  pingLocations: () => invoke<Record<string, number | null>>("ping_locations"),
  settingsInfo: () => invoke<{ version: string; autostart: boolean }>("settings_info"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  createInvite: () => invoke<Account>("create_invite"),
  deleteInvite: (code: string) => invoke<Account>("delete_invite", { code }),
  checkUpdate: () => invoke<{ version: string; notes: string | null } | null>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
};

/** Tauri hands back command errors as the value the Rust side returned. */
export function asApiError(e: unknown): ApiError {
  if (e && typeof e === "object" && "message" in e) return e as ApiError;
  return { error: "error", message: String(e) };
}
