// The typed controls the two configuration screens are built out of.
//
// Every control here is the same shape: a spec carrying a label, a hint, the current
// value, a validator and a save, and a row that renders the control plus its own
// feedback. Nothing in this module knows what a setting means; the screens do.
//
// Three decisions are worth stating because they are what makes this a settings UI and
// not a form:
//
//   - There is no Save button. A control writes when it settles - immediately for a
//     switch or a choice, after a short pause for anything typed - and says so quietly.
//     A settings surface with a Save button asks the user to remember which of eleven
//     controls they touched, which is the job the machine is here to do.
//   - An invalid value is refused WITHOUT discarding it. The control keeps exactly the
//     characters typed and `.form__error` says what is wrong, because a field that snaps
//     back to the old value while you are halfway through typing `#a48bff` is a field
//     nobody can type into. Nothing reaches the backend until it validates.
//   - The backend validates again. That is not belt-and-braces: these files are edited by
//     hand, so a value can reach `config_edit` without ever passing through here, and a
//     form that trusted the backend would have to round-trip to say "that is not a
//     colour". Two gates, one per entrance.
//
// The raw-file viewer lives here too. It is the collapsed 查看原始文件 affordance both
// pages carry - the one part of the retired 配置 screen worth keeping - and it belongs
// beside the controls it sits under rather than in a module of its own.

import { el, fill, type Child } from "./dom";
import { formatBytes } from "./format";
import { t } from "./i18n";
import { icon, type IconName } from "./icons";
import { ipcMessage, pickFile, type ConfigFile } from "./ipc";
import { toast } from "./toast";
import { button, chip, expander, note, openButton, pathText, withTip } from "./ui";

/** How long a typed value sits still before it is written.
 *
 *  Long enough that `#a48bff` is one write rather than seven, short enough that letting go
 *  of the keyboard and looking at the screen shows 已保存 already there. */
const SETTLE_MS = 450;

export interface Row<T> {
  /** Unique on the page; becomes the control's id and the label's `for`. */
  key: string;
  label: string;
  hint?: string;
  value: T;
  /** null when the value is acceptable, otherwise the sentence shown under the control. */
  validate?: (value: T) => string | null;
  /** Rejects with a message when the write fails; the edit stays on screen either way. */
  save: (value: T) => Promise<void>;
  /** Shown beside the control: where this value comes from today. */
  meta?: Child;
  /** Present means the control is inert, and this is the reason. */
  disabled?: string;
}

interface Feedback {
  /** Valid: schedule or perform the write. */
  commit: (value: unknown, immediate: boolean) => void;
  /** Invalid: say so, write nothing, keep what the user typed. */
  reject: (message: string) => void;
}

/** The label / control / feedback scaffold every control shares.
 *
 *  `hint` is deliberately NOT drawn under the control. A settings page whose every row
 *  carries a sentence is a page nobody reads, and the label plus the value it is showing is
 *  the explanation for all but a handful of rows. Anything longer than a label goes on
 *  hover and on keyboard focus, through `ui.ts::withTip` - the one tooltip this app has.
 *  A `disabled` reason goes to the same place: it is the answer to "why can I not use
 *  this", asked by pointing at the thing. */
