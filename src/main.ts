import { listen } from "@tauri-apps/api/event";
import { Account, Announcement, asApiError, backend, ConnectOptions, DayUsage, Location, Overview } from "./backend";
import { LANGUAGES, locale, pickLanguage, setLanguage, t, watch } from "./i18n";
import { attachMap, drawMap, mapHtml, markCountries, setPlaces, showPlace } from "./map";
import { placeOf } from "./places";

// ---------- state ----------

type Screen = "loading" | "code" | "home" | "setup" | "settings" | "forced";
type Theme = "system" | "light" | "dark";
type SettingsTab = "general" | "connection" | "notifications" | "protection" | "skip" | "account" | "messages" | "invites" | "about";
/** What is wrong with the connected location: nothing, it stopped responding, or it got slow from high load. */
type Trouble = "" | "down" | "slow";

const saved = {
  get: (key: string) => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  set: (key: string, value: string) => {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* the setting just won't be remembered */
    }
  },
};

/** The pings measured last time, so they are there when the app opens with the VPN already on. */
function savedPings(): Record<string, number | null> {
  try {
    const pings = JSON.parse(saved.get("pings") || "{}");
    return pings && typeof pings === "object" ? pings : {};
  } catch {
    return {};
  }
}

function savedList(key: string): string[] {
  try {
    const list = JSON.parse(saved.get(key) || "[]");
    return Array.isArray(list) ? list.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

const isMac = navigator.userAgent.includes("Mac");
/** Control+Option+V on a Mac; with Shift on Windows, where Ctrl+Alt is also AltGr for typing. */
const DEFAULT_SHORTCUT = isMac ? "Control+Alt+V" : "Control+Alt+Shift+V";

const state = {
  screen: "loading" as Screen,
  account: null as Account | null,
  overview: null as Overview | null,
  pings: savedPings(),
  /** How far the update download is, 0 to 1; null while the size isn't known. */
  updateProgress: null as number | null,
  /** "best" or a location id; remembered between runs. */
  choice: saved.get("location") || "best",
  autoConnect: saved.get("autoConnect") === "1",
  theme: (saved.get("theme") as Theme) || "system",
  autostart: false,
  appVersion: "",
  busy: false,
  codeError: "",
  codeNotice: "",
  lockedUntil: 0,
  actionError: "",
  settingsError: "",
  inviteError: "",
  /** The invite code made last, highlighted in the list. */
  newInvite: "",
  /** A newer version found on GitHub, if any. */
  update: null as { version: string } | null,
  updating: false,
  updateMessage: "",
  lastBytes: null as { at: number; up: number; down: number } | null,
  speed: { up: 0, down: 0 },
  /** The CakeVPN version the panel requires (the forced-update screen). */
  forcedVersion: "",
  /** The forced update was started by itself once; again only with the button. */
  forcedTried: false,
  /** What Windows says is wrong with the background service (fix screen). */
  helperProblem: null as { kind: string; detail: string } | null,
  /** Why CakeVPN moved to another location, shown under the button until moveNoticeUntil. */
  moveNotice: "",
  moveNoticeUntil: 0,
  /** The server can't be reached (some networks block it); the saved account is in use. */
  offline: false,
  /** Protection settings; they take effect at the next connect. */
  options: {
    blockAds: saved.get("blockAds") === "1",
    killSwitch: saved.get("killSwitch") === "1",
    bypassDomains: savedList("bypassDomains"),
    bypassApps: savedList("bypassApps"),
  } as ConnectOptions,
  /** The options the current connection was made with, to offer a reconnect after changes. */
  appliedOptions: "",
  skipError: "",
  /** The id of the panel message this person closed. */
  closedAnnouncement: Number(saved.get("closedAnnouncement") || 0),
  /** Announcements closed here. */
  closedAnnouncements: new Set<number>(savedList("closedAnnouncements").map(Number)),
  /** Announcements a notification was already shown for. */
  notifiedAnnouncements: new Set<number>(savedList("notifiedAnnouncements").map(Number)),
  shownAnnouncements: "",
  /** Messages for this person that they closed here (the server is told too). */
  closedMessages: new Set<number>(),
  /** Messages a notification was already shown for. */
  notifiedMessages: new Set<number>(savedList("notifiedMessages").map(Number)),
  /** Which messages are on screen, to redraw them only when they change. */
  shownMessages: "",
  speedTest: { phase: "" as "" | "down" | "up", down: null as number | null, up: null as number | null, error: "" },
  /** "auto" follows the computer's language. */
  lang: saved.get("lang") || "auto",
  settingsTab: (saved.get("settingsTab") as SettingsTab) || "general",
  /** Connect by itself on Wi-Fi networks that aren't trusted. */
  autoWifi: saved.get("autoWifi") === "1",
  trustedWifi: savedList("trustedWifi"),
  /** The network this computer is on, read for the setting above. */
  network: null as { onWifi: boolean; name: string | null } | null,
  /** A network where the person turned the VPN off: not turned on again by itself there. */
  declinedNetwork: "",
  /** Connect again by itself when the connection drops. */
  autoReconnect: saved.get("autoReconnect") !== "0",
  /** Closing the window keeps CakeVPN in the tray, or quits it. */
  closeToTray: saved.get("closeToTray") !== "0",
  /** The keyboard shortcut that turns the VPN on and off; "" for none. */
  shortcut: saved.get("shortcut") ?? DEFAULT_SHORTCUT,
  recordingShortcut: false,
  shortcutError: "",
  notifyConnection: saved.get("notifyConnection") !== "0",
  notifyUpdates: saved.get("notifyUpdates") !== "0",
  notifyMessages: saved.get("notifyMessages") !== "0",
  /** Until when a change of the tunnel was asked for by the person or by CakeVPN itself, so it isn't taken for a drop. */
  expectedUntil: 0,
  /** The connection dropped by itself, and CakeVPN is bringing it back. */
  dropped: false,
  lastConnectedId: "",
  /** When CakeVPN reconnected by itself lately, to stop after a few tries. */
  reconnects: [] as number[],
  /** The update version a notification was shown for. */
  notifiedUpdate: "",
  /** The last 30 days for the usage graph; null until Settings asked for it. */
  history: null as DayUsage[] | null,
  historyError: "",
  historyList: false,
  /** When things were last checked (ms), for the one ticker that paces everything. */
  last: { overview: 0, pings: 0, update: 0, network: 0 },
  /** When the account was last asked for, to ask less often while offline. */
  lastRefresh: 0,
  /** What is wrong with the connected location right now, and since when (ms). */
  trouble: { kind: "" as Trouble, since: 0 },
  /** The fastest checks of this connection so far, to see a sudden slowdown against. */
  baseline: { key: "", tunnel: 0, direct: 0 },
  /** The invite code whose Delete button was pressed once and now asks to confirm. */
  confirmDelete: "",
  /** When CakeVPN last moved away from a location by itself. */
  movedAt: 0,
};

const app = document.getElementById("app")!;
const isWindows = navigator.userAgent.includes("Windows");
/** A phone: no tray, no start at login, no keyboard shortcut; updates install through the browser. */
const isAndroid = navigator.userAgent.includes("Android");
/** How the settings speak of this device. */
const THIS_DEVICE = isAndroid ? "this phone" : "this computer";
document.documentElement.classList.toggle("phone", isAndroid);

// ---------- helpers ----------

function esc(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

function $(selector: string): HTMLElement | null {
  return app.querySelector(selector);
}

function setText(selector: string, text: string) {
  const el = $(selector);
  if (el && el.textContent !== text) el.textContent = text;
}

function flag(country: string): string {
  const cc = (country || "").toUpperCase();
  if (!/^[A-Z]{2}$/.test(cc)) return `<span class="cc">🌐</span>`;
  // Windows has no flag emoji, so it gets the two letters instead.
  if (isWindows) return `<span class="cc">${cc}</span>`;
  const emoji = String.fromCodePoint(...[...cc].map((ch) => 0x1f1e6 + ch.charCodeAt(0) - 65));
  return `<span class="flag">${emoji}</span>`;
}

function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (bytes >= 1000 && i < units.length - 1) {
    bytes /= 1000;
    i++;
  }
  return `${bytes.toFixed(i === 0 ? 0 : bytes < 10 ? 2 : 1)} ${units[i]}`;
}

function formatRate(bytesPerSecond: number): string {
  const mbps = (bytesPerSecond * 8) / 1e6;
  return mbps >= 10 ? `${mbps.toFixed(0)} Mbps` : `${mbps.toFixed(1)} Mbps`;
}

// "Speed now" glides from one reading to the next instead of jumping. The
// browser only runs these frames while the window can be seen.
const shownSpeed = { up: 0, down: 0 };
let speedFrom = { up: 0, down: 0 };
let speedTo = { up: 0, down: 0 };
let speedStart = 0;
let speedMoving = false;

function drawSpeed() {
  const el = $("#speed");
  if (!el || !windowInFront()) return;
  const html = tunnelState() === "connected" ? `↓ ${formatRate(shownSpeed.down)}<br>↑ ${formatRate(shownSpeed.up)}` : "—";
  if (el.innerHTML !== html) el.innerHTML = html;
}

function speedStep(now: number) {
  const t = Math.min(1, (now - speedStart) / 800);
  const eased = 1 - Math.pow(1 - t, 3);
  shownSpeed.up = speedFrom.up + (speedTo.up - speedFrom.up) * eased;
  shownSpeed.down = speedFrom.down + (speedTo.down - speedFrom.down) * eased;
  drawSpeed();
  if (t < 1) requestAnimationFrame(speedStep);
  else speedMoving = false;
}

/**
 * Shows a new speed reading, gliding to it. Only while the window is in
 * front: covered by another app or in the tray, the numbers stay as they are
 * and nothing is drawn.
 */
function showSpeed(target: { up: number; down: number }) {
  if (!windowInFront() || state.screen !== "home") {
    Object.assign(shownSpeed, target);
    speedMoving = false;
    return;
  }
  speedFrom = { ...shownSpeed };
  speedTo = { ...target };
  speedStart = performance.now();
  if (!speedMoving) {
    speedMoving = true;
    requestAnimationFrame(speedStep);
  }
}

function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

function planLabel(account: Account): string {
  const p = account.plan;
  // speedMbps includes the invite bonus; older servers leave it out.
  const mbps = account.speedMbps || p.mbps;
  return p.mbps > 0 ? `${p.name} · ${mbps} Mbps` : `${p.name} · Max speed`;
}

/** A small "i" that shows its text on hover or tap. */
function infoTip(text: string): string {
  return `<span class="info" tabindex="0" role="note" aria-label="${esc(text)}" data-tip="${esc(text)}">i</span>`;
}

function moreSpeedTip(account: Account): string {
  const r = account.referral;
  return `More speed: every friend who joins with your invite code adds +${r.mbpsPerFriend} Mbps to your plan, for up to ${r.maxFriends} friends.`;
}

function invitesOn(account: Account | null): boolean {
  return !!account?.referral && account.referral.maxFriends > 0;
}

/** Lower is better: the closest location wins. One that wasn't measured comes after those that were. */
function score(loc: Location): number {
  return state.pings[loc.id] ?? 100_000;
}

/** From this load (in percent) on, the server calls a location's load high. */
const OVERLOAD_PERCENT = 85;

function overloaded(loc: Location | undefined): boolean {
  return (loc?.load?.percent ?? 0) >= OVERLOAD_PERCENT;
}

/**
 * The best location is the closest one: the lowest ping. Load doesn't
 * count; a busy location is only left when it actually gets slow (see
 * leaveTroubledLocation). Sorting keeps the list's order for equal or
 * unmeasured pings.
 */
function bestLocation(): Location | undefined {
  return (state.account?.locations ?? []).filter((l) => l.online).sort((a, b) => score(a) - score(b))[0];
}

/**
 * Where to go when `from` is in trouble: the closest other location that
 * answers pings. After a slowdown from high load, not to another busy one.
 */
function otherLocation(from: Location, kind: Trouble): Location | undefined {
  return (state.account?.locations ?? [])
    .filter((l) => l.online && l.id !== from.id && state.pings[l.id] != null)
    .filter((l) => kind !== "slow" || !overloaded(l))
    .sort((a, b) => score(a) - score(b))[0];
}

/** The location on the home screen: the one the VPN is connected to, otherwise the chosen one. */
function shownLocation(): Location | undefined {
  const tstate = tunnelState();
  if (tstate === "connected" || tstate === "connecting") {
    const connected = (state.account?.locations ?? []).find((l) => l.id === state.overview?.locationId);
    if (connected) return connected;
  }
  return chosenLocation();
}

/** Moves the saved choice along when CakeVPN picked a location for someone. */
function followMove(to: Location) {
  if (state.choice !== "best") {
    state.choice = to.id;
    saved.set("location", to.id);
  }
}

function moveMessage(from: Location, to: Location, kind: Trouble): string {
  return kind === "down"
    ? `${from.name} stopped responding, so CakeVPN moved you to ${to.name}.`
    : `${from.name} slowed down from high load, so CakeVPN moved you to ${to.name}.`;
}

/** The note under the button goes away by itself after a while, or with its ×. */
const MOVE_NOTICE_FOR = 30_000;

function showMoveNotice(text: string) {
  state.moveNotice = text;
  state.moveNoticeUntil = Date.now() + MOVE_NOTICE_FOR;
}

function chosenLocation(): Location | undefined {
  const all = state.account?.locations ?? [];
  if (state.choice !== "best") {
    const picked = all.find((l) => l.id === state.choice && l.online);
    if (picked) return picked;
  }
  return bestLocation();
}

function tunnelState() {
  return state.overview?.status?.state ?? "disconnected";
}

function applyTheme() {
  if (state.theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = state.theme;
}

// ---------- rendering ----------
//
// render() builds a screen from scratch; it runs when the screen or its
// layout changes. updateHome() only touches the parts that change every
// couple of seconds, so animations keep running instead of restarting.

function render() {
  app.dataset.screen = state.screen;
  if (state.screen === "loading") {
    app.innerHTML = `<div class="center screen"><div class="logo big">🍰</div><div class="muted">Loading…</div></div>`;
  } else if (state.screen === "code") {
    renderCode();
  } else if (state.screen === "setup") {
    renderSetup();
  } else if (state.screen === "settings") {
    renderSettings();
  } else if (state.screen === "forced") {
    renderForced();
  } else {
    renderHome();
  }
}

function header(right = ""): string {
  return `<header><div class="brand"><span class="logo">🍰</span> CakeVPN</div><div class="head-right">${right}</div></header>`;
}

function renderCode() {
  const locked = state.lockedUntil > Date.now();
  app.innerHTML = `
    ${header()}
    <main class="code-screen screen">
      <h1>Enter your code</h1>
      <p class="muted">Type the 5-character code you were given.</p>
      ${state.codeNotice ? `<div class="notice">${esc(state.codeNotice)}</div>` : ""}
      <form id="code-form" autocomplete="off">
        <div class="boxes">
          ${[0, 1, 2, 3, 4].map((i) => `<input class="box" data-i="${i}" maxlength="1" inputmode="text" ${locked || state.busy ? "disabled" : ""}>`).join("")}
        </div>
        <div class="error ${locked || state.codeError ? "" : "hidden"}" id="code-error"></div>
        <button class="primary" type="submit" ${locked || state.busy ? "disabled" : ""}>${state.busy ? "Checking…" : "Sign in"}</button>
      </form>
    </main>`;
  updateCodeMessage();
  const boxes = [...app.querySelectorAll<HTMLInputElement>(".box")];
  if (!locked) boxes[0]?.focus();
  boxes.forEach((box, i) => {
    box.addEventListener("input", () => {
      box.value = box.value.toUpperCase().replace(/[^A-Z0-9]/g, "");
      if (box.value && i < 4) boxes[i + 1].focus();
      if (boxes.every((b) => b.value)) submitCode(boxes.map((b) => b.value).join(""));
    });
    box.addEventListener("keydown", (e) => {
      if (e.key === "Backspace" && !box.value && i > 0) boxes[i - 1].focus();
    });
    box.addEventListener("paste", (e) => {
      const text = (e.clipboardData?.getData("text") ?? "").toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 5);
      if (!text) return;
      e.preventDefault();
      text.split("").forEach((ch, j) => (boxes[j].value = ch));
      if (text.length === 5) submitCode(text);
      else boxes[text.length].focus();
    });
  });
  $("#code-form")!.addEventListener("submit", (e) => {
    e.preventDefault();
    const code = boxes.map((b) => b.value).join("");
    if (code.length === 5) submitCode(code);
  });
}

function updateCodeMessage() {
  const locked = state.lockedUntil > Date.now();
  const wait = Math.ceil((state.lockedUntil - Date.now()) / 1000);
  setText("#code-error", locked ? `Too many wrong codes. Try again in ${formatDuration(wait)}.` : state.codeError);
}

/** The panel requires a newer CakeVPN: a screen that updates it. */
function enterForcedUpdate(version: string) {
  state.forcedVersion = version || state.forcedVersion;
  state.update = { version: state.forcedVersion };
  if (state.screen !== "forced") setScreen("forced");
  // A computer updates by itself; Android only installs what the person taps.
  if (!isAndroid && !state.updating && !state.forcedTried) {
    state.forcedTried = true;
    void installUpdate();
  }
}

function renderForced() {
  const v = state.forcedVersion;
  app.innerHTML = `
    ${header()}
    <main class="center setup screen">
      <div class="logo big">⬆️</div>
      <h1>Update required</h1>
      <p class="muted">${
        isAndroid
          ? `This version of CakeVPN is too old. Download version ${esc(v)} and install it to keep using CakeVPN.`
          : `This version of CakeVPN is too old. It is updating itself to version ${esc(v)}.`
      }</p>
      ${state.updating ? `<div class="progress-track"><i id="forced-fill" style="width:${Math.round((state.updateProgress ?? 0) * 100)}%"></i></div>` : ""}
      ${state.updateMessage ? `<p class="muted small">${esc(state.updateMessage)}</p>` : ""}
      <button class="primary" id="forced-update" ${state.updating ? "disabled" : ""}>${
        state.updating ? esc(updatingText()) : isAndroid ? "Download the update" : "Update now"
      }</button>
    </main>`;
  $("#forced-update")?.addEventListener("click", () => void installUpdate());
}

/** What is wrong with the Windows service, in words (see helper_problem). */
function windowsProblemText(): string {
  switch (state.helperProblem?.kind) {
    case "file_missing":
      return "A CakeVPN file is missing. Windows Security may have removed it: open Windows Security, then Virus & threat protection, then Protection history, allow CakeVPN there, and install CakeVPN again.";
    case "not_installed":
      return "CakeVPN's background service isn't installed. Press Fix it; Windows will ask for permission once.";
    case "stopped":
      return "CakeVPN's background service is stopped. Press Fix it to start it; Windows will ask for permission once.";
    case "starting":
      return "CakeVPN's background service is starting…";
    case "elsewhere":
      return "CakeVPN's background service belongs to another copy of CakeVPN. Press Fix it to use this one; Windows will ask for permission once.";
    case "outdated":
      return "CakeVPN was updated and its background service needs a restart. Press Fix it; Windows will ask for permission once.";
    case "not_answering":
      return "CakeVPN's background service is running but not answering. Press Fix it to restart it; Windows will ask for permission once.";
    case "crashed":
      return "CakeVPN's background service stopped unexpectedly. Press Fix it to start it again. If it keeps happening, send us the line below.";
    default:
      return "CakeVPN's background service isn't running. Fix it here; Windows will ask for permission once.";
  }
}

/** Asks Windows what is wrong with the service, and shows it. */
async function readHelperProblem() {
  if (!isWindows) return;
  try {
    state.helperProblem = await backend.helperProblem();
  } catch {
    state.helperProblem = null;
  }
  if (state.screen === "setup") render();
}

let problemAsked = false;

function renderSetup() {
  if (isWindows && !problemAsked) {
    problemAsked = true;
    void readHelperProblem();
  }
  const outdated = state.overview?.helper === "outdated";
  app.innerHTML = `
    ${header()}
    <main class="center setup screen">
      <div class="logo big">🔧</div>
      <h1>${isWindows ? "One quick fix" : outdated ? "Update needed" : "One-time setup"}</h1>
      <p class="muted">${
        isWindows
          ? windowsProblemText()
          : outdated
            ? "CakeVPN was updated and its network helper needs updating too."
            : "CakeVPN needs to install a small network helper. Your Mac will ask for your password once."
      }</p>
      ${isWindows && state.helperProblem?.detail ? `<p class="muted small" data-keep>${esc(state.helperProblem.detail)}</p>` : ""}
      ${state.actionError ? `<div class="error">${esc(state.actionError)}</div>` : ""}
      <button class="primary" id="install" ${state.busy ? "disabled" : ""}>${
        isWindows ? (state.busy ? "Fixing…" : "Fix it") : state.busy ? "Setting up…" : "Set up"
      }</button>
    </main>`;
  $("#install")?.addEventListener("click", installHelper);
}

const ENVELOPE = `<svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round"><rect x="3" y="5.5" width="18" height="13" rx="2.5"/><path d="M4 7.5l8 6 8-6" stroke-linecap="round"/></svg>`;

// Eight even teeth around the middle, so it stands straight.
const GEAR = `<svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path fill="currentColor" fill-rule="evenodd" d="M9.62 4.68 L10.09 1.98 L13.91 1.98 L14.38 4.68 L15.50 5.14 L17.73 3.56 L20.44 6.27 L18.86 8.50 L19.32 9.62 L22.02 10.09 L22.02 13.91 L19.32 14.38 L18.86 15.50 L20.44 17.73 L17.73 20.44 L15.50 18.86 L14.38 19.32 L13.91 22.02 L10.09 22.02 L9.62 19.32 L8.50 18.86 L6.27 20.44 L3.56 17.73 L5.14 15.50 L4.68 14.38 L1.98 13.91 L1.98 10.09 L4.68 9.62 L5.14 8.50 L3.56 6.27 L6.27 3.56 L8.50 5.14 Z M15.2 12 A3.2 3.2 0 1 0 8.8 12 A3.2 3.2 0 1 0 15.2 12 Z"/></svg>`;

/** How busy a location is: a small bar and the percentage. */
function loadBadge(l: Location): string {
  if (!l.load) return "";
  return `<span class="load ${l.load.level}" title="How busy this location's server is, from everyone using it">
      <span class="load-bar"><i style="width:${Math.max(4, l.load.percent)}%"></i></span>
      <span class="load-pct">${l.load.percent}%</span>
    </span>`;
}

/** "18 ms", or "" when the location wasn't measured. */
function pingText(l: Location): string {
  const ping = state.pings[l.id];
  return ping != null ? `${ping} ms` : "";
}

function locationRow(l: Location, selected: boolean, best = false): string {
  const city = placeOf(l)?.city;
  const live = !best && tunnelState() === "connected" && state.overview?.locationId === l.id;
  return `
    <button class="loc ${selected ? "selected" : ""} ${live ? "live" : ""}" data-loc="${best ? "best" : esc(l.id)}" ${l.online ? "" : "disabled"}>
      ${flag(l.country)}
      <span class="loc-name">${esc(best ? "Best location" : l.name)}<small>${
        live ? `<span class="live-note">Connected</span>` : esc(best ? l.name : (city ?? ""))
      }</small></span>
      <span class="loc-meta">${loadBadge(l)}<span class="ping">${pingText(l)}</span></span>
    </button>`;
}

/** The list under the map: "Best location", then every location. */
function locationsHtml(): string {
  const best = bestLocation();
  return `${best ? locationRow(best, state.choice === "best", true) : ""}
    ${(state.account?.locations ?? []).map((l) => locationRow(l, state.choice === l.id)).join("")}`;
}

/**
 * The pins on the map, one per location that has a known place. The one the
 * VPN is connected to pulses; the chosen one is filled. With a few locations
 * every pin is named; with many, only the chosen one, so names don't pile up.
 * The map puts each pin at its place and keeps it the same size at any zoom.
 */
function pinsHtml(): string {
  const locations = state.account?.locations ?? [];
  const shown = shownLocation();
  const connected = tunnelState() === "connected" ? state.overview?.locationId : null;
  const nameAll = locations.length <= 4;
  // The chosen location is drawn last, so its pin and name are on top of its neighbors.
  return [...locations]
    .sort((a, b) => Number(a.id === shown?.id) - Number(b.id === shown?.id))
    .map((l) => {
      const place = placeOf(l);
      if (!place) return "";
      const isShown = l.id === shown?.id;
      const ping = pingText(l);
      const classes = ["pin", isShown ? "chosen" : "", l.id === connected ? "connected" : "", l.online ? "" : "offline"].join(" ");
      // The country, then the city and the ping under it.
      const where = [place.city, ping].filter(Boolean).join(" · ");
      const label =
        nameAll || isShown
          ? `<text class="pin-label" text-anchor="start">
              <tspan x="12" y="-1">${esc(l.name)}</tspan>
              <tspan class="pin-where" x="12" y="13">${esc(where)}</tspan>
            </text>`
          : "";
      const note = `${l.name} (${place.city})${ping ? ` · ${ping}` : ""}${l.load ? ` · ${t("Load {n}%", { n: l.load.percent })}` : ""}${l.online ? "" : ` · ${t("offline")}`}`;
      return `<g class="${classes}" data-loc="${esc(l.id)}" data-x="${place.x.toFixed(2)}" data-y="${place.y.toFixed(2)}">
        <title>${esc(note)}</title>
        <circle class="pin-pulse" r="7"></circle>
        <circle class="pin-hit" r="15"></circle>
        <circle class="pin-dot" r="5"></circle>
        ${label}
      </g>`;
    })
    .join("");
}

function renderHome() {
  const account = state.account!;
  app.innerHTML = `
    ${header(`<span class="plan ${account.plan.id}">${esc(planLabel(account))}</span>
      <button class="icon-btn inbox-btn" id="open-inbox" title="Messages">${ENVELOPE}<span class="inbox-count hidden" id="inbox-count"></span></button>
      <button class="icon-btn" id="open-settings" title="Settings">${GEAR}</button>`)}
    <main class="home screen">
      <div class="announcements" id="announcements"></div>
      <div class="personal-messages" id="personal-messages"></div>
      <div class="update-bar" id="update-bar">
        <span id="update-text"></span>
        <button id="update-now">Update now</button>
        <div class="progress-track" id="update-track"><i id="update-fill"></i></div>
      </div>
      <div class="home-grid">
        <section class="side">
          <button id="power" class="power">
            <svg class="ring" viewBox="0 0 120 120" aria-hidden="true">
              <circle class="ring-track" cx="60" cy="60" r="54"></circle>
              <circle class="ring-arc" cx="60" cy="60" r="54"></circle>
            </svg>
            <svg class="power-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3.5v8"/><path d="M6.6 6.9a7.6 7.6 0 1 0 10.8 0"/></svg>
            <span class="power-text" id="power-text"></span>
          </button>
          <div class="status-line" id="status-line"></div>
          <div class="error hidden" id="action-error"></div>
          <div class="notice move hidden" id="move-notice"><span id="move-text"></span><button class="notice-close" id="move-close" aria-label="Close">×</button></div>
          <div class="notice hidden" id="offline-notice">CakeVPN's sign-in server can't be reached on this network. You can still connect with your saved details.</div>
          <div class="banner" id="banner"><span class="banner-icon"></span><span class="banner-text"></span></div>

          <div class="card stats">
            <div><div class="card-label">Used this month</div><div class="big-number" id="usage"></div></div>
            <div><div class="card-label">Speed now</div><div class="big-number small" id="speed"></div></div>
          </div>
          <div class="speedtest hidden" id="speedtest">
            <button class="pill" id="run-speedtest">Run a speed test</button>
            <div class="progress-track hidden" id="speedtest-track"><i id="speedtest-fill"></i></div>
            <div class="speedtest-result" id="speedtest-result"></div>
          </div>
          <div class="muted small center-text" id="protection-line"></div>
          <div class="muted small center-text">Unlimited traffic on every plan</div>
          ${
            invitesOn(account) && account.plan.mbps > 0
              ? `<div class="invite-line">
                  <button class="link invite-link" id="invite-link">🎁 Invite friends · +${account.referral.mbpsPerFriend} Mbps each</button>
                  ${infoTip(moreSpeedTip(account))}
                </div>`
              : ""
          }
        </section>

        <section class="stage">
          <div class="card map-card">
            ${mapHtml(pinsHtml())}
            <div class="your-ip" id="your-ip"></div>
          </div>
          <div class="card locations" id="locations">${locationsHtml()}</div>
        </section>
      </div>
    </main>`;

  $("#power")!.addEventListener("click", togglePower);
  $("#move-close")!.addEventListener("click", () => {
    state.moveNotice = "";
    updateHome();
  });
  $("#open-settings")!.addEventListener("click", () => openSettings());
  $("#open-inbox")!.addEventListener("click", () => {
    state.settingsTab = "messages";
    openSettings();
  });
  $("#invite-link")?.addEventListener("click", () => openSettings(true));
  $("#update-now")!.addEventListener("click", installUpdate);
  $("#announcements")!.addEventListener("click", (e) => {
    const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".announce-close");
    if (button) closeAnnouncement(Number(button.dataset.id));
  });
  $("#personal-messages")!.addEventListener("click", (e) => {
    const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".message-close");
    if (button) closeMessage(Number(button.dataset.id));
  });
  $("#run-speedtest")!.addEventListener("click", runSpeedTest);
  updateLocations();
  $("#locations")!.addEventListener("click", (e) => {
    const row = (e.target as HTMLElement).closest<HTMLButtonElement>(".loc");
    if (row && !row.disabled) chooseLocation(row.dataset.loc!);
  });
  // A pin on the map picks its location, like its row in the list.
  attachMap(chooseLocation);
  updateHome();
}

