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

function stored(): Lang {
  return localStorage.getItem(KEY) === "en" ? "en" : "zh";
}

/** The language this window was opened in. Module-bound: a change reloads the window. */
export const currentLang: Lang = stored();

/** Save the choice and restart the UI. The backend holds no language state, so this is
 *  the only action a language picker has to perform. */
export function setLang(next: Lang): void {
  if (next === currentLang) return;
  localStorage.setItem(KEY, next);
  window.location.reload();
}

/** What a language picker shows: the native name, never translated. */
export function languages(): { id: Lang; label: string }[] {
  return [
    { id: "zh", label: "中文" },
    { id: "en", label: "English" },
  ];
}

/** The strings this window renders with. */
export const t: Dict = DICTS[currentLang];

export type { Dict };