function scaffold<T>(spec: Row<T>, control: HTMLElement): { root: HTMLElement; feedback: Feedback } {
  const id = `f-${spec.key}`;
  control.id = id;
  if (spec.disabled !== undefined) control.classList.add("is-disabled");

  const error = el("p", { class: "form__error", role: "alert", hidden: true });

  const tip = spec.disabled ?? spec.hint;
  const root = el(
    "div",
    { class: "form__row" },
    el("div", { class: "form__meta" }, el("label", { class: "form__label", for: id, text: spec.label })),
    el(
      "div",
      { class: "form__control" },
      tip === undefined ? control : withTip(control, tip),
      spec.meta === undefined ? null : spec.meta,
    ),
    error,
  );

  let timer = 0;
  let inflight = false;
  let queued: { value: T } | null = null;

  function show(message: string | null): void {
    error.textContent = message ?? "";
    error.hidden = message === null;
  }

  function write(value: T): void {
    // One write at a time per control, with only the newest follow-up kept: a slow write
    // plus more typing must not land in the order the IPC felt like.
    if (inflight) {
      queued = { value };
      return;
    }
    inflight = true;
    void spec
      .save(value)
      .then(
        () => {
          show(null);
          toast(t.common.saved, "ok");
        },
        (err: unknown) => show(ipcMessage(err)),
      )
      .finally(() => {
        inflight = false;
        const next = queued;
        queued = null;
        if (next !== null) write(next.value);
      });
  }

  const feedback: Feedback = {
    commit: (value: unknown, immediate: boolean) => {
      if (spec.disabled !== undefined) return;
      const next = value as T;
      const problem = spec.validate?.(next) ?? null;
      if (problem !== null) {
        window.clearTimeout(timer);
        show(problem);
        return;
      }
      show(null);
      window.clearTimeout(timer);
      if (immediate) write(next);
      else timer = window.setTimeout(() => write(next), SETTLE_MS);
    },
    reject: (message: string) => {
      window.clearTimeout(timer);
      show(message);
    },
  };

  return { root, feedback };
}

// --- text ------------------------------------------------------------------------------

export interface TextRow extends Row<string> {
  placeholder?: string;
  /** For engine names and hotkey specs: strings whose characters matter one by one. */
  mono?: boolean;
}

export function text(spec: TextRow): HTMLElement {
  const input = el("input", {
    class: `input${spec.mono === true ? " input--mono" : ""}`,
    type: "text",
    value: spec.value,
    placeholder: spec.placeholder,
    spellcheck: "false",
    disabled: spec.disabled !== undefined,
  });
  const { root, feedback } = scaffold(spec, input);
  input.addEventListener("input", () => feedback.commit(input.value, false));
  // Leaving a field is a decision: write what is there now rather than waiting out a
  // settle timer the user cannot see.
  input.addEventListener("blur", () => feedback.commit(input.value, true));
  return root;
}

// --- number ----------------------------------------------------------------------------

export interface NumberRow extends Row<number | null> {
  min: number;
  max: number;
  step?: number;
  /** Rejects a fraction. Steps, seeds, seconds-as-integers. */
  integer?: boolean;
  /** Empty is allowed and means "not set". */
  nullable?: boolean;
  /** Rendered inside the stepper, after the number. */
  unit?: string;
  /** What the empty state means, e.g. 跟随运行时默认. */
  placeholder?: string;
}