/** What the map and list last showed, so they are redrawn when the VPN's state or the choice changes. */
let shownPlaces = "";
/** The location the map last moved to: null for the whole world, undefined before its first look. */
let mapFollows: string | null | undefined;

/**
 * The map goes to the location the VPN connects to, and back to the whole
 * world when the VPN is turned off. While the VPN is still connecting, or
 * moving to another location, it stays where it is.
 */
function followConnection() {
  const tstate = tunnelState();
  if (tstate === "connecting") return;
  const to = tstate === "connected" ? (state.overview?.locationId ?? null) : null;
  if (to === mapFollows) return;
  const first = mapFollows === undefined;
  mapFollows = to;
  const location = to ? state.account?.locations.find((l) => l.id === to) : undefined;
  const place = location ? placeOf(location) : null;
  if (place) showPlace(place, { glide: !first });
  else if (!first) showPlace(null, { onlyIfAutomatic: true });
}

/** Updates the home screen in place. */
function updateHome() {
  if (state.screen !== "home" || !state.account) return;
  const places = `${tunnelState()}|${state.overview?.locationId}|${state.choice}`;
  if (places !== shownPlaces) {
    shownPlaces = places;
    updateLocations();
  }
  followConnection();
  const status = state.overview?.status;
  const tstate = tunnelState();
  const power = $("#power") as HTMLButtonElement | null;
  if (!power) return;
  power.className = `power ${tstate}`;
  power.disabled = state.busy;
  setText("#power-text", { disconnected: "Connect", connecting: "Connecting…", connected: "Connected", failed: "Try again" }[tstate]);

  const since = status?.connectedSince ? Date.now() / 1000 - status.connectedSince : 0;
  const line = $("#status-line")!;
  line.className = `status-line ${tstate}`;
  setText(
    "#status-line",
    tstate === "connected"
      ? `Protected · ${formatDuration(since)}`
      : tstate === "connecting"
        ? // With the kill switch on, the helper says why it is still trying.
          status?.error || "Setting up a secure connection…"
        : tstate === "failed"
          ? status?.error || "Could not connect."
          : "Not connected",
  );

  const error = $("#action-error")!;
  error.classList.toggle("hidden", !state.actionError);
  setText("#action-error", state.actionError);
  if (state.moveNotice && Date.now() > state.moveNoticeUntil) state.moveNotice = "";
  const moved = $("#move-notice")!;
  moved.classList.toggle("hidden", !state.moveNotice);
  setText("#move-text", state.moveNotice);
  // Once connected the server is reached through the VPN, so the note goes away.
  $("#offline-notice")!.classList.toggle("hidden", !state.offline || tstate === "connected" || tstate === "connecting");

  const banner = $("#banner")!;
  const b = state.overview?.banner;
  banner.className = `banner ${b ? `show ${b.kind}` : ""}`;
  if (b) {
    setText("#banner .banner-icon", { wifi: "📶", internet: "🌐", load: "🔥", vpn: "🛠️", dns: "🔎" }[b.kind] ?? "⚠️");
    setText("#banner .banner-text", b.message);
  }

  drawAnnouncements();

  drawMessages();
  const unread = openMessages().length + openAnnouncements().length;
  const count = $("#inbox-count");
  if (count) {
    count.classList.toggle("hidden", unread === 0);
    setText("#inbox-count", unread > 9 ? "9+" : String(unread));
  }

  // The speed test runs against the location this computer is connected to.
  const test = state.speedTest;
  const connectedTo = state.account.locations.find((l) => l.id === state.overview?.locationId);
  const canTest = tstate === "connected" && !!connectedTo?.speedTest;
  $("#speedtest")!.classList.toggle("hidden", !canTest && !test.phase);
  $("#speedtest-track")!.classList.toggle("hidden", !test.phase);
  const run = $("#run-speedtest") as HTMLButtonElement;
  run.disabled = !!test.phase;
  setText("#run-speedtest", test.phase === "down" ? "Testing download…" : test.phase === "up" ? "Testing upload…" : "Run a speed test");
  setText(
    "#speedtest-result",
    test.error
      ? test.error
      : test.down !== null
        ? `↓ ${formatMbps(test.down)}${test.up !== null ? ` · ↑ ${formatMbps(test.up)}` : ""}`
        : "",
  );
  $("#speedtest-result")!.classList.toggle("error-text", !!test.error);

  setText("#protection-line", tstate === "connected" ? protectionText() : "");

  const bar = $("#update-bar")!;
  bar.classList.toggle("show", !!state.update);
  if (state.update) {
    setText("#update-text", state.updating ? updatingText() : `CakeVPN ${state.update.version} is ready`);
    $("#update-now")!.classList.toggle("hidden", state.updating);
    bar.classList.toggle("updating", state.updating);
    ($("#update-fill") as HTMLElement).style.width = `${Math.round((state.updateProgress ?? 0) * 100)}%`;
  }

  setText("#usage", formatBytes(state.account.usage.bytes));
  drawSpeed();
}

