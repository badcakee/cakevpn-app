// The home screen's world map: every country, drawn once, and a view of it
// that can be zoomed and dragged. The pins on it come from main.ts.

import { WORLD_COUNTRIES, WORLD_FINE, WORLD_HEIGHT, WORLD_WIDTH } from "./worldmap";

/** How far in the map goes: 1 shows the whole world. */
const MAX_ZOOM = 8;
/** Room kept between the whole world and the card's edge, in pixels. */
const EDGE = 12;
/** A press that moves less than this many pixels is a click, not a drag. */
const DRAG_FROM = 4;

/** What is shown: how far in, and the map point in the middle of the card. */
const view = { zoom: 1, cx: WORLD_WIDTH / 2, cy: WORLD_HEIGHT / 2 };
/** The card's size in pixels, read when it is laid out. */
let size = { w: 0, h: 0 };
/** True while the view is where CakeVPN put it, false once the person moved it. */
let automatic = true;
/** Where the locations are, for the view that shows them all. */
let places: { x: number; y: number }[] = [];
/** False until the map has been shown once: it then opens on the locations. */
let opened = false;
let flight = 0;
/** Set by a drag, so the click that ends it doesn't pick a location. */
let dragged = false;

const svg = () => document.querySelector<SVGSVGElement>("#world");

/** The map's markup. The countries are drawn once; only the view's transform changes after that. */
export function mapHtml(pins: string): string {
  const countries = WORLD_COUNTRIES.map(([code, path]) => `<path class="country" data-cc="${code}" d="${path}"></path>`).join("");
  return `
    <svg class="world" id="world" role="img" aria-label="Map of the CakeVPN locations. Scroll to zoom, drag to move.">
      <g id="map-view">
        <g id="map-countries" transform="scale(${1 / WORLD_FINE})">${countries}</g>
        <g id="map-pins">${pins}</g>
      </g>
    </svg>
    <div class="map-zoom">
      <button id="zoom-in" title="Zoom in" aria-label="Zoom in">+</button>
      <button id="zoom-out" title="Zoom out" aria-label="Zoom out">−</button>
      <button id="zoom-fit" title="Show all locations" aria-label="Show all locations">
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" d="M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4"/></svg>
      </button>
    </div>`;
}

/** Pixels per map unit when the whole world is shown. */
function fit(): number {
  return Math.max(0.05, Math.min((size.w - 2 * EDGE) / WORLD_WIDTH, (size.h - 2 * EDGE) / WORLD_HEIGHT));
}

/**
 * The view that shows every location: as close as fits them all with room
 * for their names, and no closer than 3 times. The whole world when there
 * are no locations.
 */
function overview(): { zoom: number; cx: number; cy: number } {
  if (!places.length || !size.w) return { zoom: 1, cx: WORLD_WIDTH / 2, cy: WORLD_HEIGHT / 2 };
  const xs = places.map((p) => p.x);
  const ys = places.map((p) => p.y);
  const [left, right, top, bottom] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
  const across = (size.w - 300) / Math.max(1e-6, (right - left) * fit());
  const down = (size.h - 150) / Math.max(1e-6, (bottom - top) * fit());
  const zoom = Math.min(3, Math.max(1, Math.min(across, down)));
  // Names are written to the right of their pins, so the pins sit a little left of the middle.
  return { zoom, cx: (left + right) / 2 + 50 / (fit() * zoom), cy: (top + bottom) / 2 };
}

/** Tells the map where the locations are. */
export function setPlaces(list: { x: number; y: number }[]) {
  places = list;
}

/** Keeps the map inside the card: no empty strip beside it while it is larger than the card. */
function clamp() {
  view.zoom = Math.min(MAX_ZOOM, Math.max(1, view.zoom));
  const scale = fit() * view.zoom;
  const within = (center: number, length: number, shown: number) => {
    const half = shown / (2 * scale);
    const edge = EDGE / scale;
    return length + 2 * edge <= 2 * half ? length / 2 : Math.min(length + edge - half, Math.max(half - edge, center));
  };
  view.cx = within(view.cx, WORLD_WIDTH, size.w);
  view.cy = within(view.cy, WORLD_HEIGHT, size.h);
}