export function number(spec: NumberRow): HTMLElement {
  const step = spec.step ?? 1;
  const input = el("input", {
    class: "input stepper__input",
    type: "text",
    inputmode: spec.integer === true ? "numeric" : "decimal",
    value: spec.value === null ? "" : String(spec.value),
    placeholder: spec.placeholder,
    spellcheck: "false",
    disabled: spec.disabled !== undefined,
  });

  const down = button({
    glyph: "minus",
    name: t.common.stepDown(spec.label),
    small: true,
    kind: "quiet",
    onClick: () => nudge(-step),
    disabled: spec.disabled !== undefined,
  });
  const up = button({
    glyph: "plus",
    name: t.common.stepUp(spec.label),
    small: true,
    kind: "quiet",
    onClick: () => nudge(step),
    disabled: spec.disabled !== undefined,
  });
  down.classList.add("stepper__btn");
  up.classList.add("stepper__btn");

  const control = el(
    "div",
    { class: "stepper" },
    down,
    input,
    spec.unit === undefined ? null : el("span", { class: "stepper__unit", text: spec.unit }),
    up,
  );
  const { root, feedback } = scaffold(spec, control);
  // The stepper carries the label, but focus has to land on the text field inside it.
  control.removeAttribute("id");
  input.id = `f-${spec.key}`;

  /** The typed text as a number, or the reason it is not one. */
  function parse(raw: string): { value: number | null } | { problem: string } {
    const trimmed = raw.trim();
    if (trimmed === "") {
      return spec.nullable === true ? { value: null } : { problem: t.common.required };
    }
    const parsed = Number(trimmed);
    if (!Number.isFinite(parsed)) return { problem: t.common.notANumber };
    if (spec.integer === true && !Number.isInteger(parsed)) return { problem: t.common.notAnInteger };
    if (parsed < spec.min || parsed > spec.max) {
      return { problem: t.common.range(spec.min, spec.max) };
    }
    return { value: parsed };
  }

  function settle(immediate: boolean): void {
    const parsed = parse(input.value);
    if ("problem" in parsed) feedback.reject(parsed.problem);
    else feedback.commit(parsed.value, immediate);
  }

  /** Stepping is a click, so it writes at once - and it clamps rather than erroring,
   *  because a button that produces an invalid value is a broken button. */
  function nudge(by: number): void {
    const parsed = parse(input.value);
    const from =
      "problem" in parsed || parsed.value === null ? (spec.value ?? spec.min) : parsed.value;
    const next = Math.min(spec.max, Math.max(spec.min, round(from + by, step)));
    input.value = String(next);
    feedback.commit(next, true);
  }

  input.addEventListener("input", () => settle(false));
  input.addEventListener("blur", () => settle(true));
  return root;
}

/** Keeps `0.1 + 0.2` out of the file. */
function round(value: number, step: number): number {
  const text = String(step);
  const decimals = text.includes(".") ? text.split(".")[1].length : 0;
  return Number(value.toFixed(decimals));
}

// --- toggle ----------------------------------------------------------------------------

export function toggle(spec: Row<boolean>): HTMLElement {
  const control = el(
    "button",
    {
      class: "switch",
      type: "button",
      role: "switch",
      "aria-checked": String(spec.value),
      disabled: spec.disabled !== undefined,
    },
    el("span", { class: "switch__thumb", "aria-hidden": "true" }),
  );
  const { root, feedback } = scaffold(spec, control);
  control.addEventListener("click", () => {
    const next = control.getAttribute("aria-checked") !== "true";
    control.setAttribute("aria-checked", String(next));
    feedback.commit(next, true);
  });
  return root;
}

// --- choices ---------------------------------------------------------------------------

export interface Choice {
  value: string;
  label: string;
}

export interface ChoiceRow extends Row<string | null> {
  options: Choice[];
  /** Adds a first option meaning "not set here"; `null` is then a legal value. */
  unset?: string;
}

function withUnset(spec: ChoiceRow): Choice[] {
  return spec.unset === undefined ? spec.options : [{ value: "", label: spec.unset }, ...spec.options];
}

/** Three or four short options: all of them visible, one click to change. */
export function segmented(spec: ChoiceRow): HTMLElement {
  const items: HTMLButtonElement[] = [];
  const control = el("div", { class: "seg", role: "radiogroup", "aria-label": spec.label });
  const { root, feedback } = scaffold(spec, control);

  for (const option of withUnset(spec)) {
    const selected = (spec.value ?? "") === option.value;
    const item = el(
      "button",
      {
        class: `seg__item${selected ? " is-active" : ""}`,
        type: "button",
        role: "radio",
        "aria-checked": String(selected),
        disabled: spec.disabled !== undefined,
      },
      el("span", { text: option.label }),
    );
    item.addEventListener("click", () => {
      for (const other of items) {
        const on = other === item;
        other.classList.toggle("is-active", on);
        other.setAttribute("aria-checked", String(on));
      }
      feedback.commit(option.value === "" ? null : option.value, true);
    });
    items.push(item);
    control.appendChild(item);
  }
  return root;
}