/** "IPv4 1.2.3.4 · IPv6 2603:…" for the main location, or "" when unknown. */
function ipsText(account: Account | null): string {
  const ips = account?.ips;
  if (!ips) return "";
  return [ips.ipv4 && `IPv4 ${ips.ipv4}`, ips.ipv6 && `IPv6 ${ips.ipv6}`].filter(Boolean).join(" · ");
}

/** Redraws the location parts after the account, pings or choice changed. */
function updateLocations() {
  syncTray();
  if (state.screen !== "home") return;
  const list = $("#locations");
  const pins = $("#map-pins");
  if (list) list.innerHTML = locationsHtml();
  if (pins) {
    pins.innerHTML = pinsHtml();
    setPlaces((state.account?.locations ?? []).flatMap((l) => placeOf(l) ?? []));
    const connected = tunnelState() === "connected" ? state.account?.locations.find((l) => l.id === state.overview?.locationId) : undefined;
    markCountries(shownLocation()?.country, connected?.country);
    drawMap();
  }
  // The addresses belong to the main location; other locations use their own.
  const main = shownLocation()?.id === "main";
  setText("#your-ip", main ? ipsText(state.account) : "");
}

/** Small line icons for the Settings tabs. */
const TAB_ICONS: Record<SettingsTab, string> = {
  general: '<path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/>',
  connection: '<path d="M5 12.5a10 10 0 0 1 14 0M8 15.5a6 6 0 0 1 8 0"/><circle cx="12" cy="19" r="1.2"/><path d="M2 9.5a14 14 0 0 1 20 0"/>',
  notifications: '<path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15z"/><path d="M10 20a2 2 0 0 0 4 0"/>',
  protection: '<path d="M12 3l7 3v5c0 5-3 8-7 10-4-2-7-5-7-10V6z"/><path d="M9 12l2 2 4-4"/>',
  skip: '<path d="M4 12h9M13 12l-4-4M13 12l-4 4"/><path d="M17 5v14"/>',
  account: '<circle cx="12" cy="8" r="4"/><path d="M4 20c1.5-4 4.5-6 8-6s6.5 2 8 6"/>',
  invites: '<rect x="4" y="9" width="16" height="11" rx="2"/><path d="M12 9v11M4 13h16M12 9c-2-4-6-3-5 0M12 9c2-4 6-3 5 0"/>',
  about: '<circle cx="12" cy="12" r="9"/><path d="M12 8v5M12 16v.5"/>',
  messages: '<rect x="3" y="5.5" width="18" height="13" rx="2.5"/><path d="M4 7.5l8 6 8-6"/>',
};

