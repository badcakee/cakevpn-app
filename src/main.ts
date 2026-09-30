import { Account, asApiError, backend, ConnectOptions, DayUsage, Location, Overview } from "./backend";

// ---------- state ----------

type Screen = "loading" | "code" | "home" | "setup" | "settings";
type Theme = "system" | "light" | "dark";

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
  pickerOpen: false,
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
  /** Why CakeVPN moved to another location, shown under the button. */
  moveNotice: "",
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
  speedTest: { phase: "" as "" | "down" | "up", down: null as number | null, up: null as number | null, error: "" },
  /** The last 30 days for the usage graph; null until Settings asked for it. */
  history: null as DayUsage[] | null,
  historyError: "",
  historyList: false,
  /** When things were last checked (ms), for the one ticker that paces everything. */
  last: { overview: 0, pings: 0, update: 0 },
  /** When the account was last asked for, to ask less often while offline. */
  lastRefresh: 0,
  /** Since when (ms) the connected location has been overloaded; 0 when it isn't. */
  overloadSince: 0,
  /** The invite code whose Delete button was pressed once and now asks to confirm. */
  confirmDelete: "",
  /** When CakeVPN last moved away from an overloaded location. */
  movedAt: 0,
};

const app = document.getElementById("app")!;
const isWindows = navigator.userAgent.includes("Windows");

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
  if (!el) return;
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

