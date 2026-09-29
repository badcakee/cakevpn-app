import { Account, asApiError, backend, Location, Overview } from "./backend";

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

const state = {
  screen: "loading" as Screen,
  account: null as Account | null,
  overview: null as Overview | null,
  pings: {} as Record<string, number | null>,
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
  lastBytes: null as { at: number; up: number; down: number } | null,
  speed: { up: 0, down: 0 },
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

function invitesOn(account: Account | null): boolean {
  return !!account?.referral && account.referral.maxFriends > 0;
}

/** Lower is better: a quick answer and a quiet server. */
function score(loc: Location): number {
  const ping = state.pings[loc.id];
  return (ping ?? 250) + (loc.load?.percent ?? 50) * 2;
}

function bestLocation(): Location | undefined {
  const online = (state.account?.locations ?? []).filter((l) => l.online);
  return online.sort((a, b) => score(a) - score(b))[0];
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
  return `<span class="load ${l.load.level}" title="How busy this location is">
      <span class="load-bar"><i style="width:${Math.max(4, l.load.percent)}%"></i></span>
      <span class="load-pct">${l.load.percent}%</span>
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
  const loc = chosenLocation();
  return `${loc ? flag(loc.country) : ""}
    <span class="loc-name">${loc ? esc(loc.name) : "No location online"}${state.choice === "best" ? "<small>Best location</small>" : ""}</span>
    <span class="loc-meta">${loc ? loadBadge(loc) : ""}<span class="chevron">▾</span></span>`;
}

function renderHome() {
  const account = state.account!;
  app.innerHTML = `
    ${header(`<span class="plan ${account.plan.id}">${esc(planLabel(account))}</span>
      <button class="icon-btn" id="open-settings" title="Settings">${GEAR}</button>`)}
    <main class="home screen">
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
      <div class="banner" id="banner"><span class="banner-icon"></span><span class="banner-text"></span></div>

      <div class="card">
        <div class="card-label">Location</div>
        <button class="loc current" id="toggle-picker">${currentLocationHtml()}</button>
        <div class="picker ${state.pickerOpen ? "open" : ""}" id="picker"><div class="picker-inner">${pickerHtml()}</div></div>
      </div>

      <div class="card stats">
        <div><div class="card-label">Used this month</div><div class="big-number" id="usage"></div></div>
        <div><div class="card-label">Speed now</div><div class="big-number small" id="speed"></div></div>
      </div>
      <div class="muted small center-text">Unlimited traffic on every plan</div>
      ${
        invitesOn(account) && account.plan.mbps > 0
          ? `<button class="link invite-link" id="invite-link">🎁 Invite friends · +${account.referral.mbpsPerFriend} Mbps each</button>`
          : ""
      }
    </main>`;

  $("#power")!.addEventListener("click", togglePower);
  $("#open-settings")!.addEventListener("click", () => openSettings());
  $("#invite-link")?.addEventListener("click", () => openSettings(true));
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
        ? "Setting up a secure connection…"
        : tstate === "failed"
          ? status?.error || "Could not connect."
          : "Not connected",
  );

  const error = $("#action-error")!;
  error.classList.toggle("hidden", !state.actionError);
  setText("#action-error", state.actionError);

  const banner = $("#banner")!;
  const b = state.overview?.banner;
  banner.className = `banner ${b ? `show ${b.kind}` : ""}`;
  if (b) {
    setText("#banner .banner-icon", b.kind === "wifi" ? "📶" : b.kind === "load" ? "🔥" : "🐢");
    setText("#banner .banner-text", b.message);
  }

  setText("#usage", formatBytes(state.account.usage.bytes));
  const speed = $("#speed")!;
  const speedHtml = tstate === "connected" ? `↓ ${formatRate(state.speed.down)}<br>↑ ${formatRate(state.speed.up)}` : "—";
  if (speed.innerHTML !== speedHtml) speed.innerHTML = speedHtml;
}

/** Redraws the location parts after the account, pings or choice changed. */
function updateLocations() {
  if (state.screen !== "home") return;
  const current = $("#toggle-picker");
  const inner = $("#picker .picker-inner");
  if (current) current.innerHTML = currentLocationHtml();
  if (inner) inner.innerHTML = pickerHtml();
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
      </div>
      <button class="danger-outline" id="sign-out">Sign out of this device</button>`
          : ""
      }

      <div class="about muted small">CakeVPN ${esc(state.appVersion)}${helperVersion ? ` · helper ${esc(helperVersion)}` : ""}</div>
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
  $("#sign-out")?.addEventListener("click", signOut);
  $("#make-invite")?.addEventListener("click", makeInvite);
  app.querySelectorAll<HTMLButtonElement>(".copy-code").forEach((button) =>
    button.addEventListener("click", () => copyCode(button)),
  );
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
        </div>`,
    )
    .join("");
  return `
      <div class="section-label" id="invites">Invite friends</div>
      <div class="card invites">
        ${
          capped
            ? `<div class="invite-head">
                 <div><b>+${r.bonusMbps} Mbps</b><small>${friends} of ${r.maxFriends} friends joined</small></div>
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
  setScreen("settings");
  if (toInvites) $("#invites")?.scrollIntoView({ behavior: "smooth", block: "start" });
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

async function togglePower() {
  const tstate = tunnelState();
  state.busy = true;
  state.actionError = "";
  updateHome();
  try {
    if (tstate === "connected" || tstate === "connecting") {
      await backend.disconnect();
    } else {
      const loc = chosenLocation();
      if (!loc) throw { message: "No location is online right now." };
      await backend.connect(loc.id);
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
        await backend.connect(loc.id);
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
    if ((ov.helper === "missing" || ov.helper === "outdated") && state.screen === "home") setScreen("setup");
    if (ov.helper === "ok" && state.screen === "setup") setScreen(state.account ? "home" : "code");
  } catch {
    /* keep the last view; the next poll tries again */
  }
}

async function refreshAccount() {
  if (!state.account) return;
  try {
    state.account = await backend.refreshAccount();
    updateLocations();
    updateHome();
  } catch (e) {
    const err = asApiError(e);
    if (err.error === "signed_out") handleSignedOut("You were signed out because this code was used on another device.");
    if (err.error === "code_disabled") handleSignedOut("This code has been turned off. Ask for a new one.");
  }
}

async function refreshPings() {
  if (!state.account || tunnelState() === "connected" || tunnelState() === "connecting") return;
  try {
    state.pings = { ...state.pings, ...(await backend.pingLocations()) };
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

  setInterval(async () => {
    await refreshOverview();
    if (state.screen === "home") updateHome();
    if (state.screen === "code" && state.lockedUntil > 0) {
      if (state.lockedUntil <= Date.now()) {
        state.lockedUntil = 0;
        render();
      } else {
        updateCodeMessage();
      }
    }
  }, 2000);
  // The connected timer ticks every second between the 2-second status polls.
  setInterval(updateHome, 1000);
  // Load changes quickly, so the account (with each location's load) is refreshed often.
  setInterval(refreshAccount, 20000);
  setInterval(refreshPings, 30000);
}

start();
