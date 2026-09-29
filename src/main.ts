import { Account, asApiError, backend, Location, Overview } from "./backend";

// ---------- state ----------

type Screen = "loading" | "code" | "home" | "setup";

const state = {
  screen: "loading" as Screen,
  account: null as Account | null,
  overview: null as Overview | null,
  pings: {} as Record<string, number | null>,
  /** "best" or a location id; remembered between runs. */
  choice: localStorage.getItem("location") || "best",
  pickerOpen: false,
  busy: false,
  codeError: "",
  codeNotice: "",
  lockedUntil: 0,
  actionError: "",
  lastBytes: null as { at: number; up: number; down: number } | null,
  speed: { up: 0, down: 0 },
};

const app = document.getElementById("app")!;
const isWindows = navigator.userAgent.includes("Windows");

// ---------- helpers ----------

function esc(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
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
  return p.mbps > 0 ? `${p.name} · ${p.mbps} Mbps` : `${p.name} · Max speed`;
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

// ---------- rendering ----------

function render() {
  if (state.screen === "loading") {
    app.innerHTML = `<div class="center"><div class="logo big">🍰</div><div class="muted">Loading…</div></div>`;
  } else if (state.screen === "code") {
    renderCode();
  } else if (state.screen === "setup") {
    renderSetup();
  } else {
    renderHome();
  }
}

function header(right = ""): string {
  return `<header><div class="brand"><span class="logo">🍰</span> CakeVPN</div>${right}</header>`;
}

function renderCode() {
  const locked = state.lockedUntil > Date.now();
  const wait = Math.ceil((state.lockedUntil - Date.now()) / 1000);
  const message = locked
    ? `Too many wrong codes. Try again in ${formatDuration(wait)}.`
    : state.codeError;
  app.innerHTML = `
    ${header()}
    <main class="code-screen">
      <h1>Enter your code</h1>
      <p class="muted">Type the 5-character code you were given.</p>
      ${state.codeNotice ? `<div class="notice">${esc(state.codeNotice)}</div>` : ""}
      <form id="code-form" autocomplete="off">
        <div class="boxes">
          ${[0, 1, 2, 3, 4].map((i) => `<input class="box" data-i="${i}" maxlength="1" inputmode="text" ${locked || state.busy ? "disabled" : ""}>`).join("")}
        </div>
        <div class="error ${message ? "" : "hidden"}" id="code-error">${esc(message)}</div>
        <button class="primary" type="submit" ${locked || state.busy ? "disabled" : ""}>${state.busy ? "Checking…" : "Sign in"}</button>
      </form>
    </main>`;
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
  app.querySelector("#code-form")!.addEventListener("submit", (e) => {
    e.preventDefault();
    const code = boxes.map((b) => b.value).join("");
    if (code.length === 5) submitCode(code);
  });
}

function renderSetup() {
  const outdated = state.overview?.helper === "outdated";
  app.innerHTML = `
    ${header()}
    <main class="center setup">
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
  app.querySelector("#install")?.addEventListener("click", installHelper);
}

function renderHome() {
  const account = state.account!;
  const status = state.overview?.status;
  const tstate = tunnelState();
  const loc = chosenLocation();
  const since = status?.connectedSince ? Date.now() / 1000 - status.connectedSince : 0;

  const buttonText = { disconnected: "Connect", connecting: "Connecting…", connected: "Connected", failed: "Try again" }[tstate];
  const statusLine =
    tstate === "connected"
      ? `Protected · ${formatDuration(since)}`
      : tstate === "connecting"
        ? "Setting up a secure connection…"
        : tstate === "failed"
          ? esc(status?.error || "Could not connect.")
          : "Not connected";

  const banner = state.overview?.banner;
  const locationRow = (l: Location, selected: boolean, label?: string) => {
    const ping = state.pings[l.id];
    const load = l.load;
    return `
      <button class="loc ${selected ? "selected" : ""}" data-loc="${label ? "best" : esc(l.id)}" ${l.online ? "" : "disabled"}>
        ${flag(l.country)}
        <span class="loc-name">${esc(label ?? l.name)}${label ? `<small>${esc(l.name)}</small>` : ""}</span>
        <span class="loc-meta">
          ${load ? `<span class="load ${load.level}" title="Load ${load.percent}%"><i style="width:${Math.max(6, load.percent)}%"></i></span>` : ""}
          <span class="ping">${ping != null ? `${ping} ms` : ""}</span>
        </span>
      </button>`;
  };
  const best = bestLocation();
  const picker = state.pickerOpen
    ? `<div class="picker">
        ${best ? locationRow(best, state.choice === "best", "Best location") : ""}
        ${account.locations.map((l) => locationRow(l, state.choice === l.id)).join("")}
      </div>`
    : "";

  app.innerHTML = `
    ${header(`<span class="plan ${account.plan.id}">${esc(planLabel(account))}</span>`)}
    <main class="home">
      <button id="power" class="power ${tstate}" ${state.busy ? "disabled" : ""}>
        <span class="power-icon">⏻</span>
        <span class="power-text">${buttonText}</span>
      </button>
      <div class="status-line ${tstate}">${statusLine}</div>
      ${state.actionError ? `<div class="error">${esc(state.actionError)}</div>` : ""}
      ${banner ? `<div class="banner ${banner.kind}">${banner.kind === "wifi" ? "📶" : banner.kind === "load" ? "🔥" : "🐢"} ${esc(banner.message)}</div>` : ""}

      <div class="card">
        <div class="card-label">Location</div>
        <button class="loc current" id="toggle-picker">
          ${loc ? flag(loc.country) : ""}
          <span class="loc-name">${loc ? esc(loc.name) : "No location online"}${state.choice === "best" ? "<small>Best location</small>" : ""}</span>
          <span class="chevron">${state.pickerOpen ? "▲" : "▼"}</span>
        </button>
        ${picker}
      </div>

      <div class="card stats">
        <div><div class="card-label">Used this month</div><div class="big-number">${formatBytes(account.usage.bytes)}</div></div>
        <div><div class="card-label">Speed now</div><div class="big-number small">${
          tstate === "connected" ? `↓ ${formatRate(state.speed.down)}<br>↑ ${formatRate(state.speed.up)}` : "—"
        }</div></div>
      </div>
      <div class="muted small center-text">Unlimited traffic on every plan</div>
    </main>
    <footer><button class="link" id="sign-out">Sign out</button></footer>`;

  app.querySelector("#power")!.addEventListener("click", togglePower);
  app.querySelector("#toggle-picker")!.addEventListener("click", () => {
    state.pickerOpen = !state.pickerOpen;
    render();
  });
  app.querySelectorAll<HTMLButtonElement>(".picker .loc").forEach((el) =>
    el.addEventListener("click", () => chooseLocation(el.dataset.loc!)),
  );
  app.querySelector("#sign-out")!.addEventListener("click", signOut);
}

// ---------- actions ----------

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
  render();
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
  render();
}

async function chooseLocation(id: string) {
  state.choice = id;
  localStorage.setItem("location", id);
  state.pickerOpen = false;
  render();
  // Switching while connected moves the tunnel right away.
  if (tunnelState() === "connected") {
    const loc = chosenLocation();
    if (loc && loc.id !== state.overview?.locationId) {
      state.busy = true;
      render();
      try {
        await backend.connect(loc.id);
      } catch (e) {
        state.actionError = asApiError(e).message;
      }
      state.busy = false;
      await refreshOverview();
      render();
    }
  }
}

async function signOut() {
  await backend.signOut();
  state.account = null;
  state.codeNotice = "";
  state.screen = "code";
  render();
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
  state.screen = "code";
  state.codeNotice = message;
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
    if ((ov.helper === "missing" || ov.helper === "outdated") && state.screen === "home") state.screen = "setup";
    if (ov.helper === "ok" && state.screen === "setup") state.screen = state.account ? "home" : "code";
  } catch {
    /* keep the last view; the next poll tries again */
  }
}

async function refreshAccount() {
  if (!state.account) return;
  try {
    state.account = await backend.refreshAccount();
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
  } catch {
    /* pings are only a hint */
  }
}

async function start() {
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
  if (state.overview?.helper !== "ok" && state.overview) state.screen = "setup";
  render();
  refreshPings().then(render);

  setInterval(async () => {
    const before = JSON.stringify([state.overview, state.screen]);
    await refreshOverview();
    // Redraw every tick on the home screen for the timer; elsewhere only on change.
    // The code screen is left alone so typed letters stay put.
    if (state.screen === "home" && !state.pickerOpen) render();
    else if (state.screen === "setup" && before !== JSON.stringify([state.overview, state.screen])) render();
    if (state.screen === "code" && state.lockedUntil > 0) {
      if (state.lockedUntil <= Date.now()) state.lockedUntil = 0;
      render();
    }
  }, 2000);
  setInterval(() => refreshAccount().then(() => state.screen === "home" && !state.pickerOpen && render()), 60000);
  setInterval(() => refreshPings(), 30000);
}

start();