function settingsTabs(): { id: SettingsTab; label: string }[] {
  const tabs: { id: SettingsTab; label: string }[] = [
    { id: "general", label: "General" },
    { id: "connection", label: "Connection" },
    ...(isAndroid ? [] : [{ id: "notifications" as SettingsTab, label: "Notifications" }]),
    { id: "protection", label: "Protection" },
    { id: "skip", label: "Skip the VPN" },
  ];
  if (state.account) tabs.push({ id: "account", label: "Account" }, { id: "messages", label: "Messages" });
  if (invitesOn(state.account)) tabs.push({ id: "invites", label: "Invite friends" });
  tabs.push({ id: "about", label: "Updates & about" });
  return tabs;
}

function switchRow(id: string, title: string, note: string, on: boolean): string {
  return `<label class="row">
      <span><b>${title}</b><small>${note}</small></span>
      <input type="checkbox" class="switch" id="${id}" ${on ? "checked" : ""}>
    </label>`;
}

/** "⌃⌥V" on a Mac, "Ctrl+Alt+Shift+V" on Windows. */
function shortcutLabel(accelerator: string): string {
  if (!accelerator) return "Off";
  const parts = accelerator.split("+");
  if (isMac) {
    const mac: Record<string, string> = { Control: "⌃", Alt: "⌥", Shift: "⇧", Super: "⌘" };
    return parts.map((p) => mac[p] ?? p).join("");
  }
  const win: Record<string, string> = { Control: "Ctrl", Super: "Win" };
  return parts.map((p) => win[p] ?? p).join("+");
}

/** The shortcut for a key press, or "" when it isn't one: it needs a modifier and a letter, digit or F-key. */
function acceleratorOf(e: KeyboardEvent): string {
  const key = /^Key([A-Z])$/.exec(e.code)?.[1] ?? /^Digit([0-9])$/.exec(e.code)?.[1] ?? (/^F([1-9]|1[0-2])$/.test(e.code) ? e.code : "");
  const mods = [e.ctrlKey && "Control", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean) as string[];
  if (!key || !mods.length || (mods.length === 1 && mods[0] === "Shift")) return "";
  return [...mods, key].join("+");
}

function generalTab(): string {
  const themeButton = (value: Theme, label: string) =>
    `<button class="seg ${state.theme === value ? "active" : ""}" data-theme="${value}">${label}</button>`;
  return `
    <div class="card list">
      <div class="row">
        <span><b>Language</b><small>The language CakeVPN is shown in</small></span>
        <select id="set-lang" class="select" data-keep>
          <option value="auto" ${state.lang === "auto" ? "selected" : ""}>${esc(t(`Same as ${THIS_DEVICE}`))}</option>
          ${LANGUAGES.map((l) => `<option value="${l.code}" ${state.lang === l.code ? "selected" : ""}>${esc(l.name)}</option>`).join("")}
        </select>
      </div>
      <div class="row">
        <span><b>Appearance</b><small>Light, dark, or the same as ${THIS_DEVICE}</small></span>
        <div class="segmented small" id="theme">${themeButton("system", "Automatic")}${themeButton("light", "Light")}${themeButton("dark", "Dark")}</div>
      </div>
    </div>
    ${isAndroid ? "" : computerCard()}
    ${state.settingsError ? `<div class="error">${esc(state.settingsError)}</div>` : ""}`;
}

/** Start at login, the close button and the keyboard shortcut: things only a computer has. */
function computerCard(): string {
  const closeButton = (value: boolean, label: string) =>
    `<button class="seg ${state.closeToTray === value ? "active" : ""}" data-close="${value ? "tray" : "quit"}">${label}</button>`;
  return `
    <div class="card list">
      ${switchRow("set-autostart", "Open at startup", "Start CakeVPN in the tray when your computer starts", state.autostart)}
      <div class="row">
        <span><b>Closing the window</b><small>What the window's close button does</small></span>
        <div class="segmented small" id="close">${closeButton(true, "Keep in the tray")}${closeButton(false, "Quit CakeVPN")}</div>
      </div>
      <div class="row">
        <span><b>Keyboard shortcut</b><small>${
          state.recordingShortcut ? "Press the keys you want, like Ctrl + Alt + V. Esc to cancel." : "Turns the VPN on and off from any app"
        }</small></span>
        <span class="shortcut">
          <kbd class="${state.recordingShortcut ? "recording" : ""}" data-keep>${state.recordingShortcut ? "…" : esc(shortcutLabel(state.shortcut))}</kbd>
          ${
            state.recordingShortcut
              ? ""
              : `<button class="outline small-btn" id="shortcut-change">${state.shortcut ? "Change" : "Set"}</button>
                 ${state.shortcut ? `<button class="link" id="shortcut-off">Turn off</button>` : ""}`
          }
        </span>
      </div>
      ${state.shortcutError ? `<div class="error">${esc(state.shortcutError)}</div>` : ""}
    </div>`;
}

function connectionTab(): string {
  const net = state.network;
  const here = net?.onWifi && net.name ? net.name : "";
  const trusted = state.trustedWifi;
  return `
    <div class="card list">
      ${switchRow("set-autoconnect", "Connect when CakeVPN opens", "Turns the VPN on as soon as CakeVPN starts", state.autoConnect)}
      ${switchRow("set-reconnect", "Reconnect by itself", "If the connection drops, CakeVPN connects again", state.autoReconnect)}
    </div>
    ${isAndroid ? "" : publicWifiCard(here, trusted, net)}`;
}

function publicWifiCard(here: string, trusted: string[], net: typeof state.network): string {
  return `
    <div class="card list">
      ${switchRow("set-autowifi", "Connect on public Wi-Fi", "Turns the VPN on by itself on any Wi-Fi that isn't in your trusted list, like in a café or hotel", state.autoWifi)}
      <div class="row column">
        <span><b>Trusted Wi-Fi networks</b><small>On these, CakeVPN doesn't turn on by itself</small></span>
        <div class="skip-list" data-keep>${
          trusted.length
            ? trusted
                .map((n) => `<span class="chip">📶 ${esc(n)}<button class="trust-remove" data-name="${esc(n)}" title="Remove" aria-label="Remove">×</button></span>`)
                .join("")
            : `<span class="muted small">${esc(t("None yet"))}</span>`
        }</div>
        ${
          here && !trusted.includes(here)
            ? `<button class="outline small-btn" id="trust-here">${esc(t("Trust {wifi}", { wifi: here }))}</button>`
            : net?.onWifi && !here
              ? `<span class="muted small">This Mac doesn't tell apps the Wi-Fi's name, so every Wi-Fi counts as public.</span>`
              : ""
        }
      </div>
    </div>`;
}

function notificationsTab(): string {
  return `
    <div class="card list">
      ${switchRow("set-notify-connection", "The connection", "When the VPN drops, comes back, turns on by itself on public Wi-Fi, or moves you to another location", state.notifyConnection)}
      ${switchRow("set-notify-updates", "Updates", "When a new version of CakeVPN is ready", state.notifyUpdates)}
      ${switchRow("set-notify-messages", "Messages from CakeVPN", "When CakeVPN sends you a message", state.notifyMessages)}
    </div>
    <p class="muted small">Notifications only appear while the CakeVPN window isn't in front of you.</p>
    <button class="outline" id="test-notification">Send a test notification</button>`;
}

function reconnectNotice(): string {
  return optionsChanged()
    ? `<div class="notice reconnect">Reconnect to use your new settings.
         <button id="reconnect" ${state.busy ? "disabled" : ""}>Reconnect now</button></div>`
    : "";
}

function protectionTab(): string {
  return `
    <div class="card list">
      ${switchRow("set-ads", "Block ads and trackers", `Stops known ad and tracking sites, in every app on ${THIS_DEVICE}`, state.options.blockAds)}
      ${isAndroid ? "" : switchRow("set-kill", "Kill switch", "If the VPN drops, your internet stays blocked until it reconnects or you disconnect", state.options.killSwitch)}
    </div>
    ${reconnectNotice()}`;
}

function skipTab(): string {
  return `
    <div class="card skip">
      <p class="muted small">${isAndroid ? "These websites use your normal internet instead of the VPN." : "These websites and apps use your normal internet instead of the VPN."}</p>
      ${skipListHtml("domain", state.options.bypassDomains)}
      <form class="skip-add" id="add-domain">
        <input type="text" id="new-domain" placeholder="Website, like mybank.com" autocomplete="off" spellcheck="false">
        <button class="outline small-btn">Add</button>
      </form>
      ${
        isAndroid
          ? ""
          : `${skipListHtml("app", state.options.bypassApps)}
      <form class="skip-add" id="add-app">
        <input type="text" id="new-app" placeholder="${isWindows ? "App, like steam.exe" : "App, like Steam"}" autocomplete="off" spellcheck="false">
        <button class="outline small-btn">Add</button>
      </form>`
      }
      ${state.skipError ? `<div class="error">${esc(state.skipError)}</div>` : ""}
    </div>
    ${reconnectNotice()}`;
}

function accountTab(): string {
  const account = state.account;
  if (!account) return "";
  return `
    <div class="account-grid">
      <div class="card list">
        <div class="row"><span><b>Plan</b></span><span class="value">${esc(planLabel(account))}</span></div>
        <div class="row"><span><b>Used this month</b></span><span class="value">${formatBytes(account.usage.bytes)}</span></div>
        <div class="row"><span><b>Traffic</b></span><span class="value">Unlimited</span></div>
        ${account.ips?.ipv4 ? `<div class="row"><span><b>Your IPv4</b></span><span class="value ip" data-keep>${esc(account.ips.ipv4)}</span></div>` : ""}
        <div class="row"><span><b>Your IPv6</b></span><span class="value ip" ${account.ips?.ipv6 ? "data-keep" : ""}>${
          account.ips?.ipv6 ? esc(account.ips.ipv6) : "Shared"
        }</span></div>
      </div>
      <div class="card usage-card" id="usage-card">
        <div class="card-label">Last 30 days</div>
        <div id="usage-chart">${usageChartHtml()}</div>
      </div>
    </div>
    <button class="danger-outline" id="sign-out">Sign out of this device</button>`;
}