/** More options than fit on a line, or options whose labels are sentences. */
export function select(spec: ChoiceRow): HTMLElement {
  const control = el(
    "select",
    { class: "input select", disabled: spec.disabled !== undefined },
    withUnset(spec).map((option) =>
      el("option", {
        value: option.value,
        selected: (spec.value ?? "") === option.value,
        text: option.label,
      }),
    ),
  );
  const { root, feedback } = scaffold(spec, control);
  control.addEventListener("change", () => {
    feedback.commit(control.value === "" ? null : control.value, true);
  });
  return root;
}

// --- colour ----------------------------------------------------------------------------

const HEX = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;

/** One of the presenter's colours as something CSS understands.
 *
 *  The presenter writes `#aarrggbb` - alpha FIRST, the WPF/XAML order - and CSS's eight
 *  digit hex is `#rrggbbaa`. Handing the string straight to a style attribute would show a
 *  translucent grey as an opaque near-black, so the conversion lives here rather than in
 *  each place a swatch is drawn. The two lengths both notations agree on pass through. */
export function cssColour(value: string): string {
  if (!HEX.test(value)) return "transparent";
  const digits = value.slice(1);
  if (digits.length !== 8) return value;
  const alpha = parseInt(digits.slice(0, 2), 16) / 255;
  const r = parseInt(digits.slice(2, 4), 16);
  const g = parseInt(digits.slice(4, 6), 16);
  const b = parseInt(digits.slice(6, 8), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha.toFixed(3)})`;
}

/** The `#rrggbb` a native colour input can show, dropping any alpha. */
function opaque(value: string): string {
  if (!HEX.test(value)) return "#000000";
  const digits = value.slice(1);
  if (digits.length === 3) {
    return `#${digits[0]}${digits[0]}${digits[1]}${digits[1]}${digits[2]}${digits[2]}`;
  }
  return `#${digits.slice(digits.length - 6)}`;
}

export interface ColourRow extends Row<string | null> {
  /** What an empty field falls back to, shown as the placeholder and in the well. */
  fallback?: string;
  /** Allows clearing the colour, which means "not set here". */
  unset?: boolean;
}

export function colour(spec: ColourRow): HTMLElement {
  const picker = el("input", {
    type: "color",
    value: opaque(spec.value ?? spec.fallback ?? "#000000"),
    "aria-label": t.common.pickColour(spec.label),
    disabled: spec.disabled !== undefined,
  });
  const well = el(
    "button",
    {
      class: "swatch__chip",
      type: "button",
      "aria-label": t.common.pickColour(spec.label),
      disabled: spec.disabled !== undefined,
    },
    picker,
  );
  const hex = el("input", {
    class: "input input--mono swatch__hex",
    type: "text",
    value: spec.value ?? "",
    placeholder: spec.fallback ?? t.common.notSet,
    spellcheck: "false",
    disabled: spec.disabled !== undefined,
  });
  well.style.background = cssColour(spec.value ?? spec.fallback ?? "");

  const clear =
    spec.unset === true
      ? button({
          glyph: "x",
          name: t.common.clearField(spec.label),
          title: t.common.clearInherit,
          small: true,
          kind: "quiet",
          disabled: spec.disabled !== undefined,
          onClick: () => {
            hex.value = "";
            well.style.background = cssColour(spec.fallback ?? "");
            feedback.commit(null, true);
          },
        })
      : null;

  const control = el("div", { class: "swatch" }, well, hex, clear);
  const { root, feedback } = scaffold(spec, control);
  control.removeAttribute("id");
  hex.id = `f-${spec.key}`;

  // The well is a button so the keyboard reaches it; the native input inside it is what
  // opens the OS picker.
  well.addEventListener("click", (ev: Event) => {
    if (ev.target !== picker) picker.click();
  });
  picker.addEventListener("input", () => {
    hex.value = picker.value;
    well.style.background = picker.value;
    feedback.commit(picker.value, true);
  });

  function settle(immediate: boolean): void {
    const typed = hex.value.trim();
    if (typed === "") {
      if (spec.unset !== true) {
        feedback.reject(t.common.colourRequired);
        return;
      }
      well.style.background = cssColour(spec.fallback ?? "");
      feedback.commit(null, immediate);
      return;
    }
    if (!HEX.test(typed)) {
      // Refused here, and `config_edit` would refuse it too - which is the point: this
      // value never reaches the file, and the characters stay in the field.
      feedback.reject(t.common.badHex);
      return;
    }
    well.style.background = cssColour(typed);
    picker.value = opaque(typed);
    feedback.commit(typed, immediate);
  }

  hex.addEventListener("input", () => settle(false));
  hex.addEventListener("blur", () => settle(true));
  return root;
}

