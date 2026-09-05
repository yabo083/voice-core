// Transient feedback: the one component in this app that says something happened.
//
// Every write the 设置 form lands raises one here, and so does every command that can
// reject: register_pack / remove_pack (config.json unwritable), provision (already
// running, or a usage error), start_stack / stop_stack (exe missing), open_path (outside
// the tree). One mechanism, so a person learns where this app talks to them once.
//
// Nothing that belongs on a screen goes here. A failed provision stage renders in the
// stage list with its remedy, because that is state, not an event - and a value a control
// refused renders under that control in `.form__error`, because the answer to "why is this
// red" has to be beside the thing that is red.

import { el } from "./dom";
import { icon } from "./icons";

export type ToastTone = "ok" | "fail" | "info";

/** How long each tone stays.
 *
 *  An acknowledgement is read at a glance and is then in the way, which is why 已保存 goes
 *  as fast as the badge it replaced did. A failure is read, re-read and acted on. */
const HOLD_MS: Record<ToastTone, number> = { ok: 2400, info: 6000, fail: 12000 };

/** How many are on screen at once.
 *
 *  The form raises one per write, and a stack taller than this stops being a notification
 *  and becomes a panel over the controls the user is still using. The oldest goes first. */
const MAX = 3;

let host: HTMLElement | null = null;

/** What is on screen, oldest first - a Map iterates in insertion order - keyed by tone and
 *  message, which is what makes a repeat a repeat. */
const shown = new Map<string, { node: HTMLElement; timer: number }>();

export function toast(message: string, tone: ToastTone = "info"): void {
  if (host === null) {
    // One persistent live region rather than a fresh one per message: assistive
    // tech only announces insertions into a region it was already watching.
    host = el("div", { class: "toasts", "aria-live": "polite", "aria-atomic": "false" });
    document.body.appendChild(host);
  }

  // A tab is not part of any message, so it cannot collide with one.
  const key = `${tone}\t${message}`;
  const live = shown.get(key);
  if (live !== undefined) {
    // The same thing happening again is the same notification. Dragging a stepper from 6 to
    // 12 writes twelve times and says 已保存 once, staying exactly as long after the last
    // write as it would have after a single one.
    window.clearTimeout(live.timer);
    live.timer = window.setTimeout(() => dismiss(key), HOLD_MS[tone]);
    return;
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
        onclick: () => dismiss(key),
      },
      icon("x"),
    ),
  );

  host.appendChild(item);
  shown.set(key, { node: item, timer: window.setTimeout(() => dismiss(key), HOLD_MS[tone]) });
  for (const old of [...shown.keys()].slice(0, shown.size - MAX)) dismiss(old);
}

function dismiss(key: string): void {
  const live = shown.get(key);
  if (live === undefined) return;
  window.clearTimeout(live.timer);
  live.node.remove();
  shown.delete(key);
}