/** Every message from CakeVPN, newest first: an inbox (announcements too). */
function messagesTab(): string {
  const account = state.account;
  const personal = (account?.inbox?.length ? account.inbox : (account?.messages ?? [])).map((m) => ({
    key: `m${m.id}`,
    id: m.id,
    everyone: false,
    text: m.text,
    kind: m.kind,
    createdAt: m.createdAt,
    open: !m.closedAt && !state.closedMessages.has(m.id),
  }));
  const everyone = allAnnouncements().map((a) => ({
    key: `a${a.id}`,
    id: a.id,
    everyone: true,
    text: a.text,
    kind: a.kind,
    createdAt: a.createdAt ?? 0,
    open: !state.closedAnnouncements.has(a.id) && a.id !== state.closedAnnouncement,
  }));
  const list = [...personal, ...everyone].sort((a, b) => b.createdAt - a.createdAt);
  if (!list.length) return `<p class="muted">No messages yet. When CakeVPN writes to you, it shows up here and as a notification.</p>`;
  const when = (seconds: number) =>
    seconds ? new Date(seconds * 1000).toLocaleString(locale(), { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "";
  return `<div class="inbox">${list
    .map(
      (m) => `<div class="card inbox-item ${m.open ? "unread" : ""} ${m.kind === "warning" ? "is-warning" : ""}">
        <div class="inbox-head">
          <span>${m.kind === "warning" ? "⚠️" : m.everyone ? "📣" : "✉️"}</span>
          ${m.everyone ? `<span class="muted small">To everyone</span>` : ""}
          <span class="muted small" data-keep>${esc(when(m.createdAt))}</span>
          ${m.open ? `<span class="inbox-new">New</span>` : ""}
        </div>
        <div class="inbox-text" data-keep>${esc(m.text)}</div>
        ${m.open ? `<button class="link inbox-read" data-id="${m.id}" data-everyone="${m.everyone ? 1 : 0}">Mark as read</button>` : ""}
      </div>`,
    )
    .join("")}</div>`;
}

function aboutTab(): string {
  const helperVersion = state.overview?.status?.version;
  return `
    <div class="card updates">
      <div class="row"><span><b>Version</b></span><span class="value" data-keep>${esc(state.appVersion)}</span></div>
      ${
        state.update
          ? `<button class="primary" id="settings-update" ${state.updating ? "disabled" : ""}>${
              state.updating ? esc(updatingText()) : esc(t("Update to {version}", { version: state.update.version }))
            }</button>
            ${state.updating ? `<div class="progress-track"><i id="settings-update-fill" style="width:${Math.round((state.updateProgress ?? 0) * 100)}%"></i></div>` : ""}`
          : `<button class="outline" id="check-update" ${state.updateMessage === "Checking…" ? "disabled" : ""}>Check for updates</button>`
      }
      ${state.updateMessage ? `<div class="muted small update-message">${esc(state.updateMessage)}</div>` : ""}
      <div class="muted small">CakeVPN also checks by itself a few times a day, and shows a banner when an update is ready.</div>
    </div>
    <div class="about muted small" data-keep>
      CakeVPN ${esc(state.appVersion)}${helperVersion ? ` · helper ${esc(helperVersion)}` : ""} · cakevpn.net
    </div>`;
}

function settingsPaneHtml(tab: SettingsTab): string {
  switch (tab) {
    case "general":
      return generalTab();
    case "connection":
      return connectionTab();
    case "notifications":
      return notificationsTab();
    case "protection":
      return protectionTab();
    case "skip":
      return skipTab();
    case "account":
      return accountTab();
    case "invites":
      return state.account && invitesOn(state.account) ? invitesHtml(state.account) : "";
    case "about":
      return aboutTab();
    case "messages":
      return messagesTab();
  }
}

function renderSettings() {
  const tabs = settingsTabs();
  if (!tabs.some((tab) => tab.id === state.settingsTab)) state.settingsTab = "general";
  const current = tabs.find((tab) => tab.id === state.settingsTab)!;
  app.innerHTML = `
    <header>
      <button class="icon-btn back" id="back" title="Back">‹</button>
      <div class="brand">Settings</div>
      <div class="head-right"></div>
    </header>
    <main class="settings-wide screen">
      <nav class="settings-nav" aria-label="Settings">
        ${tabs
          .map(
            (tab) => `<button class="nav-item ${tab.id === current.id ? "active" : ""}" data-tab="${tab.id}">
              <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">${TAB_ICONS[tab.id]}</svg><span>${tab.label}</span>
            </button>`,
          )
          .join("")}
      </nav>
      <section class="settings-pane" id="settings-pane">
        <h2>${current.label}</h2>
        ${settingsPaneHtml(current.id)}
      </section>
    </main>`;

  $("#back")!.addEventListener("click", closeSettings);
  app.querySelectorAll<HTMLButtonElement>(".nav-item").forEach((button) =>
    button.addEventListener("click", () => {
      state.settingsTab = button.dataset.tab as SettingsTab;
      saved.set("settingsTab", state.settingsTab);
      state.recordingShortcut = false;
      render();
      if (state.settingsTab === "connection") readNetwork();
      if (state.settingsTab === "account" && state.account) loadHistory();
    }),
  );

  // General
  $("#set-lang")?.addEventListener("change", (e) => {
    state.lang = (e.target as HTMLSelectElement).value;
    saved.set("lang", state.lang);
    applyLanguage();
    render();
    syncTray(true);
  });
  $("#theme")?.addEventListener("click", (e) => {
    const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".seg");
    if (!button) return;
    state.theme = button.dataset.theme as Theme;
    saved.set("theme", state.theme);
    applyTheme();
    $("#theme")!.querySelectorAll(".seg").forEach((el) => el.classList.toggle("active", el === button));
  });
  $("#close")?.addEventListener("click", (e) => {
    const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".seg");
    if (!button) return;
    state.closeToTray = button.dataset.close === "tray";
    saved.set("closeToTray", state.closeToTray ? "1" : "0");
    backend.setCloseToTray(state.closeToTray).catch(() => {});
    $("#close")!.querySelectorAll(".seg").forEach((el) => el.classList.toggle("active", el === button));
  });
  $("#set-autostart")?.addEventListener("change", async (e) => {
    const box = e.target as HTMLInputElement;
    state.settingsError = "";
    try {
      state.autostart = await backend.setAutostart(box.checked);
    } catch (err) {
      state.settingsError = asApiError(err).message;
    }
    box.checked = state.autostart;
    if (state.settingsError) render();
  });
  $("#shortcut-change")?.addEventListener("click", () => {
    state.recordingShortcut = true;
    state.shortcutError = "";
    render();
  });
  $("#shortcut-off")?.addEventListener("click", () => useShortcut(""));

  // Connection
  const toggle = (id: string, apply: (on: boolean) => void) =>
    $(id)?.addEventListener("change", (e) => apply((e.target as HTMLInputElement).checked));
  toggle("#set-autoconnect", (on) => {
    state.autoConnect = on;
    saved.set("autoConnect", on ? "1" : "0");
  });
  toggle("#set-reconnect", (on) => {
    state.autoReconnect = on;
    saved.set("autoReconnect", on ? "1" : "0");
  });
  toggle("#set-autowifi", (on) => {
    state.autoWifi = on;
    saved.set("autoWifi", on ? "1" : "0");
    state.declinedNetwork = "";
    if (on) checkPublicWifi();
  });
  $("#trust-here")?.addEventListener("click", () => {
    const name = state.network?.name;
    if (name && !state.trustedWifi.includes(name)) state.trustedWifi.push(name);
    saved.set("trustedWifi", JSON.stringify(state.trustedWifi));
    render();
  });
  app.querySelectorAll<HTMLButtonElement>(".trust-remove").forEach((button) =>
    button.addEventListener("click", () => {
      state.trustedWifi = state.trustedWifi.filter((n) => n !== button.dataset.name);
      saved.set("trustedWifi", JSON.stringify(state.trustedWifi));
      render();
    }),
  );

  // Notifications
  toggle("#set-notify-connection", (on) => {
    state.notifyConnection = on;
    saved.set("notifyConnection", on ? "1" : "0");
  });
  toggle("#set-notify-updates", (on) => {
    state.notifyUpdates = on;
    saved.set("notifyUpdates", on ? "1" : "0");
  });
  toggle("#set-notify-messages", (on) => {
    state.notifyMessages = on;
    saved.set("notifyMessages", on ? "1" : "0");
  });
  $("#test-notification")?.addEventListener("click", () =>
    backend.notify("CakeVPN", t("Notifications work. This is how CakeVPN tells you about your connection.")).catch(() => {}),
  );

  // Protection and Skip the VPN
  toggle("#set-ads", (on) => {
    state.options.blockAds = on;
    saveOptions();
  });
  toggle("#set-kill", (on) => {
    state.options.killSwitch = on;
    saveOptions();
  });
  $("#add-domain")?.addEventListener("submit", (e) => {
    e.preventDefault();
    addSkipped("domain", ($("#new-domain") as HTMLInputElement).value);
  });
  $("#add-app")?.addEventListener("submit", (e) => {
    e.preventDefault();
    addSkipped("app", ($("#new-app") as HTMLInputElement).value);
  });
  app.querySelectorAll<HTMLButtonElement>(".skip-remove").forEach((button) =>
    button.addEventListener("click", () => removeSkipped(button.dataset.kind as "domain" | "app", button.dataset.value!)),
  );
  $("#reconnect")?.addEventListener("click", reconnect);
  document.querySelectorAll<HTMLButtonElement>(".inbox-read").forEach((button) =>
    button.addEventListener("click", () => {
      const id = Number(button.dataset.id);
      if (button.dataset.everyone === "1") {
        closeAnnouncement(id);
        render();
      } else closeMessage(id);
    }),
  );

  // Account and invites
  wireUsageChart();
  $("#sign-out")?.addEventListener("click", signOut);
  $("#make-invite")?.addEventListener("click", makeInvite);
  app.querySelectorAll<HTMLButtonElement>(".copy-code").forEach((button) =>
    button.addEventListener("click", () => copyCode(button)),
  );
  app.querySelectorAll<HTMLButtonElement>(".delete-code").forEach((button) =>
    button.addEventListener("click", () => deleteInvite(button.dataset.code!)),
  );

  // Updates
  $("#settings-update")?.addEventListener("click", installUpdate);
  $("#check-update")?.addEventListener("click", async () => {
    state.updateMessage = "Checking…";
    render();
    await checkForUpdate(true);
    render();
  });
}

/** Keeps a shortcut the person pressed while "Change" was on. */
document.addEventListener("keydown", (e) => {
  if (!state.recordingShortcut) return;
  e.preventDefault();
  if (e.key === "Escape") {
    state.recordingShortcut = false;
    render();
    return;
  }
  const accelerator = acceleratorOf(e);
  if (accelerator) useShortcut(accelerator);
});

async function useShortcut(accelerator: string) {
  state.recordingShortcut = false;
  state.shortcutError = "";
  try {
    await backend.setShortcut(accelerator || null);
    state.shortcut = accelerator;
    saved.set("shortcut", accelerator);
  } catch (e) {
    state.shortcutError = asApiError(e).message;
    // The one that worked before stays.
    backend.setShortcut(state.shortcut || null).catch(() => {});
  }
  if (state.screen === "settings") render();
}

// ---------- protection settings ----------

const MAX_SKIPPED = { domain: 100, app: 50 };

function saveOptions() {
  saved.set("blockAds", state.options.blockAds ? "1" : "0");
  saved.set("killSwitch", state.options.killSwitch ? "1" : "0");
  saved.set("bypassDomains", JSON.stringify(state.options.bypassDomains));
  saved.set("bypassApps", JSON.stringify(state.options.bypassApps));
  state.skipError = "";
  if (state.screen === "settings") render();
}

/** The settings changed since this connection was made. */
function optionsChanged(): boolean {
  return tunnelState() === "connected" && !!state.appliedOptions && state.appliedOptions !== JSON.stringify(state.options);
}

