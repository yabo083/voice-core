// Panel language: which dictionary `t` reads, and where the choice lives.
//
// `zh` is the source of truth (`strings/*.ts` define both languages per domain, with the
// English object typed against the Chinese one), so this module is deliberately tiny:
// pick the language before the first render, expose it as `t`, and on a change reload the
// window. A reload is the whole migration path - every screen is derived from backend
// state on boot and re-rendered from stores, so nothing needs a live re-translation, and
// a module-bound `t` stays a plain property read at every call site.

import { zh, type Dict } from "./strings/zh";
import { en } from "./strings/en";

export type Lang = "zh" | "en";

const DICTS: Record<Lang, Dict> = { zh, en };
const KEY = "vc.lang";
/** Set across one reload, so the next boot knows to draw the same mask it hides behind. */
const TRANSITION = "vc.langTransition";

function stored(): Lang {
  return localStorage.getItem(KEY) === "en" ? "en" : "zh";
}

/** The language this window was opened in. Module-bound: a change reloads the window. */
export const currentLang: Lang = stored();

/** What a language picker shows: the native name, never translated. */
export function languages(): { id: Lang; label: string }[] {
  return [
    { id: "zh", label: "中文" },
    { id: "en", label: "English" },
  ];
}

/** Save the choice and restart the UI behind a blurred mask.
 *
 *  A reload re-renders every screen from scratch, and watching the layout rebuild is the
 *  cheap version of a language switch. The mask fades in over the old window, the reload
 *  happens underneath it, and `settleLanguageTransition` fades the same mask away once
 *  the new window has something to show - so what the user sees is one crossfade. */
export function setLang(next: Lang): void {
  if (next === currentLang) return;
  localStorage.setItem(KEY, next);
  sessionStorage.setItem(TRANSITION, "1");
  const mask = document.createElement("div");
  mask.className = "lang-mask";
  document.body.appendChild(mask);
  window.setTimeout(() => mask.classList.add("is-shown"), 20);
  window.setTimeout(() => window.location.reload(), 280);
}

/** Called once at boot, before the first paint: if the previous window went out behind
 *  the language mask, come back in behind it too. */
export function settleLanguageTransition(): void {
  if (sessionStorage.getItem(TRANSITION) !== "1") return;
  sessionStorage.removeItem(TRANSITION);
  const mask = document.createElement("div");
  mask.className = "lang-mask is-shown";
  document.body.appendChild(mask);
  // A beat in, the screens are mounted and styled; fading from there reads as a reveal,
  // not as a flash of the mask itself.
  window.setTimeout(() => {
    mask.classList.add("is-fading");
    window.setTimeout(() => mask.remove(), 420);
  }, 120);
}

/** The strings this window renders with. */
export const t: Dict = DICTS[currentLang];

export type { Dict };