// --- tags ------------------------------------------------------------------------------

export interface TagsRow extends Row<string[]> {
  placeholder?: string;
}

/** A short list of short strings - languages, and nothing else so far.
 *
 *  A list rather than a comma-separated field because the thing being edited IS a list: a
 *  typo in `ja,zh ` produces a language tag nobody can see is wrong, while a chip that says
 *  the wrong thing is wrong on screen. */
export function tags(spec: TagsRow): HTMLElement {
  let current = [...spec.value];
  const control = el("div", { class: "tags" });
  const entry = el("input", {
    class: "input tags__input",
    type: "text",
    placeholder: spec.placeholder ?? t.common.addTag,
    spellcheck: "false",
    disabled: spec.disabled !== undefined,
  });
  const { root, feedback } = scaffold(spec, control);
  control.removeAttribute("id");
  entry.id = `f-${spec.key}`;

  function render(): void {
    fill(
      control,
      current.map((tag) =>
        el(
          "span",
          { class: "tags__item" },
          el("span", { text: tag }),
          el(
            "button",
            {
              class: "tags__x",
              type: "button",
              "aria-label": t.common.removeTag(tag),
              disabled: spec.disabled !== undefined,
              onclick: () => {
                current = current.filter((kept) => kept !== tag);
                render();
                feedback.commit(current, true);
              },
            },
            icon("x"),
          ),
        ),
      ),
      entry,
    );
  }

  function add(): void {
    for (const piece of entry.value.split(/[,，\s]+/)) {
      const tag = piece.trim();
      if (tag !== "" && !current.includes(tag)) current.push(tag);
    }
    entry.value = "";
    render();
    entry.focus();
    feedback.commit(current, true);
  }

  entry.addEventListener("keydown", (ev: KeyboardEvent) => {
    if (ev.key === "Enter" || ev.key === "," || ev.key === "，") {
      ev.preventDefault();
      add();
      return;
    }
    if (ev.key === "Backspace" && entry.value === "" && current.length > 0) {
      current = current.slice(0, -1);
      render();
      feedback.commit(current, true);
    }
  });
  entry.addEventListener("blur", () => {
    if (entry.value.trim() !== "") add();
  });

  render();
  return root;
}

// --- file row --------------------------------------------------------------------------

export interface FileRow extends Row<string | null> {
  /** Copies the chosen file where it belongs and resolves with the name to store. */
  bring: (picked: string) => Promise<string>;
  extensions: string[];
  pickLabel: string;
  glyph: IconName;
}

/** A picker, never a text field: the value stored is produced by an import that copies the
 *  file into place, so a hand-typed path would be a path nothing ever validated. */