/** "https://www.MyBank.com/login" → "mybank.com"; "" when it isn't a website name. */
function cleanDomain(text: string): string {
  const name = text
    .trim()
    .toLowerCase()
    .replace(/^[a-z]+:\/\//, "")
    .replace(/[/?#:].*$/, "")
    .replace(/^www\./, "")
    .replace(/\.$/, "");
  const labels = name.split(".");
  const ok =
    name.length <= 253 &&
    labels.length >= 2 &&
    labels.every((l) => /^[a-z0-9-]{1,63}$/.test(l) && !l.startsWith("-") && !l.endsWith("-"));
  return ok ? name : "";
}

/** A program's file name, never a path. */
function cleanApp(text: string): string {
  const name = text.trim();
  return name.length > 0 && name.length <= 100 && /^[\p{L}\p{N} ._+()-]+$/u.test(name) ? name : "";
}

function addSkipped(kind: "domain" | "app", text: string) {
  const list = kind === "domain" ? state.options.bypassDomains : state.options.bypassApps;
  const value = kind === "domain" ? cleanDomain(text) : cleanApp(text);
  if (!value) {
    state.skipError =
      kind === "domain"
        ? "Type a website name like mybank.com."
        : `Type the app's name like ${isWindows ? "steam.exe" : "Steam"}, without the folder it is in.`;
    render();
    return;
  }
  if (list.length >= MAX_SKIPPED[kind]) {
    state.skipError = `That's the most ${kind === "domain" ? "websites" : "apps"} that can skip the VPN.`;
    render();
    return;
  }
  if (!list.some((x) => x.toLowerCase() === value.toLowerCase())) list.push(value);
  saveOptions();
}

function removeSkipped(kind: "domain" | "app", value: string) {
  if (kind === "domain") state.options.bypassDomains = state.options.bypassDomains.filter((x) => x !== value);
  else state.options.bypassApps = state.options.bypassApps.filter((x) => x !== value);
  saveOptions();
}

function skipListHtml(kind: "domain" | "app", list: string[]): string {
  if (!list.length) return "";
  return `<div class="skip-list">${list
    .map(
      (value) =>
        `<span class="chip">${kind === "app" ? "▣ " : ""}${esc(value)}<button class="skip-remove" data-kind="${kind}" data-value="${esc(value)}" title="Remove" aria-label="Remove ${esc(value)}">×</button></span>`,
    )
    .join("")}</div>`;
}

/** Connects again to the same location, so new settings take effect. */
async function reconnect() {
  const id = state.overview?.locationId ?? chosenLocation()?.id;
  if (!id) return;
  expectChange();
  state.busy = true;
  render();
  try {
    await connectTo(id);
  } catch (e) {
    state.settingsError = asApiError(e).message;
  }
  state.busy = false;
  await refreshOverview();
  render();
}

// ---------- usage graph ----------

async function loadHistory() {
  try {
    state.history = await backend.usageHistory();
    state.historyError = "";
  } catch (e) {
    state.historyError = asApiError(e).message;
  }
  const chart = $("#usage-chart");
  if (chart && state.screen === "settings") {
    chart.innerHTML = usageChartHtml();
    wireUsageChart();
  }
}

function dayLabel(day: string, long = false): string {
  const date = new Date(`${day}T00:00:00`);
  return date.toLocaleDateString(locale(), long ? { weekday: "short", day: "numeric", month: "short" } : { day: "numeric", month: "short" });
}

/**
 * One column per day. A single series needs no legend; the peak is labeled,
 * pointing at a day shows it in the line above, and the list has every value.
 */
function usageChartHtml(): string {
  const days = state.history;
  if (!days) {
    return `<p class="muted small">${state.historyError ? esc(state.historyError) : "Loading…"}</p>`;
  }
  const total = days.reduce((sum, d) => sum + d.bytes, 0);
  if (total === 0) {
    return `<p class="muted small">Nothing used in the last 30 days yet.</p>`;
  }
  const W = 300;
  const H = 96;
  const top = 16; // room for the peak's label
  const slot = W / days.length;
  const width = Math.min(8, slot - 2); // thin columns, 2px of card between them
  const peak = Math.max(...days.map((d) => d.bytes));
  const peakIndex = days.findIndex((d) => d.bytes === peak);
  const columns = days
    .map((d, i) => {
      const x = i * slot + (slot - width) / 2;
      const h = d.bytes > 0 ? Math.max(2, ((H - top) * d.bytes) / peak) : 0;
      const r = Math.min(3, h, width / 2);
      const y = H - h;
      // Rounded at the top, square on the baseline.
      const bar =
        h > 0
          ? `<path class="bar" d="M${x},${H} V${y + r} Q${x},${y} ${x + r},${y} H${x + width - r} Q${x + width},${y} ${x + width},${y + r} V${H} Z"/>`
          : "";
      return `<g class="day" data-i="${i}">${bar}<rect class="hit" x="${i * slot}" y="0" width="${slot}" height="${H}"/></g>`;
    })
    .join("");
  // The peak's value sits on its cap, kept inside the plot at the edges.
  const peakX = Math.min(W - 28, Math.max(28, peakIndex * slot + slot / 2));
  const rows = [...days]
    .reverse()
    .filter((d) => d.bytes > 0)
    .map((d) => `<div class="row"><span>${esc(dayLabel(d.day, true))}</span><span class="value">${formatBytes(d.bytes)}</span></div>`)
    .join("");
  return `
    <div class="usage-head"><b id="usage-value">${formatBytes(total)}</b><span class="muted small" id="usage-when">in the last 30 days</span></div>
    <div class="usage-plot">
      <svg viewBox="0 0 ${W} ${H + 1}" role="img" aria-label="Data used per day over the last 30 days; ${formatBytes(total)} in total">
        <text class="peak" x="${peakX}" y="${top - 5}" text-anchor="middle">${formatBytes(peak)}</text>
        ${columns}
        <line class="base" x1="0" y1="${H + 0.5}" x2="${W}" y2="${H + 0.5}"/>
      </svg>
    </div>
    <div class="usage-axis muted small"><span>${esc(dayLabel(days[0].day))}</span><span>Today</span></div>
    <button class="link" id="usage-toggle">${state.historyList ? "Hide the numbers" : "Show the numbers"}</button>
    ${state.historyList ? `<div class="usage-list list">${rows}</div>` : ""}`;
}

function wireUsageChart() {
  const plot = document.querySelector<HTMLElement>(".usage-plot");
  const days = state.history;
  $("#usage-toggle")?.addEventListener("click", () => {
    state.historyList = !state.historyList;
    const chart = $("#usage-chart");
    if (chart) {
      chart.innerHTML = usageChartHtml();
      wireUsageChart();
    }
  });
  if (!plot || !days) return;
  const total = days.reduce((sum, d) => sum + d.bytes, 0);
  // Pointing at a day puts it in the line above the plot, where the total was.
  const show = (group: Element) => {
    const d = days[Number((group as HTMLElement).dataset.i)];
    plot.querySelectorAll(".day.on").forEach((g) => g.classList.remove("on"));
    group.classList.add("on");
    setText("#usage-value", formatBytes(d.bytes));
    setText("#usage-when", `on ${dayLabel(d.day, true)}`);
  };
  const hide = () => {
    plot.querySelectorAll(".day.on").forEach((g) => g.classList.remove("on"));
    setText("#usage-value", formatBytes(total));
    setText("#usage-when", "in the last 30 days");
  };
  plot.querySelectorAll(".day").forEach((group) => {
    group.addEventListener("pointerenter", () => show(group));
    group.addEventListener("click", () => show(group));
  });
  plot.addEventListener("pointerleave", hide);
}

function invitesHtml(account: Account): string {
  const r = account.referral;
  const capped = account.plan.mbps > 0;
  const full = r.invites.length >= r.maxFriends;
  const friends = Math.min(r.friends, r.maxFriends);
  const rows = [...r.invites]
    .reverse()
    .map(
      (i) => `
        <div class="invite-row ${i.code === state.newInvite ? "new" : ""}">
          <span class="invite-code">${esc(i.code)}</span>
          <span class="tag ${i.joined ? "joined" : "waiting"}">${i.joined ? "Joined" : "Not used yet"}</span>
          <button class="copy-code" data-code="${esc(i.code)}">Copy</button>
          ${
            i.joined
              ? ""
              : `<button class="delete-code ${state.confirmDelete === i.code ? "confirm" : ""}" data-code="${esc(i.code)}" ${
                  state.busy ? "disabled" : ""
                }>${state.confirmDelete === i.code ? "Delete?" : "Delete"}</button>`
          }
        </div>`,
    )
    .join("");
  return `
      <div class="card invites" id="invites">
        ${
          capped
            ? `<div class="invite-head">
                 <div><b>+${r.bonusMbps} Mbps ${infoTip(moreSpeedTip(account))}</b><small>${friends} of ${r.maxFriends} friends joined</small></div>
                 <div class="progress"><i style="width:${(friends / Math.max(1, r.maxFriends)) * 100}%"></i></div>
               </div>
               <p class="muted small">Each friend who signs in with your invite code gives you +${r.mbpsPerFriend} Mbps, up to ${r.maxFriends} friends. They get their own free CakeVPN.</p>`
            : `<p class="muted small">You already have the fastest plan, so invites don't add speed, but your friends still get their own free CakeVPN.</p>`
        }
        ${state.inviteError ? `<div class="error">${esc(state.inviteError)}</div>` : ""}
        <button class="primary" id="make-invite" ${full || state.busy ? "disabled" : ""}>${
          full ? `You've made all ${r.maxFriends} invites` : state.busy ? "Making a code…" : "Create invite code"
        }</button>
        ${rows ? `<div class="invite-list">${rows}</div>` : ""}
      </div>`;
}

// ---------- language ----------

function applyLanguage() {
  setLanguage(pickLanguage(state.lang));
  watch(app);
}

// ---------- tray menu ----------

let trayShown = "";

/** Keeps the tray menu in step: the status, Connect/Disconnect, and the locations to pick from. */
function syncTray(force = false) {
  const account = state.account;
  const tstate = tunnelState();
  const connectedTo = account?.locations.find((l) => l.id === state.overview?.locationId);
  const on = tstate === "connected" || tstate === "connecting";
  const status =
    tstate === "connected" && connectedTo
      ? t("Connected · {place}", { place: connectedTo.name })
      : tstate === "connecting"
        ? t("Connecting…")
        : t("Not connected");
  const locations = account
    ? [
        { id: "best", name: t("Best location"), chosen: state.choice === "best", enabled: true },
        ...account.locations.map((l) => ({ id: l.id, name: l.name, chosen: state.choice === l.id, enabled: l.online })),
      ]
    : [];
  const model = {
    status,
    toggle: on ? t("Disconnect") : t("Connect"),
    locationsLabel: t("Location"),
    locations,
    open: t("Open CakeVPN"),
    quit: t("Quit CakeVPN"),
    tooltip: `CakeVPN · ${status}`,
  };
  const json = JSON.stringify(model);
  if (!force && json === trayShown) return;
  trayShown = json;
  backend.setTray(model).catch(() => {});
}

/** The tray menu and the keyboard shortcut do what the window's own buttons do. */
async function trayAction(action: string) {
  if (!state.account || state.screen === "setup" || state.busy) return;
  if (action === "toggle") return togglePower();
  const id = action.startsWith("loc:") ? action.slice(4) : "";
  if (!id) return;
  await chooseLocation(id);
  if (tunnelState() === "disconnected" || tunnelState() === "failed") await togglePower();
}

// ---------- notifications ----------

/** A desktop notification, only while the window isn't in front (then the window shows it). */
function notifyUser(kind: "connection" | "update", title: string, body: string) {
  if (kind === "connection" && !state.notifyConnection) return;
  if (kind === "update" && !state.notifyUpdates) return;
  if (windowInFront()) return;
  backend.notify(title, body).catch(() => {});
}

// ---------- the connection by itself ----------

/** The next change of the tunnel was asked for, by the person or by CakeVPN: it isn't a drop. */
function expectChange() {
  state.expectedUntil = Date.now() + 20_000;
}

/**
 * Notices the tunnel dropping by itself, says so, and brings it back when
 * "Reconnect by itself" is on (at most 3 times in 5 minutes).
 */
function watchConnection(before: string, after: string) {
  if (after === "connected") {
    if (state.overview?.locationId) state.lastConnectedId = state.overview.locationId;
    if (state.dropped && before !== "connected") {
      state.dropped = false;
      const place = state.account?.locations.find((l) => l.id === state.overview?.locationId)?.name ?? "";
      notifyUser("connection", t("CakeVPN is connected again"), place ? t("Connected to {place}.", { place }) : "");
    }
    return;
  }
  if (before !== "connected" || Date.now() < state.expectedUntil) return;
  state.dropped = true;
  const coming = state.autoReconnect || state.options.killSwitch;
  notifyUser(
    "connection",
    t("The VPN connection dropped"),
    coming ? t("CakeVPN is connecting again.") : t("Open CakeVPN to connect again."),
  );
  if ((after === "failed" || after === "disconnected") && state.autoReconnect) reconnectSoon();
}

function reconnectSoon() {
  const now = Date.now();
  state.reconnects = state.reconnects.filter((at) => now - at < 5 * 60_000);
  if (state.reconnects.length >= 3) return;
  state.reconnects.push(now);
  setTimeout(async () => {
    const tstate = tunnelState();
    const id = state.lastConnectedId || chosenLocation()?.id;
    if (!state.dropped || !id || tstate === "connected" || tstate === "connecting" || state.busy) return;
    expectChange();
    try {
      await connectTo(id);
    } catch {
      /* the next check notices it is still down */
    }
    await refreshOverview();
    if (state.screen === "home") updateHome();
  }, 3000);
}

/** Reads the network this computer is on (for Settings and for public Wi-Fi). */
async function readNetwork() {
  try {
    state.network = await backend.currentNetwork();
  } catch {
    state.network = null;
  }
  if (state.screen === "settings" && state.settingsTab === "connection") render();
}

/** On a Wi-Fi that isn't trusted, turns the VPN on by itself (once per network, if the person turns it off again). */
async function checkPublicWifi() {
  if (!state.autoWifi || !state.account || state.busy || state.overview?.helper !== "ok") return;
  let net;
  try {
    net = await backend.currentNetwork();
  } catch {
    return;
  }
  state.network = net;
  if (!net.onWifi) {
    state.declinedNetwork = "";
    return;
  }
  const key = net.name ?? "?";
  if ((net.name && state.trustedWifi.includes(net.name)) || state.declinedNetwork === key) return;
  if (tunnelState() !== "disconnected") return;
  const loc = chosenLocation();
  if (!loc) return;
  state.declinedNetwork = key;
  expectChange();
  try {
    await connectTo(loc.id);
    notifyUser(
      "connection",
      t("CakeVPN turned on"),
      net.name ? t("{wifi} is a public Wi-Fi, so the VPN is on.", { wifi: net.name }) : t("This Wi-Fi isn't trusted, so the VPN is on."),
    );
  } catch {
    /* tried once on this network */
  }
  await refreshOverview();
  if (state.screen === "home") updateHome();
}

// ---------- actions ----------

function setScreen(screen: Screen) {
  if (state.screen === screen) return;
  state.screen = screen;
  render();
}

async function openSettings(toInvites = false) {
  try {
    const info = await backend.settingsInfo();
    state.autostart = info.autostart;
    state.appVersion = info.version;
  } catch {
    /* show the page anyway */
  }
  state.settingsError = "";
  state.inviteError = "";
  state.skipError = "";
  if (toInvites) state.settingsTab = "invites";
  state.recordingShortcut = false;
  // Android's back button goes back through the page's history, so
  // Settings adds a step to it: back then closes Settings, not the app.
  if (isAndroid && state.screen !== "settings") history.pushState({ settings: true }, "");
  setScreen("settings");
  // The usage graph is only asked for while its tab is open.
  if (state.settingsTab === "account" && state.account) loadHistory();
  if (state.settingsTab === "connection") readNetwork();
}

async function checkForUpdate(fromButton = false) {
  state.last.update = Date.now();
  try {
    const found = await backend.checkUpdate();
    state.update = found ? { version: found.version } : null;
    if (found && state.notifiedUpdate !== found.version) {
      state.notifiedUpdate = found.version;
      notifyUser("update", t("CakeVPN {version} is ready", { version: found.version }), t("Open CakeVPN to install it."));
    }
    state.updateMessage = fromButton && !found ? "You have the newest version." : "";
  } catch (e) {
    state.updateMessage = fromButton ? asApiError(e).message : "";
  }
  if (state.screen === "home") updateHome();
}

/** "Downloading the update… 45%", then "Installing…" once it is all there. */
function updatingText(): string {
  const p = state.updateProgress;
  if (p === null) return "Downloading the update…";
  return p >= 1 ? "Installing the update…" : `Downloading the update… ${Math.round(p * 100)}%`;
}

/** Shows the download's progress where the update was started, without redrawing the screen. */
function drawUpdateProgress() {
  if (state.screen === "home") return updateHome();
  if (state.screen === "forced") {
    const fill = $("#forced-fill") as HTMLElement | null;
    if (fill) fill.style.width = `${Math.round((state.updateProgress ?? 0) * 100)}%`;
    const button = $("#forced-update");
    if (button) button.textContent = updatingText();
    return;
  }
  const fill = $("#settings-update-fill") as HTMLElement | null;
  if (fill) fill.style.width = `${Math.round((state.updateProgress ?? 0) * 100)}%`;
  const button = $("#settings-update");
  if (button) button.textContent = updatingText();
}

async function installUpdate() {
  state.updating = true;
  state.updateMessage = "";
  state.updateProgress = null;
  if (state.screen === "home") updateHome();
  else render();
  const watch = setInterval(async () => {
    try {
      const p = await backend.updateProgress();
      if (p && p.total > 0) state.updateProgress = Math.min(1, p.downloaded / p.total);
      drawUpdateProgress();
    } catch {
      /* the bar just stays where it is */
    }
  }, 400);
  try {
    // CakeVPN restarts by itself once the update is installed.
    await backend.installUpdate();
    if (isAndroid) {
      clearInterval(watch);
      state.updating = false;
      state.updateMessage = `The download opened in your browser. Open it when it's done to install CakeVPN ${state.update?.version ?? ""}.`;
      if (state.screen === "home") updateHome();
      else render();
    }
  } catch (e) {
    clearInterval(watch);
    state.updating = false;
    state.updateMessage = asApiError(e).message;
    state.actionError = state.screen === "home" ? state.updateMessage : state.actionError;
    if (state.screen === "home") updateHome();
    else render();
  }
}

