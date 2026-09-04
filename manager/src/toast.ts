// Transient feedback for the handful of commands that can reject: register_pack /
// remove_pack (config.json unwritable), provision (already running, or a usage
// error), start_stack / stop_stack (exe missing), open_path (outside the tree).
//
// Nothing that belongs on a screen goes here. A failed provision stage renders in
// the stage list with its remedy, because that is state, not an event.

import { el } from "./dom";
import { icon } from "./icons";

export type ToastTone = "ok" | "fail" | "info";

const HOLD_MS: Record<ToastTone, number> = { ok: 4000, info: 6000, fail: 12000 };

let host: HTMLElement | null = null;

export function toast(message: string, tone: ToastTone = "info"): void {
  if (host === null) {
    // One persistent live region rather than a fresh one per message: assistive
    // tech only announces insertions into a region it was already watching.
    host = el("div", { class: "toasts", "aria-live": "polite", "aria-atomic": "false" });
    document.body.appendChild(host);
  }

  const glyph = tone === "ok" ? "check-circle" : tone === "fail" ? "warning-circle" : "info";
  const item = el(
    "div",
    { class: `toast toast--${tone}`, role: tone === "fail" ? "alert" : "status" },
    icon(glyph, "toast__icon"),
    el("p", { class: "toast__text", text: message }),
    el(
      "button",
      {
        class: "btn btn--quiet btn--icon toast__close",
        type: "button",
        "aria-label": "关闭提示",
        onclick: () => item.remove(),
      },
      icon("x"),
    ),
  );

  host.appendChild(item);
  window.setTimeout(() => item.remove(), HOLD_MS[tone]);
}
