// The app's languages. The screens are written in English; a watcher puts
// each English text on the page into the chosen language as soon as it
// appears, so the code that draws the screens stays as it is. Texts that
// never reach the page (the tray menu, notifications) go through t().

import { TRANSLATIONS } from "./translations";

export const LANGUAGES: { code: string; name: string }[] = [
  { code: "en", name: "English" },
  { code: "fr", name: "Français" },
  { code: "es", name: "Español" },
  { code: "it", name: "Italiano" },
  { code: "de", name: "Deutsch" },
  { code: "ko", name: "한국어" },
];

/** A text with {placeholders}, matched against whole texts on the page. */
interface Pattern {
  match: RegExp;
  names: string[];
  out: string;
}

let lang = "en";
let exact = new Map<string, string>();
let patterns: Pattern[] = [];

/** The language to use: the one chosen, or the computer's when that is "auto" and known. */
export function pickLanguage(chosen: string): string {
  if (chosen !== "auto" && LANGUAGES.some((l) => l.code === chosen)) return chosen;
  const wanted = (navigator.languages?.length ? navigator.languages : [navigator.language]).map((l) => l.slice(0, 2).toLowerCase());
  return wanted.find((w) => LANGUAGES.some((l) => l.code === w)) ?? "en";
}

export function setLanguage(code: string) {
  lang = code;
  document.documentElement.lang = code;
  const table = TRANSLATIONS[code] ?? {};
  exact = new Map();
  patterns = [];
  for (const [english, translated] of Object.entries(table)) {
    if (!english.includes("{")) {
      exact.set(english, translated);
      continue;
    }
    const names: string[] = [];
    const source = english
      .split(/(\{\w+\})/)
      .map((part) => {
        const name = part.match(/^\{(\w+)\}$/)?.[1];
        if (name) {
          names.push(name);
          return "(.+?)";
        }
        return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      })
      .join("");
    patterns.push({ match: new RegExp(`^${source}$`, "s"), names, out: translated });
  }
}

export function currentLanguage(): string {
  return lang;
}

/** The locale for dates. */
export function locale(): string {
  return lang;
}

function fill(text: string, values: Record<string, string | number>): string {
  return text.replace(/\{(\w+)\}/g, (all, name) => (name in values ? String(values[name]) : all));
}

/** A text in the chosen language, with {placeholders} filled in. */
export function t(english: string, values: Record<string, string | number> = {}): string {
  return fill(translate(english) ?? english, values);
}

/** The translation of a whole text, or null when there is none. */
function translate(text: string): string | null {
  if (lang === "en") return null;
  const hit = exact.get(text);
  if (hit !== undefined) return hit;
  for (const p of patterns) {
    const m = text.match(p.match);
    if (!m) continue;
    const values: Record<string, string> = {};
    // A value can itself be a text with a translation (a warning inside a sentence).
    p.names.forEach((name, i) => (values[name] = translate(m[i + 1]) ?? m[i + 1]));
    return fill(p.out, values);
  }
  return null;
}

/** Translates one piece of text, keeping the spaces around it. */
function translateNodeText(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed || !/[A-Za-z]/.test(trimmed)) return null;
  const done = translate(trimmed);
  return done === null || done === trimmed ? null : text.replace(trimmed, done);
}

const ATTRIBUTES = ["placeholder", "title", "aria-label", "data-tip"];

function translateElement(el: Element) {
  for (const name of ATTRIBUTES) {
    const value = el.getAttribute(name);
    if (!value) continue;
    const done = translateNodeText(value);
    if (done !== null) el.setAttribute(name, done);
  }
}

/** Names of places, networks and codes are marked data-keep and stay as they are. */
function kept(node: Node): boolean {
  const el = node.nodeType === Node.ELEMENT_NODE ? (node as Element) : node.parentElement;
  return !!el?.closest("[data-keep]");
}

function translateText(node: Node) {
  if (kept(node)) return;
  const done = translateNodeText(node.nodeValue ?? "");
  if (done !== null) node.nodeValue = done;
}

function translateTree(root: Node) {
  if (root.nodeType === Node.TEXT_NODE) return translateText(root);
  if (root.nodeType !== Node.ELEMENT_NODE || kept(root)) return;
  translateElement(root as Element);
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node.nodeType === Node.TEXT_NODE) translateText(node);
    else if (!kept(node)) translateElement(node as Element);
  }
}

let watcher: MutationObserver | null = null;

/** Keeps everything under root in the chosen language, now and as the page changes. */
export function watch(root: HTMLElement) {
  watcher?.disconnect();
  translateTree(root);
  if (lang === "en") return;
  watcher = new MutationObserver((records) => {
    for (const r of records) {
      if (r.type === "characterData") translateTree(r.target);
      else if (r.type === "attributes") {
        if (!kept(r.target)) translateElement(r.target as Element);
      }
      else r.addedNodes.forEach((n) => translateTree(n));
    }
  });
  watcher.observe(root, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ATTRIBUTES });
}