/** Moves the drawing to the view, and keeps every pin the same size on screen. */
export function drawMap() {
  const el = svg();
  if (!el || !size.w) return;
  clamp();
  const scale = fit() * view.zoom;
  const tx = size.w / 2 - view.cx * scale;
  const ty = size.h / 2 - view.cy * scale;
  el.querySelector("#map-view")!.setAttribute("transform", `translate(${tx.toFixed(2)} ${ty.toFixed(2)}) scale(${scale.toFixed(4)})`);
  const pins = [...el.querySelectorAll<SVGGElement>(".pin")];
  const spots = pins.map((pin) => ({ pin, x: tx + Number(pin.dataset.x) * scale, y: ty + Number(pin.dataset.y) * scale }));
  pins.forEach((pin) => {
    const x = Number(pin.dataset.x);
    pin.setAttribute("transform", `translate(${x} ${pin.dataset.y}) scale(${(1 / scale).toFixed(5)})`);
    // The name goes to the right, unless the card ends there or another pin is in its way.
    const px = tx + x * scale;
    const py = ty + Number(pin.dataset.y) * scale;
    const taken = (from: number, to: number) =>
      spots.some((s) => s.pin !== pin && s.x > px + from && s.x < px + to && Math.abs(s.y - py) < 20);
    // A pin outside the card keeps its name to itself, or the name's end would show at the edge.
    pin.classList.toggle("outside", px < 0 || px > size.w || py < 0 || py > size.h);
    const left = px > size.w - 170 || (taken(6, 150) && !taken(-150, -6) && px > 170);
    if (pin.classList.contains("left") !== left) {
      pin.classList.toggle("left", left);
      pin.querySelector("text")?.setAttribute("text-anchor", left ? "end" : "start");
      pin.querySelectorAll("tspan").forEach((line) => line.setAttribute("x", left ? "-12" : "12"));
    }
  });
  const zoomIn = document.querySelector<HTMLButtonElement>("#zoom-in");
  const zoomOut = document.querySelector<HTMLButtonElement>("#zoom-out");
  if (zoomIn) zoomIn.disabled = view.zoom >= MAX_ZOOM - 0.001;
  if (zoomOut) zoomOut.disabled = view.zoom <= 1.001;
}

/** Zooms by `factor`, keeping the map point under (px, py) where it is. */
function zoomAt(px: number, py: number, factor: number) {
  const before = fit() * view.zoom;
  const mx = view.cx + (px - size.w / 2) / before;
  const my = view.cy + (py - size.h / 2) / before;
  view.zoom = Math.min(MAX_ZOOM, Math.max(1, view.zoom * factor));
  const after = fit() * view.zoom;
  view.cx = mx - (px - size.w / 2) / after;
  view.cy = my - (py - size.h / 2) / after;
  drawMap();
}