/** Shows a new speed reading: gliding when someone is looking, at once otherwise. */
function showSpeed(target: { up: number; down: number }) {
  if (!windowVisible() || state.screen !== "home") {
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

/** Lower is better: a quick answer and a quiet server. */
function score(loc: Location): number {
  const ping = state.pings[loc.id];
  return (ping ?? 250) + (loc.load?.percent ?? 50) * 2;
}

/** Above this load (in percent) CakeVPN sends people to a quieter location. */
const OVERLOAD_PERCENT = 85;

function overloaded(loc: Location | undefined): boolean {
  return (loc?.load?.percent ?? 0) > OVERLOAD_PERCENT;
}

/** The best online location that isn't overloaded, other than `except`. */
function quieterLocation(except?: Location): Location | undefined {
  return (state.account?.locations ?? [])
    .filter((l) => l.online && l.id !== except?.id && !overloaded(l))
    .sort((a, b) => score(a) - score(b))[0];
}

function bestLocation(): Location | undefined {
  const online = (state.account?.locations ?? []).filter((l) => l.online);
  // With no ping measured yet, load alone would send people far away: keep the list's order.
  if (!online.some((l) => state.pings[l.id] != null)) return online.find((l) => !overloaded(l)) ?? online[0];
  return quieterLocation() ?? online.sort((a, b) => score(a) - score(b))[0];
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

function moveMessage(from: Location, to: Location): string {
  return `${from.name} is very busy right now (${from.load?.percent ?? OVERLOAD_PERCENT}% load), so CakeVPN moved you to ${to.name}.`;
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

function renderSetup() {
  const outdated = state.overview?.helper === "outdated";
  app.innerHTML = `
    ${header()}
    <main class="center setup screen">
      <div class="logo big">🔧</div>
      <h1>${outdated ? "Update needed" : "One-time setup"}</h1>
      <p class="muted">${
        isWindows
          ? "The CakeVPN service is not running. Reinstall CakeVPN to fix it."
          : outdated
            ? "CakeVPN was updated and its network helper needs updating too."
            : "CakeVPN needs to install a small network helper. Your Mac will ask for your password once."
      }</p>
      ${state.actionError ? `<div class="error">${esc(state.actionError)}</div>` : ""}
      ${isWindows ? "" : `<button class="primary" id="install" ${state.busy ? "disabled" : ""}>${state.busy ? "Setting up…" : "Set up"}</button>`}
    </main>`;
  $("#install")?.addEventListener("click", installHelper);
}

const GEAR = `<svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path fill="currentColor" d="M19.4 13a7.5 7.5 0 0 0 0-2l2.1-1.6-2-3.5-2.5 1a7.7 7.7 0 0 0-1.7-1l-.4-2.7h-4l-.4 2.7a7.7 7.7 0 0 0-1.7 1l-2.5-1-2 3.5L4.6 11a7.5 7.5 0 0 0 0 2l-2.1 1.6 2 3.5 2.5-1a7.7 7.7 0 0 0 1.7 1l.4 2.7h4l.4-2.7a7.7 7.7 0 0 0 1.7-1l2.5 1 2-3.5zM12 15.5a3.5 3.5 0 1 1 0-7 3.5 3.5 0 0 1 0 7z"/></svg>`;

function loadBadge(l: Location): string {
  if (!l.load) return "";
  return `<span class="load ${l.load.level}" title="How busy this location's server is, from everyone using it">
      <span class="load-bar"><i style="width:${Math.max(4, l.load.percent)}%"></i></span>
      <span class="load-pct">Load ${l.load.percent}%</span>
    </span>`;
}

function locationRow(l: Location, selected: boolean, best = false): string {
  const ping = state.pings[l.id];
  return `
    <button class="loc ${selected ? "selected" : ""}" data-loc="${best ? "best" : esc(l.id)}" ${l.online ? "" : "disabled"}>
      ${flag(l.country)}
      <span class="loc-name">${esc(best ? "Best location" : l.name)}${best ? `<small>${esc(l.name)}</small>` : ""}</span>
      <span class="loc-meta">${loadBadge(l)}<span class="ping">${ping != null ? `${ping} ms` : ""}</span></span>
    </button>`;
}

function pickerHtml(): string {
  const best = bestLocation();
  return `${best ? locationRow(best, state.choice === "best", true) : ""}
    ${(state.account?.locations ?? []).map((l) => locationRow(l, state.choice === l.id)).join("")}`;
}

function currentLocationHtml(): string {
  const loc = shownLocation();
  const ping = loc ? state.pings[loc.id] : null;
  // "Best location" only when that is really where this is, not while the VPN is connected somewhere else.
  const isBest = state.choice === "best" && !!loc && loc.id === bestLocation()?.id;
  return `${loc ? flag(loc.country) : ""}
    <span class="loc-name">${loc ? esc(loc.name) : "No location online"}${isBest ? "<small>Best location</small>" : ""}</span>
    <span class="loc-meta">${loc ? loadBadge(loc) : ""}<span class="ping">${ping != null ? `${ping} ms` : ""}</span><span class="chevron">▾</span></span>`;
}

function renderHome() {
  const account = state.account!;
  app.innerHTML = `
    ${header(`<span class="plan ${account.plan.id}">${esc(planLabel(account))}</span>
      <button class="icon-btn" id="open-settings" title="Settings">${GEAR}</button>`)}
    <main class="home screen">
      <div class="announce hidden" id="announce">
        <span class="announce-icon" id="announce-icon"></span>
        <span class="announce-text" id="announce-text"></span>
        <button id="announce-close" title="Close" aria-label="Close this message">×</button>
      </div>
      <div class="update-bar" id="update-bar">
        <span id="update-text"></span>
        <button id="update-now">Update now</button>
        <div class="progress-track" id="update-track"><i id="update-fill"></i></div>
      </div>
      <button id="power" class="power">
        <svg class="ring" viewBox="0 0 120 120" aria-hidden="true">
          <circle class="ring-track" cx="60" cy="60" r="54"></circle>
          <circle class="ring-arc" cx="60" cy="60" r="54"></circle>
        </svg>
        <span class="power-icon">⏻</span>
        <span class="power-text" id="power-text"></span>
      </button>
      <div class="status-line" id="status-line"></div>
      <div class="error hidden" id="action-error"></div>
      <div class="notice hidden" id="move-notice"></div>
      <div class="notice hidden" id="offline-notice">CakeVPN's sign-in server can't be reached on this network. You can still connect with your saved details.</div>
      <div class="banner" id="banner"><span class="banner-icon"></span><span class="banner-text"></span></div>

      <div class="card">
        <div class="card-label">Location</div>
        <button class="loc current" id="toggle-picker">${currentLocationHtml()}</button>
        <div class="picker ${state.pickerOpen ? "open" : ""}" id="picker"><div class="picker-inner">${pickerHtml()}</div></div>
        <div class="your-ip" id="your-ip"></div>
      </div>

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
    </main>`;

  $("#power")!.addEventListener("click", togglePower);
  $("#open-settings")!.addEventListener("click", () => openSettings());
  $("#invite-link")?.addEventListener("click", () => openSettings(true));
  $("#update-now")!.addEventListener("click", installUpdate);
  $("#announce-close")!.addEventListener("click", closeAnnouncement);
  $("#run-speedtest")!.addEventListener("click", runSpeedTest);
  updateLocations();
  $("#toggle-picker")!.addEventListener("click", () => {
    state.pickerOpen = !state.pickerOpen;
    $("#picker")!.classList.toggle("open", state.pickerOpen);
    $("#toggle-picker")!.classList.toggle("open", state.pickerOpen);
  });
  $("#picker")!.addEventListener("click", (e) => {
    const row = (e.target as HTMLElement).closest<HTMLButtonElement>(".loc");
    if (row && !row.disabled) chooseLocation(row.dataset.loc!);
  });
  updateHome();
}

/** Updates the home screen in place. */
function updateHome() {
  if (state.screen !== "home" || !state.account) return;
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
  const moved = $("#move-notice")!;
  moved.classList.toggle("hidden", !state.moveNotice);
  setText("#move-notice", state.moveNotice);
  // Once connected the server is reached through the VPN, so the note goes away.
  $("#offline-notice")!.classList.toggle("hidden", !state.offline || tstate === "connected" || tstate === "connecting");

  const banner = $("#banner")!;
  const b = state.overview?.banner;
  banner.className = `banner ${b ? `show ${b.kind}` : ""}`;
  if (b) {
    setText("#banner .banner-icon", { wifi: "📶", internet: "🌐", load: "🔥", vpn: "🛠️" }[b.kind] ?? "⚠️");
    setText("#banner .banner-text", b.message);
  }

  // The panel's message, until this person closes it.
  const message = state.account.announcement;
  const showMessage = !!message && message.id !== state.closedAnnouncement;
  const announce = $("#announce")!;
  announce.className = `announce ${!showMessage ? "hidden" : message!.kind === "warning" ? "is-warning" : ""}`;
  if (showMessage) {
    setText("#announce-icon", message!.kind === "warning" ? "⚠️" : "📣");
    setText("#announce-text", message!.text);
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
  if (state.screen !== "home") return;
  const current = $("#toggle-picker");
  const inner = $("#picker .picker-inner");
  if (current) current.innerHTML = currentLocationHtml();
  if (inner) inner.innerHTML = pickerHtml();
  // The addresses belong to the main location; other locations use their own.
  const main = shownLocation()?.id === "main";
  setText("#your-ip", main ? ipsText(state.account) : "");
}

function renderSettings() {
  const account = state.account;
  const helperVersion = state.overview?.status?.version;
  const themeButton = (value: Theme, label: string) =>
    `<button class="seg ${state.theme === value ? "active" : ""}" data-theme="${value}">${label}</button>`;
  app.innerHTML = `
    <header>
      <button class="icon-btn back" id="back" title="Back">‹</button>
      <div class="brand">Settings</div>
      <div class="head-right"></div>
    </header>
    <main class="settings screen">
      <div class="section-label">General</div>
      <div class="card list">
        <label class="row">
          <span><b>Open at startup</b><small>Start CakeVPN in the tray when your computer starts</small></span>
          <input type="checkbox" class="switch" id="set-autostart" ${state.autostart ? "checked" : ""}>
        </label>
        <label class="row">
          <span><b>Connect automatically</b><small>Connect as soon as CakeVPN opens</small></span>
          <input type="checkbox" class="switch" id="set-autoconnect" ${state.autoConnect ? "checked" : ""}>
        </label>
      </div>
      ${state.settingsError ? `<div class="error">${esc(state.settingsError)}</div>` : ""}

      <div class="section-label">Protection</div>
      <div class="card list">
        <label class="row">
          <span><b>Block ads and trackers</b><small>Stops known ad and tracking sites, in every app on this computer</small></span>
          <input type="checkbox" class="switch" id="set-ads" ${state.options.blockAds ? "checked" : ""}>
        </label>
        <label class="row">
          <span><b>Kill switch</b><small>If the VPN drops, your internet stays blocked until it reconnects or you disconnect</small></span>
          <input type="checkbox" class="switch" id="set-kill" ${state.options.killSwitch ? "checked" : ""}>
        </label>
      </div>

      <div class="section-label">Skip the VPN</div>
      <div class="card skip">
        <p class="muted small">These websites and apps use your normal internet instead of the VPN.</p>
        ${skipListHtml("domain", state.options.bypassDomains)}
        <form class="skip-add" id="add-domain">
          <input type="text" id="new-domain" placeholder="Website, like mybank.com" autocomplete="off" spellcheck="false">
          <button class="outline small-btn">Add</button>
        </form>
        ${skipListHtml("app", state.options.bypassApps)}
        <form class="skip-add" id="add-app">
          <input type="text" id="new-app" placeholder="${isWindows ? "App, like steam.exe" : "App, like Steam"}" autocomplete="off" spellcheck="false">
          <button class="outline small-btn">Add</button>
        </form>
        ${state.skipError ? `<div class="error">${esc(state.skipError)}</div>` : ""}
      </div>
      ${
        optionsChanged()
          ? `<div class="notice reconnect">Reconnect to use your new settings.
               <button id="reconnect" ${state.busy ? "disabled" : ""}>Reconnect now</button></div>`
          : ""
      }

      <div class="section-label">Appearance</div>
      <div class="segmented" id="theme">
        ${themeButton("system", "Automatic")}${themeButton("light", "Light")}${themeButton("dark", "Dark")}
      </div>

      ${invitesOn(account) ? invitesHtml(account!) : ""}

      ${
        account
          ? `<div class="section-label">Account</div>
      <div class="card list">
        <div class="row"><span><b>Plan</b></span><span class="value">${esc(planLabel(account))}</span></div>
        <div class="row"><span><b>Used this month</b></span><span class="value">${formatBytes(account.usage.bytes)}</span></div>
        <div class="row"><span><b>Traffic</b></span><span class="value">Unlimited</span></div>
        ${account.ips?.ipv4 ? `<div class="row"><span><b>Your IPv4</b></span><span class="value ip">${esc(account.ips.ipv4)}</span></div>` : ""}
        <div class="row"><span><b>Your IPv6</b></span><span class="value ip">${
          account.ips?.ipv6 ? esc(account.ips.ipv6) : "Shared"
        }</span></div>
      </div>
      <div class="section-label">Last 30 days</div>
      <div class="card usage-card" id="usage-card">${usageChartHtml()}</div>
      <button class="danger-outline" id="sign-out">Sign out of this device</button>`
          : ""
      }

      <div class="section-label">Updates</div>
      <div class="card updates">
        <div class="row"><span><b>Version</b></span><span class="value">${esc(state.appVersion)}</span></div>
        ${
          state.update
            ? `<button class="primary" id="settings-update" ${state.updating ? "disabled" : ""}>${
                state.updating ? esc(updatingText()) : `Update to ${esc(state.update.version)}`
              }</button>
              ${state.updating ? `<div class="progress-track"><i id="settings-update-fill" style="width:${Math.round((state.updateProgress ?? 0) * 100)}%"></i></div>` : ""}`
            : `<button class="outline" id="check-update" ${state.updateMessage === "Checking…" ? "disabled" : ""}>Check for updates</button>`
        }
        ${state.updateMessage ? `<div class="muted small update-message">${esc(state.updateMessage)}</div>` : ""}
        <div class="muted small">CakeVPN also checks by itself a few times a day, and shows a banner when an update is ready.</div>
      </div>

      <div class="about muted small">
        CakeVPN ${esc(state.appVersion)}${helperVersion ? ` · helper ${esc(helperVersion)}` : ""}
      </div>
    </main>`;

  $("#back")!.addEventListener("click", closeSettings);
  $("#set-autostart")!.addEventListener("change", async (e) => {
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
  $("#set-autoconnect")!.addEventListener("change", (e) => {
    state.autoConnect = (e.target as HTMLInputElement).checked;
    saved.set("autoConnect", state.autoConnect ? "1" : "0");
  });
  $("#theme")!.addEventListener("click", (e) => {
    const button = (e.target as HTMLElement).closest<HTMLButtonElement>(".seg");
    if (!button) return;
    state.theme = button.dataset.theme as Theme;
    saved.set("theme", state.theme);
    applyTheme();
    app.querySelectorAll(".seg").forEach((el) => el.classList.toggle("active", el === button));
  });
  $("#set-ads")!.addEventListener("change", (e) => {
    state.options.blockAds = (e.target as HTMLInputElement).checked;
    saveOptions();
  });
  $("#set-kill")!.addEventListener("change", (e) => {
    state.options.killSwitch = (e.target as HTMLInputElement).checked;
    saveOptions();
  });
  $("#add-domain")!.addEventListener("submit", (e) => {
    e.preventDefault();
    addSkipped("domain", ($("#new-domain") as HTMLInputElement).value);
  });
  $("#add-app")!.addEventListener("submit", (e) => {
    e.preventDefault();
    addSkipped("app", ($("#new-app") as HTMLInputElement).value);
  });
  app.querySelectorAll<HTMLButtonElement>(".skip-remove").forEach((button) =>
    button.addEventListener("click", () => removeSkipped(button.dataset.kind as "domain" | "app", button.dataset.value!)),
  );
  $("#reconnect")?.addEventListener("click", reconnect);
  wireUsageChart();
  $("#sign-out")?.addEventListener("click", signOut);
  $("#make-invite")?.addEventListener("click", makeInvite);
  $("#settings-update")?.addEventListener("click", installUpdate);
  $("#check-update")?.addEventListener("click", async () => {
    state.updateMessage = "Checking…";
    render();
    await checkForUpdate(true);
    render();
  });
  app.querySelectorAll<HTMLButtonElement>(".copy-code").forEach((button) =>
    button.addEventListener("click", () => copyCode(button)),
  );
  app.querySelectorAll<HTMLButtonElement>(".delete-code").forEach((button) =>
    button.addEventListener("click", () => deleteInvite(button.dataset.code!)),
  );
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
  const card = $("#usage-card");
  if (card && state.screen === "settings") {
    card.innerHTML = usageChartHtml();
    wireUsageChart();
  }
}

function dayLabel(day: string, long = false): string {
  const date = new Date(`${day}T00:00:00`);
  return date.toLocaleDateString(undefined, long ? { weekday: "short", day: "numeric", month: "short" } : { day: "numeric", month: "short" });
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
    const card = $("#usage-card");
    if (card) {
      card.innerHTML = usageChartHtml();
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
      <div class="section-label" id="invites">Invite friends</div>
      <div class="card invites">
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
  setScreen("settings");
  // The usage graph is only asked for while Settings is open.
  if (state.account) loadHistory();
  if (toInvites) $("#invites")?.scrollIntoView({ behavior: "smooth", block: "start" });
}

async function checkForUpdate(fromButton = false) {
  state.last.update = Date.now();
  try {
    const found = await backend.checkUpdate();
    state.update = found ? { version: found.version } : null;
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
  const fill = $("#settings-update-fill") as HTMLElement | null;
  if (fill) fill.style.width = `${Math.round((state.updateProgress ?? 0) * 100)}%`;
  const button = $("#settings-update");
  if (button) button.textContent = updatingText();
}

async function installUpdate() {
  state.updating = true;
  state.updateMessage = "";
  state.updateProgress = null;
  if (state.screen === "settings") render();
  else updateHome();
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
  } catch (e) {
    clearInterval(watch);
    state.updating = false;
    state.updateMessage = asApiError(e).message;
    state.actionError = state.screen === "home" ? state.updateMessage : state.actionError;
    if (state.screen === "settings") render();
    else updateHome();
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
  $("#invites")?.scrollIntoView({ block: "start" });
}

let confirmTimer: number | undefined;

/** The first press asks "Delete?"; a second press within 4 seconds deletes. */
async function deleteInvite(code: string) {
  clearTimeout(confirmTimer);
  if (state.confirmDelete !== code) {
    state.confirmDelete = code;
    render();
    $("#invites")?.scrollIntoView({ block: "start" });
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
  $("#invites")?.scrollIntoView({ block: "start" });
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
  setScreen(state.account ? "home" : "code");
}

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
      state.codeError = `That code is not valid. ${err.triesLeft} ${err.triesLeft === 1 ? "try" : "tries"} left.`;
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
    applied.blockAds && "Ads blocked",
    applied.killSwitch && "Kill switch on",
    skipped > 0 && `${skipped} skip${skipped === 1 ? "s" : ""} the VPN`,
  ]
    .filter(Boolean)
    .join(" · ");
}

function formatMbps(mbps: number): string {
  return `${mbps >= 100 ? Math.round(mbps) : mbps.toFixed(1)} Mbps`;
}

function closeAnnouncement() {
  const id = state.account?.announcement?.id ?? 0;
  state.closedAnnouncement = id;
  saved.set("closedAnnouncement", String(id));
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
  state.busy = true;
  state.actionError = "";
  state.moveNotice = "";
  updateHome();
  try {
    if (tstate === "connected" || tstate === "connecting") {
      await backend.disconnect();
    } else {
      let loc = chosenLocation();
      if (!loc) throw { message: "No location is online right now." };
      const quieter = overloaded(loc) ? quieterLocation(loc) : undefined;
      if (quieter) {
        state.moveNotice = moveMessage(loc, quieter);
        followMove(quieter);
        loc = quieter;
        updateLocations();
      }
      await connectTo(loc.id);
    }
  } catch (e) {
    const err = asApiError(e);
    if (err.message === "helper_missing") state.screen = "setup";
    else state.actionError = err.message;
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
  state.pickerOpen = false;
  $("#picker")?.classList.remove("open");
  $("#toggle-picker")?.classList.remove("open");
  updateLocations();
  // Switching while connected moves the tunnel right away.
  if (tunnelState() === "connected") {
    const loc = chosenLocation();
    if (loc && loc.id !== state.overview?.locationId) {
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
    state.overview = ov;
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
    if (ov.helper === "ok" && state.screen === "setup") setScreen(state.account ? "home" : "code");
  } catch {
    /* keep the last view; the next poll tries again */
  }
}

async function refreshAccount() {
  if (!state.account) return;
  state.lastRefresh = Date.now();
  try {
    state.account = await backend.refreshAccount();
    state.offline = false;
    updateLocations();
    updateHome();
    await leaveOverloadedLocation();
  } catch (e) {
    const err = asApiError(e);
    if (err.error === "offline" && !state.offline) {
      state.offline = true;
      updateHome();
    }
    if (err.error === "signed_out") handleSignedOut("You were signed out because this code was used on another device.");
    if (err.error === "code_disabled") handleSignedOut("This code has been turned off. Ask for a new one.");
  }
}

/**
 * Moves the tunnel off a location that stays above OVERLOAD_PERCENT for 20
 * seconds, when a quieter one is up. It moves at most once every 10 minutes
 * so people don't bounce around.
 */
async function leaveOverloadedLocation() {
  const current = (state.account?.locations ?? []).find((l) => l.id === state.overview?.locationId);
  if (tunnelState() !== "connected" || !overloaded(current)) {
    state.overloadSince = 0;
    return;
  }
  const now = Date.now();
  if (!state.overloadSince) state.overloadSince = now;
  if (state.busy || now - state.overloadSince < 20_000 || now - state.movedAt < 10 * 60 * 1000) return;
  const target = quieterLocation(current);
  if (!current || !target) return;
  state.overloadSince = 0;
  state.movedAt = Date.now();
  followMove(target);
  state.busy = true;
  updateLocations();
  updateHome();
  try {
    await connectTo(target.id);
    state.moveNotice = moveMessage(current, target);
  } catch (e) {
    state.actionError = asApiError(e).message;
  }
  state.busy = false;
  await refreshOverview();
  updateLocations();
  updateHome();
}

async function refreshPings() {
  if (!state.account || tunnelState() === "connected" || tunnelState() === "connecting") return;
  try {
    state.pings = { ...state.pings, ...(await backend.pingLocations()) };
    saved.set("pings", JSON.stringify(state.pings));
    updateLocations();
  } catch {
    /* pings are only a hint */
  }
}

async function start() {
  applyTheme();
  render();
  try {
    const session = await backend.loadSession();
    state.account = session.account;
    state.offline = !!session.offline;
    state.lastRefresh = Date.now();
    state.screen = session.signedIn && session.account ? "home" : "code";
  } catch (e) {
    const err = asApiError(e);
    state.screen = "code";
    if (err.error === "signed_out") state.codeNotice = "You were signed out because this code was used on another device.";
    else if (err.error === "code_disabled") state.codeNotice = "This code has been turned off. Ask for a new one.";
    else state.codeNotice = err.message;
  }
  await refreshOverview();
  if (state.overview && state.overview.helper !== "ok") state.screen = "setup";
  render();
  await refreshPings();

  if (state.autoConnect && state.screen === "home" && tunnelState() === "disconnected") togglePower();
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
 * front (so "Speed now" moves smoothly), less while another app is in front
 * of it, rarely in the tray.
 */
const PACE = {
  // A little under a second, so a timer that fires a moment early doesn't skip a turn.
  overview: { inFront: 900, visible: 3_000, hidden: 10_000 },
  account: { visible: 10_000, hiddenConnected: 60_000, hidden: 5 * 60_000, unreachable: 30_000 },
  pings: 30_000,
  update: 3 * 60 * 60_000,
  updateOnReturn: 30 * 60_000,
};

let ticking = false;

/**
 * Runs every second and does whatever is due. One ticker instead of several
 * timers keeps the app quiet in the tray: nothing but a status check every 10
 * seconds there, and the account once a minute while connected.
 */
async function tick(returned = false) {
  if (ticking) return;
  ticking = true;
  try {
    const now = Date.now();
    let visible = windowVisible() || returned;
    const overviewEvery = !visible ? PACE.overview.hidden : windowInFront() || returned ? PACE.overview.inFront : PACE.overview.visible;
    if (now - state.last.overview >= overviewEvery || returned) {
      state.last.overview = now;
      await refreshOverview();
      visible = windowVisible();
      if (state.screen === "code" && state.lockedUntil > 0) {
        if (state.lockedUntil <= Date.now()) {
          state.lockedUntil = 0;
          render();
        } else {
          updateCodeMessage();
        }
      }
    }
    // The connected timer counts every second, but only while someone can see it.
    if (visible && state.screen === "home") updateHome();

    const connected = tunnelState() === "connected";
    let accountEvery = visible ? PACE.account.visible : connected ? PACE.account.hiddenConnected : PACE.account.hidden;
    // A server that can't be reached (and no VPN to reach it through) is asked less often.
    if (state.offline && !connected) accountEvery = Math.max(accountEvery, PACE.account.unreachable);
    if (now - state.lastRefresh >= accountEvery) await refreshAccount();

    if (visible && now - state.last.pings >= PACE.pings) {
      state.last.pings = now;
      await refreshPings();
    }
    if (now - state.last.update >= PACE.update || (returned && now - state.last.update >= PACE.updateOnReturn)) {
      await checkForUpdate();
    }
  } finally {
    ticking = false;
  }
}

start();
