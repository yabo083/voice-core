// The shared widget vocabulary: buttons, chips, panels, notes, paths, copy rows,
// empty states. Three screens draw from it so a control looks and behaves the same
// wherever it appears, and so the accessible bits (real <button>, label/control
// binding, icon-only buttons carrying a name) cannot be forgotten per screen.

import { el, type Child } from "./dom";
import { shortenPath } from "./format";
import { icon, type IconName } from "./icons";
import { openPath, ipcMessage } from "./ipc";
import { toast } from "./toast";

export type Tone = "ok" | "reuse" | "run" | "fail" | "warn" | "idle" | "accent";
export type ButtonKind = "primary" | "default" | "quiet" | "danger";

export type ScreenName = "deploy" | "voices" | "status";

/** Ask the shell to switch screens.
 *
 *  `ev.detail === 0` means the activation came from the keyboard, and only then does
 *  focus move to the destination's heading: a mouse user who clicks a link and then
 *  sees a focus ring drawn around a title reads it as a rendering bug. */
export function navigate(to: ScreenName, ev?: MouseEvent): void {
  document.dispatchEvent(
    new CustomEvent("app:navigate", { detail: { to, focus: ev !== undefined && ev.detail === 0 } }),
  );
}

export interface ButtonSpec {
  label?: string;
  kind?: ButtonKind;
  glyph?: IconName;
  onClick: (ev: MouseEvent) => void;
  disabled?: boolean;
  small?: boolean;
  title?: string;
  /** Required when there is no visible label. */
  name?: string;
  pressed?: boolean;
  expanded?: boolean;
  controls?: string;
}

export function button(spec: ButtonSpec): HTMLButtonElement {
  const kind = spec.kind ?? "default";
  const classes = ["btn", `btn--${kind}`];
  if (spec.small === true) classes.push("btn--sm");
  if (spec.label === undefined) classes.push("btn--icon");

  return el(
    "button",
    {
      class: classes.join(" "),
      type: "button",
      disabled: spec.disabled === true,
      title: spec.title,
      "aria-label": spec.label === undefined ? spec.name : undefined,
      "aria-pressed": spec.pressed === undefined ? undefined : String(spec.pressed),
      "aria-expanded": spec.expanded === undefined ? undefined : String(spec.expanded),
      "aria-controls": spec.controls,
      onclick: spec.onClick as EventListener,
    },
    spec.glyph === undefined ? null : icon(spec.glyph),
    spec.label === undefined ? null : el("span", { text: spec.label }),
  );
}

/** State always reads as icon + word + colour, never colour alone. */
export function chip(label: string, tone: Tone, glyph?: IconName): HTMLElement {
  return el(
    "span",
    { class: `chip chip--${tone}` },
    glyph === undefined ? null : icon(glyph, "chip__icon"),
    el("span", { text: label }),
  );
}

export interface PanelSpec {
  title: string;
  hint?: string;
  actions?: Child[];
  id?: string;
}

/** Returns the section and its body so a caller can re-render the body alone. */
export function panel(spec: PanelSpec): { root: HTMLElement; body: HTMLElement } {
  const body = el("div", { class: "panel__body" });
  const root = el(
    "section",
    { class: "panel", id: spec.id },
    el(
      "header",
      { class: "panel__head" },
      el(
        "div",
        { class: "panel__titles" },
        el("h2", { class: "panel__title", text: spec.title }),
        spec.hint === undefined ? null : el("p", { class: "panel__hint", text: spec.hint }),
      ),
      spec.actions === undefined ? null : el("div", { class: "panel__actions" }, spec.actions),
    ),
    body,
  );
  return { root, body };
}

export interface ExpanderSpec {
  title: string;
  /** Shown in the head, right-aligned: a summary that makes opening optional. */
  tail?: Child;
  open?: boolean;
  id: string;
}

/** A panel whose head is the disclosure control.
 *
 *  Exists because the alternative - a panel plus a separate toggle button - gives
 *  the user two targets for one idea, and because the head has to stay useful while
 *  closed: the `tail` summary is what lets a collapsed card still answer "is my
 *  environment fine". The head is a real <button> so the keyboard reaches it. */
export function expander(spec: ExpanderSpec): {
  root: HTMLElement;
  head: HTMLButtonElement;
  body: HTMLElement;
  tail: HTMLElement;
  setOpen: (open: boolean) => void;
  isOpen: () => boolean;
} {
  const body = el("div", { class: "panel__body", id: spec.id });
  const tail = el("span", { class: "expander__tail" }, spec.tail);
  const head = el(
    "button",
    {
      class: "expander__head",
      type: "button",
      "aria-expanded": String(spec.open !== false),
      "aria-controls": spec.id,
      onclick: () => setOpen(Boolean(body.hidden)),
    },
    el("span", { class: "expander__title", text: spec.title }),
    tail,
    icon("caret-right", "expander__caret"),
  );

  function setOpen(open: boolean): void {
    body.hidden = !open;
    head.setAttribute("aria-expanded", String(open));
  }
  setOpen(spec.open !== false);

  return {
    root: el("section", { class: "panel expander" }, head, body),
    head,
    body,
    tail,
    setOpen,
    isOpen: () => !body.hidden,
  };
}