/** Glides to another view. */
function glide(to: { zoom: number; cx: number; cy: number }, ms = 450) {
  const from = { ...view };
  const started = performance.now();
  const mine = ++flight;
  const step = (now: number) => {
    if (mine !== flight) return;
    const t = Math.min(1, (now - started) / ms);
    const eased = t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2;
    // Zoom is felt in steps of "twice as close", so it is glided on that scale.
    view.zoom = from.zoom * (to.zoom / from.zoom) ** eased;
    view.cx = from.cx + (to.cx - from.cx) * eased;
    view.cy = from.cy + (to.cy - from.cy) * eased;
    drawMap();
    if (t < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

/** The person took the view over: a glide in progress stops. */
function takeOver() {
  flight++;
  automatic = false;
}

/**
 * Shows a place up close (a location the VPN connected to), or all the
 * locations when `place` is null. `onlyIfAutomatic` leaves a view alone that
 * the person chose themselves.
 */
export function showPlace(place: { x: number; y: number } | null, options: { glide?: boolean; onlyIfAutomatic?: boolean } = {}) {
  if (options.onlyIfAutomatic && !automatic) return;
  const to = place ? { zoom: Math.max(view.zoom, 3), cx: place.x, cy: place.y } : overview();
  automatic = true;
  if (options.glide === false || !size.w) {
    flight++;
    Object.assign(view, to);
    drawMap();
  } else {
    glide(to);
  }
}

/** Colors the countries of the chosen and the connected location. */
export function markCountries(chosen: string | undefined, connected: string | undefined) {
  const countries = document.querySelector("#map-countries");
  if (!countries) return;
  countries.querySelectorAll(".chosen, .connected").forEach((c) => c.classList.remove("chosen", "connected"));
  const mark = (code: string | undefined, name: string) => {
    if (!code || !/^[A-Za-z]{2}$/.test(code)) return;
    countries.querySelectorAll(`[data-cc="${code.toUpperCase()}"]`).forEach((c) => c.classList.add(name));
  };
  mark(chosen, "chosen");
  mark(connected, "connected");
}

/** Makes the map on the page zoom and move, and reports a pin that was clicked. */
export function attachMap(pick: (locationId: string) => void) {
  const el = svg();
  const card = el?.parentElement;
  if (!el || !card) return;

  const measure = () => {
    const box = el.getBoundingClientRect();
    size = { w: box.width, h: box.height };
    if (!opened && size.w) {
      opened = true;
      Object.assign(view, overview());
    }
    drawMap();
  };
  measure();
  new ResizeObserver(measure).observe(el);
  const at = (e: { clientX: number; clientY: number }) => {
    const box = el.getBoundingClientRect();
    return { x: e.clientX - box.left, y: e.clientY - box.top };
  };

  // Scrolling zooms where the pointer is. A pinch on a trackpad arrives as a
  // scroll with Ctrl held, in finer steps.
  card.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      takeOver();
      const lines = e.deltaMode === 1 ? 16 : 1;
      const p = at(e);
      zoomAt(p.x, p.y, Math.exp(-e.deltaY * lines * (e.ctrlKey ? 0.012 : 0.0022)));
    },
    { passive: false },
  );

  // Safari's own pinch events (the Mac app).
  let pinchFrom = 1;
  el.addEventListener("gesturestart", (e) => {
    e.preventDefault();
    takeOver();
    pinchFrom = view.zoom;
  });
  el.addEventListener("gesturechange", (e) => {
    e.preventDefault();
    const g = e as Event & { scale: number; clientX: number; clientY: number };
    const p = at(g);
    zoomAt(p.x, p.y, (pinchFrom * g.scale) / view.zoom);
  });

  // Dragging moves the map. The pointer is only held once it really moved,
  // so a plain click still reaches the pin under it.
  let press: { id: number; x: number; y: number; cx: number; cy: number } | null = null;
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    press = { id: e.pointerId, x: e.clientX, y: e.clientY, cx: view.cx, cy: view.cy };
    dragged = false;
  });
  el.addEventListener("pointermove", (e) => {
    if (!press || e.pointerId !== press.id) return;
    const dx = e.clientX - press.x;
    const dy = e.clientY - press.y;
    if (!dragged) {
      if (Math.hypot(dx, dy) < DRAG_FROM) return;
      dragged = true;
      takeOver();
      el.setPointerCapture(e.pointerId);
      card.classList.add("dragging");
    }
    const scale = fit() * view.zoom;
    view.cx = press.cx - dx / scale;
    view.cy = press.cy - dy / scale;
    drawMap();
  });
  const release = (e: PointerEvent) => {
    if (!press || e.pointerId !== press.id) return;
    press = null;
    card.classList.remove("dragging");
    // The click that follows a drag is not a choice; after it, clicks count again.
    if (dragged) setTimeout(() => (dragged = false), 0);
  };
  el.addEventListener("pointerup", release);
  el.addEventListener("pointercancel", release);

  el.addEventListener("click", (e) => {
    if (dragged) return;
    const pin = (e.target as Element).closest<SVGGElement>(".pin");
    if (pin && !pin.classList.contains("offline")) pick(pin.dataset.loc!);
  });
  el.addEventListener("dblclick", (e) => {
    if ((e.target as Element).closest(".pin")) return;
    takeOver();
    const p = at(e);
    const factor = Math.min(2, MAX_ZOOM / view.zoom);
    const scale = fit() * view.zoom;
    glide({ zoom: view.zoom * factor, cx: view.cx + ((p.x - size.w / 2) / scale) * (1 - 1 / factor), cy: view.cy + ((p.y - size.h / 2) / scale) * (1 - 1 / factor) }, 250);
  });

  const button = (id: string, act: () => void) =>
    document.querySelector(id)?.addEventListener("click", () => {
      takeOver();
      act();
    });
  button("#zoom-in", () => glide({ ...view, zoom: Math.min(MAX_ZOOM, view.zoom * 1.7) }, 220));
  button("#zoom-out", () => glide({ ...view, zoom: Math.max(1, view.zoom / 1.7) }, 220));
  button("#zoom-fit", () => showPlace(null));
}