async function makeInvite() {
  state.busy = true;
  state.inviteError = "";
  render();
  try {
    const before = new Set(state.account?.referral.invites.map((i) => i.code));
    state.account = await backend.createInvite();
    state.newInvite = state.account.referral.invites.find((i) => !before.has(i.code))?.code ?? "";
  } catch (e) {
    state.inviteError = asApiError(e).message;
  }
  state.busy = false;
  render();
}

let confirmTimer: number | undefined;

/** The first press asks "Delete?"; a second press within 4 seconds deletes. */
async function deleteInvite(code: string) {
  clearTimeout(confirmTimer);
  if (state.confirmDelete !== code) {
    state.confirmDelete = code;
    render();
    confirmTimer = window.setTimeout(() => {
      state.confirmDelete = "";
      if (state.screen === "settings") render();
    }, 4000);
    return;
  }
  state.confirmDelete = "";
  state.busy = true;
  state.inviteError = "";
  render();
  try {
    state.account = await backend.deleteInvite(code);
  } catch (e) {
    state.inviteError = asApiError(e).message;
  }
  state.busy = false;
  render();
}

async function copyCode(button: HTMLButtonElement) {
  const code = button.dataset.code!;
  try {
    await navigator.clipboard.writeText(code);
  } catch {
    // Older webviews: copy through a hidden text box.
    const box = document.createElement("textarea");
    box.value = code;
    document.body.appendChild(box);
    box.select();
    document.execCommand("copy");
    box.remove();
  }
  button.textContent = "Copied!";
  button.classList.add("done");
  setTimeout(() => {
    button.textContent = "Copy";
    button.classList.remove("done");
  }, 1500);
}

function closeSettings() {
  // Closed with its own button: take back the history step it added.
  if (isAndroid && history.state?.settings) {
    history.back();
    return; // the back step lands in the listener below, which closes it
  }
  setScreen(state.account ? "home" : "code");
}

window.addEventListener("popstate", () => {
  if (state.screen === "settings") setScreen(state.account ? "home" : "code");
});

async function submitCode(code: string) {
  if (state.busy || state.lockedUntil > Date.now()) return;
  state.busy = true;
  state.codeError = "";
  render();
  try {
    state.account = await backend.redeem(code);
    state.codeNotice = "";
    state.screen = "home";
    refreshPings();
  } catch (e) {
    const err = asApiError(e);
    if (err.error === "locked" && err.retryAfter) {
      state.lockedUntil = Date.now() + err.retryAfter * 1000;
    } else if (err.error === "wrong_code" && err.triesLeft != null) {
      state.codeError =
        err.triesLeft === 1 ? "That code is not valid. 1 try left." : `That code is not valid. ${err.triesLeft} tries left.`;
    } else {
      state.codeError = err.message;
    }
  }
  state.busy = false;
  await refreshOverview();
  render();
}

/** Connects with the protection settings as they are now. */
async function connectTo(locationId: string) {
  const options = state.options;
  await backend.connect(locationId, options);
  state.appliedOptions = JSON.stringify(options);
  state.speedTest = { phase: "", down: null, up: null, error: "" };
}

/** "Ads blocked · Kill switch on · 2 skip the VPN", for the connection as it was made. */
function protectionText(): string {
  let applied: ConnectOptions;
  try {
    applied = JSON.parse(state.appliedOptions || "null") ?? state.options;
  } catch {
    applied = state.options;
  }
  const skipped = applied.bypassDomains.length + applied.bypassApps.length;
  return [
    applied.blockAds && t("Ads blocked"),
    applied.killSwitch && t("Kill switch on"),
    skipped === 1 && t("1 skips the VPN"),
    skipped > 1 && t("{n} skip the VPN", { n: skipped }),
  ]
    .filter(Boolean)
    .join(" · ");
}

function formatMbps(mbps: number): string {
  return `${mbps >= 100 ? Math.round(mbps) : mbps.toFixed(1)} Mbps`;
}

/** Messages this person hasn't closed yet. */
function openMessages() {
  return (state.account?.messages ?? []).filter((m) => !state.closedMessages.has(m.id));
}

/** The panel's messages for this person, until they close each one. */
function drawMessages() {
  const box = $("#personal-messages");
  if (!box) return;
  const list = openMessages();
  const key = list.map((m) => `${m.id}:${m.kind}:${m.text}`).join("|");
  if (key === state.shownMessages) return;
  state.shownMessages = key;
  box.innerHTML = list
    .map(
      (m) => `<div class="announce personal ${m.kind === "warning" ? "is-warning" : ""}">
        <span class="announce-icon">${m.kind === "warning" ? "⚠️" : "✉️"}</span>
        <span class="announce-text"><b>Message from CakeVPN</b><span class="message-body" data-keep>${esc(m.text)}</span></span>
        <button class="message-close" data-id="${m.id}" title="Close" aria-label="Close this message">×</button>
      </div>`,
    )
    .join("");
}

async function closeMessage(id: number) {
  state.closedMessages.add(id);
  updateHome();
  if (state.screen === "settings" && state.settingsTab === "messages") render();
  try {
    await backend.closeMessage(id);
  } catch {
    /* it stays closed here; the server hears of it next time */
  }
}

/**
 * A notification for each message not seen before (on a computer: the
 * page keeps running in the tray; on a phone the app does it by itself).
 */
function notifyNewMessages() {
  const messages = state.account?.messages ?? [];
  const fresh = messages.filter((m) => !state.notifiedMessages.has(m.id));
  fresh.forEach((m) => state.notifiedMessages.add(m.id));
  if (fresh.length) saved.set("notifiedMessages", JSON.stringify([...state.notifiedMessages].slice(-50).map(String)));
  const news = allAnnouncements().filter((a) => !state.notifiedAnnouncements.has(a.id));
  news.forEach((a) => state.notifiedAnnouncements.add(a.id));
  if (news.length) saved.set("notifiedAnnouncements", JSON.stringify([...state.notifiedAnnouncements].slice(-100).map(String)));
  // Android shows these from the app itself (see phone.rs), also when the page sleeps.
  if (isAndroid || !state.notifyMessages) return;
  for (const m of fresh.reverse()) {
    if (m.notify !== false) backend.notify(t("Message from CakeVPN"), m.text).catch(() => {});
  }
  for (const a of news.reverse()) {
    if (a.notify) backend.notify("CakeVPN", a.text).catch(() => {});
  }
}

/** The panel's announcements for this app (older panels send just one). */
function allAnnouncements(): Announcement[] {
  const account = state.account;
  if (account?.announcements) return account.announcements;
  return account?.announcement ? [account.announcement] : [];
}

function openAnnouncements(): Announcement[] {
  return allAnnouncements().filter((a) => !state.closedAnnouncements.has(a.id) && a.id !== state.closedAnnouncement);
}

/** Each announcement at the top, until this person closes it. */
function drawAnnouncements() {
  const box = $("#announcements");
  if (!box) return;
  const list = openAnnouncements();
  const key = list.map((a) => `${a.id}:${a.kind}:${a.text}`).join("|");
  if (key === state.shownAnnouncements) return;
  state.shownAnnouncements = key;
  box.innerHTML = list
    .map(
      (a) => `<div class="announce ${a.kind === "warning" ? "is-warning" : ""}">
        <span class="announce-icon">${a.kind === "warning" ? "⚠️" : "📣"}</span>
        <span class="announce-text" data-keep>${esc(a.text)}</span>
        <button class="announce-close" data-id="${a.id}" title="Close" aria-label="Close this message">×</button>
      </div>`,
    )
    .join("");
}

function closeAnnouncement(id: number) {
  state.closedAnnouncements.add(id);
  saved.set("closedAnnouncements", JSON.stringify([...state.closedAnnouncements].slice(-100).map(String)));
  updateHome();
}

/** Moves the speed test's bar to `percent` over `seconds`. */
function speedTestBar(percent: number, seconds: number) {
  const fill = $("#speedtest-fill") as HTMLElement | null;
  if (!fill) return;
  fill.style.transition = seconds > 0 ? `width ${seconds}s linear` : "none";
  // Reading the width makes the browser apply the last step before this one starts.
  void fill.offsetWidth;
  fill.style.width = `${percent}%`;
}

/** Download, then upload. The server allows one test a minute and a few a day. */
async function runSpeedTest() {
  if (state.speedTest.phase) return;
  state.speedTest = { phase: "down", down: null, up: null, error: "" };
  updateHome();
  // The download runs for up to 8 seconds and the upload for about 6: the
  // bar moves through its first part and then the rest in those times.
  speedTestBar(0, 0);
  speedTestBar(60, 8);
  try {
    state.speedTest.down = await backend.speedTestDownload();
    state.speedTest.phase = "up";
    updateHome();
    speedTestBar(60, 0.2);
    speedTestBar(97, 6);
    state.speedTest.up = await backend.speedTestUpload();
    speedTestBar(100, 0.2);
  } catch (e) {
    state.speedTest.error = asApiError(e).message;
  }
  state.speedTest.phase = "";
  updateHome();
}