/** The screen's primary actions, pinned below the scroll region by the shell.
 *
 *  A wizard whose "start" button scrolls away with the content is a wizard whose
 *  main action is missing. Left slot is secondary, right slot is primary, which is
 *  the order Windows 11's own wizards use. */
export function commandBar(left: Child[], right: Child[]): HTMLElement {
  return el(
    "div",
    { class: "cmdbar" },
    el("div", { class: "cmdbar__left" }, left),
    el("div", { class: "cmdbar__right" }, right),
  );
}

/** A control that cannot be used yet, plus the reason, on hover and on focus.
 *
 *  `aria-disabled` rather than `disabled` is load-bearing: Chromium drops pointer
 *  events on a disabled control, so a genuinely disabled button can never show the
 *  tooltip that explains why it is disabled. This keeps it focusable and inert. */
export function blockedButton(spec: Omit<ButtonSpec, "onClick" | "disabled">, reason: string): HTMLElement {
  const btn = button({ ...spec, onClick: () => undefined });
  btn.setAttribute("aria-disabled", "true");
  btn.removeAttribute("disabled");
  return el("span", { class: "tipwrap" }, el("span", { class: "tip", role: "tooltip", text: reason }), btn);
}

/** Same tooltip, on a control that *is* usable. */
export function withTip(control: HTMLElement, reason: string): HTMLElement {
  return el("span", { class: "tipwrap" }, el("span", { class: "tip", role: "tooltip", text: reason }), control);
}

export function note(
  tone: "info" | "warn" | "fail" | "reuse",
  title: string,
  ...body: Child[]
): HTMLElement {
  const glyph: IconName =
    tone === "fail" ? "warning-circle" : tone === "warn" ? "warning" : tone === "reuse" ? "recycle" : "info";
  return el(
    "div",
    { class: `note note--${tone}` },
    icon(glyph, "note__icon"),
    el("div", { class: "note__body" }, el("p", { class: "note__title", text: title }), body),
  );
}

/** Paths are shown middle-elided with the full string in `title`, and marked LTR
 *  so a Windows path never reorders inside a Chinese sentence. */
export function pathText(path: string, max?: number): HTMLElement {
  return el("code", {
    class: "path",
    dir: "ltr",
    title: path,
    text: shortenPath(path, max),
  });
}

export function openButton(path: string, name = "在资源管理器中打开"): HTMLButtonElement {
  return button({
    glyph: "arrow-square-out",
    name,
    title: name,
    small: true,
    kind: "quiet",
    onClick: () => {
      void openPath(path).catch((err: unknown) => toast(ipcMessage(err), "fail"));
    },
  });
}

async function copyText(value: string, source: HTMLElement, label: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(value);
    toast(`已复制${label}`, "ok");
  } catch {
    // WebView2 refuses the async clipboard when the window is not focused. Selecting
    // the text keeps the user one keystroke from the same result instead of leaving
    // a dead button.
    const range = document.createRange();
    range.selectNodeContents(source);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    toast("剪贴板不可用：文本已选中，请按 Ctrl+C", "fail");
  }
}

export interface CopyRowSpec {
  label: string;
  value: string;
  glyph?: IconName;
  hint?: string;
  /** Multi-line snippets render as a block instead of one line. */
  block?: boolean;
  /** What the toast calls it, e.g. "端点地址". Defaults to the label. */
  what?: string;
}

export function copyRow(spec: CopyRowSpec): HTMLElement {
  const value = el("code", {
    class: spec.block === true ? "code-block" : "path",
    dir: "ltr",
    text: spec.value,
  });
  const what = spec.what ?? spec.label;

  return el(
    "div",
    { class: `copyrow${spec.block === true ? " copyrow--block" : ""}` },
    el(
      "div",
      { class: "copyrow__head" },
      spec.glyph === undefined ? null : icon(spec.glyph, "copyrow__icon"),
      el("span", { class: "copyrow__label", text: spec.label }),
      button({
        glyph: "copy",
        name: `复制${what}`,
        title: `复制${what}`,
        small: true,
        kind: "quiet",
        onClick: () => void copyText(spec.value, value, what),
      }),
    ),
    value,
    spec.hint === undefined ? null : el("p", { class: "copyrow__hint", text: spec.hint }),
  );
}

export interface EmptySpec {
  glyph: IconName;
  title: string;
  lines: Child[];
  actions?: Child[];
}

export function emptyState(spec: EmptySpec): HTMLElement {
  return el(
    "div",
    { class: "empty" },
    icon(spec.glyph, "empty__icon"),
    el("h3", { class: "empty__title", text: spec.title }),
    el("div", { class: "empty__lines" }, spec.lines),
    spec.actions === undefined ? null : el("div", { class: "empty__actions" }, spec.actions),
  );
}

/** A label bound to its control by id, which is the only reason this exists as a
 *  helper: every screen that forgets the binding produces an unusable field. */
export function field(id: string, label: string, control: HTMLElement, hint?: string): HTMLElement {
  control.id = id;
  return el(
    "div",
    { class: "field" },
    el("label", { class: "field__label", for: id, text: label }),
    control,
    hint === undefined ? null : el("p", { class: "field__hint", text: hint }),
  );
}