export function file(spec: FileRow): HTMLElement {
  const name = el("code", { class: "filerow__name", dir: "ltr" });
  const tail = el("div", { class: "filerow__tail" });
  const control = el(
    "div",
    { class: "filerow" },
    el("div", { class: "filerow__lead" }, icon(spec.glyph, "filerow__icon"), name),
    tail,
  );
  const { root, feedback } = scaffold(spec, control);
  let value = spec.value;

  function render(): void {
    name.textContent = value ?? t.common.notSet;
    fill(
      tail,
      button({
        label: value === null ? spec.pickLabel : t.common.change,
        glyph: "file-plus",
        small: true,
        disabled: spec.disabled !== undefined,
        onClick: () => {
          void pickFile(spec.pickLabel, spec.extensions)
            .then(async (picked) => {
              if (picked === null) return;
              value = await spec.bring(picked);
              render();
              feedback.commit(value, true);
            })
            .catch((err: unknown) => feedback.reject(ipcMessage(err)));
        },
      }),
      value === null
        ? null
        : button({
            glyph: "x",
            name: t.common.clearField(spec.label),
            title: t.common.clear,
            small: true,
            kind: "quiet",
            disabled: spec.disabled !== undefined,
            onClick: () => {
              value = null;
              render();
              feedback.commit(null, true);
            },
          }),
    );
  }
  render();
  return root;
}

// --- hotkey capture --------------------------------------------------------------------

export interface HotkeyRow extends Row<string> {
  placeholder?: string;
}

/** Event-key to chord-name for the keys whose `KeyboardEvent.key` is a word, not a
 *  character. Everything else carries over as itself: letters upper-cased, F-keys,
 *  digits and punctuation as typed. The names match what the presenter registers. */
const KEY_NAMES: Record<string, string> = {
  " ": "Space",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Home: "Home",
  End: "End",
  Insert: "Insert",
  Delete: "Delete",
  Backspace: "Backspace",
  Enter: "Enter",
  Tab: "Tab",
};

/** The chord name for a key event, or null for a bare modifier press - which is a
 *  half-typed chord, not a value. */
function chordKey(ev: KeyboardEvent): string | null {
  const key = ev.key;
  if (key === "Control" || key === "Alt" || key === "Shift" || key === "Meta") return null;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(key)) return key;
  if (/^Numpad[0-9]$/.test(key)) return key;
  const named = KEY_NAMES[key];
  if (named !== undefined) return named;
  return key.length === 1 ? key.toUpperCase() : key;
}

/** A hotkey row: the value as text (hand-typed, as before), plus a capture button that
 *  listens for the next chord and writes it - the interaction every game rebinding a key
 *  uses, because "type Ctrl+Alt+D into a field" asks for characters nobody thinks of as
 *  text. Validation is unchanged: the chord goes through `feedback.commit`, so a
 *  modifier-less capture is refused by the same rule a typed value is. */
export function hotkey(spec: HotkeyRow): HTMLElement {
  const input = el("input", {
    class: "input input--mono",
    type: "text",
    value: spec.value,
    placeholder: spec.placeholder,
    spellcheck: "false",
    disabled: spec.disabled !== undefined,
  });
  const capture = button({
    label: t.common.capture,
    small: true,
    kind: "quiet",
    disabled: spec.disabled !== undefined,
    onClick: () => (capturing ? stop() : start()),
  });
  const hint = el("span", { class: "form__capture-hint", "aria-hidden": "true" });
  const control = el("div", { class: "capture" }, input, capture, hint);
  const { root, feedback } = scaffold(spec, control);
  control.removeAttribute("id");
  input.id = `f-${spec.key}`;

  let capturing = false;

  function stop(): void {
    if (!capturing) return;
    capturing = false;
    window.removeEventListener("keydown", onKey, true);
    capture.textContent = t.common.capture;
    hint.textContent = "";
    input.classList.remove("is-capturing");
  }

  function start(): void {
    if (capturing || spec.disabled !== undefined) return;
    capturing = true;
    capture.textContent = t.common.capturing;
    hint.textContent = t.common.captureCancel;
    input.classList.add("is-capturing");
    window.addEventListener("keydown", onKey, true);
  }

  /** Capture phase on `window`, so the chord lands here before it reaches any other
   *  control or the webview's own shortcuts; a captured chord must not also toggle
   *  something else while the user is holding it. */
  function onKey(ev: KeyboardEvent): void {
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.key === "Escape") {
      stop();
      return;
    }
    const key = chordKey(ev);
    if (key === null) return;
    const parts = [
      ev.ctrlKey ? "Ctrl" : null,
      ev.altKey ? "Alt" : null,
      ev.shiftKey ? "Shift" : null,
      ev.metaKey ? "Win" : null,
      key,
    ].filter((part): part is string => part !== null);
    input.value = parts.join("+");
    stop();
    feedback.commit(input.value, true);
  }

  input.addEventListener("input", () => feedback.commit(input.value, false));
  input.addEventListener("blur", () => feedback.commit(input.value, true));
  return root;
}