async function togglePower() {
  const tstate = tunnelState();
  expectChange();
  state.dropped = false;
  // Turned off on a public Wi-Fi: it stays off there.
  if ((tstate === "connected" || tstate === "connecting") && state.network?.onWifi) state.declinedNetwork = state.network.name ?? "?";
  state.busy = true;
  state.actionError = "";
  state.moveNotice = "";
  updateHome();
  try {
    if (tstate === "connected" || tstate === "connecting") {
      await backend.disconnect();
    } else {
      const loc = chosenLocation();
      if (!loc) throw { message: "No location is online right now." };
      await connectTo(loc.id);
    }
  } catch (e) {
    const err = asApiError(e);
    if (err.message === "helper_missing") state.screen = "setup";
    else if (err.message.startsWith("update_required")) {
      state.busy = false;
      enterForcedUpdate(err.message.split(":")[1] ?? "");
      return;
    } else {
      state.actionError = err.message;
      // The service hung up while connecting: the fix screen has the way out.
      if (err.message.startsWith("CakeVPN's background service stopped while working")) state.screen = "setup";
    }
  }
  state.busy = false;
  await refreshOverview();
  if (state.screen === "setup") render();
  else updateHome();
}

async function chooseLocation(id: string) {
  state.choice = id;
  saved.set("location", id);
  state.moveNotice = "";
  updateLocations();
  // Switching while connected moves the tunnel right away.
  if (tunnelState() === "connected") {
    const loc = chosenLocation();
    if (loc && loc.id !== state.overview?.locationId) {
      expectChange();
      state.busy = true;
      updateHome();
      try {
        await connectTo(loc.id);
      } catch (e) {
        state.actionError = asApiError(e).message;
      }
      state.busy = false;
      await refreshOverview();
      updateHome();
    }
  }
}

async function signOut() {
  expectChange();
  await backend.signOut();
  state.account = null;
  state.codeNotice = "";
  setScreen("code");
}

async function installHelper() {
  state.busy = true;
  state.actionError = "";
  render();
  try {
    await backend.installHelper();
    await refreshOverview();
    state.screen = state.account ? "home" : "code";
  } catch (e) {
    state.actionError = asApiError(e).message;
    await readHelperProblem();
  }
  state.busy = false;
  render();
}

// ---------- polling ----------

function handleSignedOut(message: string) {
  state.account = null;
  state.codeNotice = message;
  state.screen = "code";
  render();
}

async function refreshOverview() {
  try {
    const ov = await backend.overview();
    const before = tunnelState();
    state.overview = ov;
    watchConnection(before, tunnelState());
    noteBaseline();
    void leaveTroubledLocation();
    syncTray();
    const s = ov.status;
    if (s && s.state === "connected") {
      const now = Date.now();
      if (state.lastBytes) {
        const dt = (now - state.lastBytes.at) / 1000;
        if (dt > 0) {
          state.speed = {
            up: Math.max(0, (s.upBytes - state.lastBytes.up) / dt),
            down: Math.max(0, (s.downBytes - state.lastBytes.down) / dt),
          };
        }
      }
      state.lastBytes = { at: now, up: s.upBytes, down: s.downBytes };
    } else {
      state.lastBytes = null;
      state.speed = { up: 0, down: 0 };
    }
    showSpeed(state.speed);
    if ((ov.helper === "missing" || ov.helper === "outdated") && state.screen === "home") setScreen("setup");
    if (ov.helper === "ok" && state.screen === "setup") {
      problemAsked = false;
      state.helperProblem = null;
      setScreen(state.account ? "home" : "code");
    }
  } catch {
    /* keep the last view; the next poll tries again */
  }
}

async function refreshAccount() {
  if (!state.account) return;
  state.lastRefresh = Date.now();
  try {
    state.account = await backend.refreshAccount();
    notifyNewMessages();
    state.offline = false;
    syncTray();
    updateLocations();
    updateHome();
  } catch (e) {
    const err = asApiError(e);
    if (err.error === "offline" && !state.offline) {
      state.offline = true;
      updateHome();
    }
    if (err.error === "signed_out") handleSignedOut("You were signed out because this code was used on another device.");
    if (err.error === "code_disabled") handleSignedOut("This code has been turned off. Ask for a new one.");
    if (err.error === "update_required") enterForcedUpdate(err.version ?? "");
  }
}

/** How long trouble must last before CakeVPN moves (ms). */
const DOWN_FOR = 5_000;
const SLOW_FOR = 30_000;
/** CakeVPN moves by itself at most once in this long (ms), so people don't bounce around. */
const MOVE_AT_MOST_EVERY = 10 * 60_000;

/** Remembers the fastest checks of this connection, which a sudden slowdown is measured against. */
function noteBaseline() {
  const status = state.overview?.status;
  if (status?.state !== "connected") return;
  const key = `${state.overview?.locationId}@${status.connectedSince}`;
  if (state.baseline.key !== key) state.baseline = { key, tunnel: 0, direct: 0 };
  const q = status.quality;
  const lowest = (was: number, now: number | null | undefined) => (now ? (was ? Math.min(was, now) : now) : was);
  state.baseline.tunnel = lowest(state.baseline.tunnel, q?.tunnelDelayMs);
  state.baseline.direct = lowest(state.baseline.direct, q?.directDelayMs);
}

/** At least twice as slow and 300 ms slower than the fastest check. */
function jumped(now: number | null | undefined, base: number): boolean {
  return !!now && !!base && now >= Math.max(2 * base, base + 300);
}

/**
 * What is wrong with the connected location, if it's the location's fault:
 * - "down": the checks through the VPN time out while the internet itself answers;
 * - "slow": the ping through the VPN jumped while the location's load is high,
 *   and the internet outside the VPN didn't slow down with it.
 * Anything else (the Wi-Fi, the internet, another location's load) is no reason to move.
 */
function troubleNow(current: Location): Trouble {
  const q = state.overview?.status?.quality;
  if (!q || q.directFailures > 0) return "";
  if (q.tunnelFailures >= 3) return "down";
  const base = state.baseline;
  if (overloaded(current) && jumped(q.tunnelDelayMs, base.tunnel) && !jumped(q.directDelayMs, base.direct)) return "slow";
  return "";
}

/**
 * Moves the tunnel to the closest other location when the connected one
 * stops responding (5 s after the third check in a row timed out), or gets
 * slow from its own high load for 30 seconds.
 */
async function leaveTroubledLocation() {
  const current = (state.account?.locations ?? []).find((l) => l.id === state.overview?.locationId);
  const kind = tunnelState() === "connected" && current ? troubleNow(current) : "";
  const now = Date.now();
  if (kind !== state.trouble.kind) state.trouble = { kind, since: now };
  if (!kind || !current || state.busy) return;
  if (now - state.trouble.since < (kind === "down" ? DOWN_FOR : SLOW_FOR) || now - state.movedAt < MOVE_AT_MOST_EVERY) return;
  const target = otherLocation(current, kind);
  if (!target) return;
  state.trouble = { kind: "", since: now };
  state.movedAt = now;
  expectChange();
  followMove(target);
  state.busy = true;
  updateLocations();
  updateHome();
  try {
    await connectTo(target.id);
    showMoveNotice(moveMessage(current, target, kind));
    notifyUser("connection", t("CakeVPN moved you"), state.moveNotice);
  } catch (e) {
    state.actionError = asApiError(e).message;
  }
  state.busy = false;
  await refreshOverview();
  updateLocations();
  updateHome();
}

async function refreshPings() {
  // While connected the helper measures outside the tunnel; while connecting nothing can.
  if (!state.account || tunnelState() === "connecting") return;
  try {
    const measured = await backend.pingLocations();
    // A location that didn't answer keeps its last ping rather than losing it.
    for (const [id, ms] of Object.entries(measured)) if (ms != null) state.pings[id] = ms;
    saved.set("pings", JSON.stringify(state.pings));
    updateLocations();
  } catch {
    /* pings are only a hint */
  }
}

async function start() {
  applyTheme();
  applyLanguage();
  render();
  listen<string>("tray-action", (e) => trayAction(e.payload)).catch(() => {});
  backend.setCloseToTray(state.closeToTray).catch(() => {});
  if (state.shortcut) {
    backend.setShortcut(state.shortcut).catch((e) => (state.shortcutError = asApiError(e).message));
  }
  try {
    const session = await backend.loadSession();
    state.account = session.account;
    state.offline = !!session.offline;
    state.lastRefresh = Date.now();
    state.screen = session.signedIn && session.account ? "home" : "code";
    // Messages sent while CakeVPN was closed get their notification now.
    if (!session.offline) notifyNewMessages();
  } catch (e) {
    const err = asApiError(e);
    state.screen = "code";
    if (err.error === "update_required") {
      state.forcedVersion = err.version ?? "";
      state.screen = "forced";
    } else if (err.error === "signed_out") state.codeNotice = "You were signed out because this code was used on another device.";
    else if (err.error === "code_disabled") state.codeNotice = "This code has been turned off. Ask for a new one.";
    else state.codeNotice = err.message;
  }
  await refreshOverview();
  if (state.overview && state.overview.helper !== "ok" && state.screen !== "forced") state.screen = "setup";
  render();
  // Too old for the panel: on a computer the update starts right away.
  if (state.screen === "forced") enterForcedUpdate(state.forcedVersion);
  await refreshPings();

  syncTray(true);
  if (state.autoConnect && state.screen === "home" && tunnelState() === "disconnected") togglePower();
  else checkPublicWifi();
  checkForUpdate();
  state.last.overview = state.last.pings = Date.now();
  setInterval(tick, 1000);
  // Coming back to the window catches up right away.
  window.addEventListener("focus", () => tick(true));
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) tick(true);
  });
}

/** False while CakeVPN sits in the tray or the window is minimized. */
function windowVisible(): boolean {
  return !document.hidden && state.overview?.windowVisible !== false;
}

/** The window is in front: nothing covers it and the person is using it. */
function windowInFront(): boolean {
  return windowVisible() && document.hasFocus();
}

/**
 * How often each thing is checked (ms): every second while the window is in
 * front (so "Speed now" moves smoothly); covered by another app or in the
 * tray, only every 10 seconds, enough to notice the VPN going up or down.
 */
const PACE = {
  // A little under a second, so a timer that fires a moment early doesn't skip a turn.
  overview: { inFront: 900, visible: 10_000, hidden: 10_000 },
  account: { visible: 10_000, hiddenConnected: 60_000, hidden: 5 * 60_000, unreachable: 30_000 },
  pings: 30_000,
  /** Public Wi-Fi: often enough to turn the VPN on soon after joining one. */
  network: 15_000,
  update: 3 * 60 * 60_000,
  updateOnReturn: 30 * 60_000,
};

let ticking = false;

/**
 * Runs every second and does whatever is due. One ticker instead of several
 * timers keeps the app quiet while nobody is looking: nothing but a status
 * check every 10 seconds then, and the account once a minute while connected.
 */
async function tick(returned = false) {
  if (ticking) return;
  ticking = true;
  try {
    const now = Date.now();
    // "Looking" means the window is open and in front. Covered by another
    // app, minimized or in the tray all count as not looking.
    let looking = windowInFront() || returned;
    const overviewEvery = looking ? PACE.overview.inFront : PACE.overview.hidden;
    if (now - state.last.overview >= overviewEvery || returned) {
      state.last.overview = now;
      await refreshOverview();
      looking = windowInFront();
      if (state.screen === "code" && state.lockedUntil > 0) {
        if (state.lockedUntil <= Date.now()) {
          state.lockedUntil = 0;
          render();
        } else {
          updateCodeMessage();
        }
      }
    }
    // The connected timer counts every second, but only while someone is looking.
    if (looking && state.screen === "home") updateHome();

    const connected = tunnelState() === "connected";
    let accountEvery = looking ? PACE.account.visible : connected ? PACE.account.hiddenConnected : PACE.account.hidden;
    // A server that can't be reached (and no VPN to reach it through) is asked less often.
    if (state.offline && !connected) accountEvery = Math.max(accountEvery, PACE.account.unreachable);
    if (now - state.lastRefresh >= accountEvery) await refreshAccount();

    if (looking && now - state.last.pings >= PACE.pings) {
      state.last.pings = now;
      await refreshPings();
    }
    if (state.autoWifi && now - state.last.network >= PACE.network) {
      state.last.network = now;
      await checkPublicWifi();
    }
    if (now - state.last.update >= PACE.update || (returned && now - state.last.update >= PACE.updateOnReturn)) {
      await checkForUpdate();
    }
  } finally {
    ticking = false;
  }
}

start();