// --- grouping --------------------------------------------------------------------------

/** A titled block of rows inside a panel. */
export function group(title: string, ...rows: Child[]): HTMLElement {
  return el("div", { class: "form__group" }, el("h3", { class: "form__group-title", text: title }), rows);
}

export function form(...children: Child[]): HTMLElement {
  return el("div", { class: "form" }, children);
}

/** Where a value comes from today, beside the control that is about to change that.
 *
 *  The vocabulary is the runtime's own - `src/packs.rs` reports `pack` | `config` |
 *  `derived` per field - and the colours are `.src--*`, defined once. A field the registry
 *  or a built-in currently decides is the interesting case: editing here moves it into the
 *  pack, and this marker is what warns the user that it will. */
export function provenance(source: string | null, hint?: string): HTMLElement | null {
  if (source === null) return null;
  const known: Record<string, string> = {
    pack: t.common.provPack,
    config: t.common.provConfig,
    derived: t.common.provDerived,
  };
  const label = known[source] ?? source;
  const node = chip(label, source === "pack" ? "ok" : source === "config" ? "accent" : "idle");
  node.classList.add(`src--${source}`);
  if (hint !== undefined) node.title = hint;
  return node;
}

// --- the raw file, as it is on disk ----------------------------------------------------
//
// Lifted from the retired 配置 screen, behaviour unchanged: it classifies runs of text and
// never parses, because the reason to open one of these files is often that it is wrong.
// Strings are consumed with their escapes, so a `//` inside one is not a comment, and an
// unterminated string stops at its line rather than recolouring the rest of the file.

type TokenKind = "key" | "str" | "num" | "bool" | "null" | "punct" | "comment" | "plain";

interface Token {
  kind: TokenKind;
  text: string;
}

const WS = /\s/;
const DIGIT = /[0-9]/;
/** Deliberately permissive: `1e-` is malformed JSON, and a viewer's job is to show it. */
const NUMBER_BODY = /[0-9eE+.\-]/;
const PUNCT = "{}[],:";
const CRLF = /\r\n?/g;

function tokenize(source: string): Token[] {
  const out: Token[] = [];
  // The unclassified run is an offset rather than an accumulating string: this walks whole
  // config files character by character, and one slice per run is one allocation instead of
  // thousands.
  let plainFrom = 0;
  let i = 0;

  const emit = (kind: TokenKind, from: number, to: number): void => {
    if (from > plainFrom) out.push({ kind: "plain", text: source.slice(plainFrom, from) });
    out.push({ kind, text: source.slice(from, to) });
    plainFrom = to;
  };

  while (i < source.length) {
    const ch = source[i];

    if (ch === '"') {
      const from = i;
      i += 1;
      while (i < source.length) {
        const c = source[i];
        if (c === "\\") {
          i += 2;
          continue;
        }
        if (c === "\n") break;
        i += 1;
        if (c === '"') break;
      }
      // A string with a colon after it is a key, and keys are what a reader scans for.
      let ahead = i;
      while (ahead < source.length && WS.test(source[ahead])) ahead += 1;
      emit(source[ahead] === ":" ? "key" : "str", from, i);
      continue;
    }

    if (ch === "/" && source[i + 1] === "/") {
      const from = i;
      while (i < source.length && source[i] !== "\n") i += 1;
      emit("comment", from, i);
      continue;
    }

    if (ch === "/" && source[i + 1] === "*") {
      const from = i;
      i += 2;
      while (i < source.length && !(source[i] === "*" && source[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, source.length);
      emit("comment", from, i);
      continue;
    }

    if (DIGIT.test(ch) || (ch === "-" && i + 1 < source.length && DIGIT.test(source[i + 1]))) {
      const from = i;
      i += 1;
      while (i < source.length && NUMBER_BODY.test(source[i])) i += 1;
      emit("num", from, i);
      continue;
    }

    if (source.startsWith("true", i) || source.startsWith("false", i)) {
      const to = i + (ch === "t" ? 4 : 5);
      emit("bool", i, to);
      i = to;
      continue;
    }

    if (source.startsWith("null", i)) {
      emit("null", i, i + 4);
      i += 4;
      continue;
    }

    if (PUNCT.includes(ch)) {
      emit("punct", i, i + 1);
      i += 1;
      continue;
    }

    i += 1;
  }
  if (source.length > plainFrom) out.push({ kind: "plain", text: source.slice(plainFrom) });
  return out;
}

/** The tokens as numbered lines. The line number is a real element on its line, not a CSS
 *  counter: it is the number a person types into an editor to get back here, so it has to
 *  survive being read out or copied. */
export function jsonView(source: string, label: string): HTMLElement {
  const view = el("pre", { class: "jsonview", dir: "ltr", tabindex: "0", "aria-label": label });
  // CRLF is what PowerShell and Notepad write, and Chromium renders a lone \r inside <pre>
  // as a break of its own, which would double-space the whole file.
  const body = source.replace(CRLF, "\n");
  let count = 0;

  function nextLine(): HTMLElement {
    count += 1;
    const line = el(
      "span",
      { class: "jsonview__line" },
      el("span", { class: "jsonview__gutter", "aria-hidden": "true", text: String(count) }),
    );
    view.appendChild(line);
    return line;
  }

  let line = nextLine();
  for (const token of tokenize(body)) {
    const parts = token.text.split("\n");
    for (let index = 0; index < parts.length; index += 1) {
      if (index > 0) line = nextLine();
      const part = parts[index];
      if (part === "") continue;
      line.appendChild(
        token.kind === "plain"
          ? document.createTextNode(part)
          : el("span", { class: `jsonview__${token.kind}`, text: part }),
      );
    }
  }
  return view;
}

/** 查看原始文件: the bytes on disk, collapsed.
 *
 *  Collapsed, and never the primary surface - which is the whole change of this release.
 *  The form above it is where the file gets edited; this is here for the two questions the
 *  form cannot answer: "what does my own comment say" and "why does the control disagree
 *  with the line I typed". */
export function rawFile(shown: ConfigFile, id: string, open = false): HTMLElement {
  const block = expander({
    title: t.common.viewSource(shown.label),
    id,
    open,
    tail: shown.exists
      ? chip(formatBytes(shown.bytes), "idle")
      : chip(t.common.fileMissing, "warn", "warning"),
  });

  fill(
    block.body,
    el("div", { class: "cfg__path" }, pathText(shown.path, 64), openButton(shown.path)),
    !shown.exists
      ? note(
          "info",
          t.common.cfgNotWritten,
          el("p", {
            text: t.common.cfgNotWrittenBody,
          }),
        )
      : shown.text === "" && shown.bytes > 0
        ? note(
            "warn",
            t.common.fileUnreadable,
            el("p", {
              text: t.common.fileUnreadableBody,
            }),
          )
        : jsonView(shown.text, shown.label),
  );
  return block.root;
}
