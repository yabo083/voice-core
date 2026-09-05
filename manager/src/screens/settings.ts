// 设置: everything in `data/config.json` and `data/runtime.json` that is a setting, as
// controls.
//
// It replaces the 配置 screen, which showed those files and refused to touch them. The
// refusal was defensible while the only writer was a span splice driven from elsewhere; now
// `config_edit` writes any single leaf without disturbing a byte around it, so there is no
// reason left to make a person open Notepad - and no reason to keep a viewer of the same
// two files under a form that already exposes every key in them.
//
// Grouped the way somebody thinks about them, not the way the file is laid out. 字幕外观 and
// 快捷键 live in `config.json`, which the presenter re-reads on mtime. 服务 is the one key of
// `runtime.json` a person changes, and it lands at the next service start - the single
// standing note on this screen, because it is the one control that breaks the pattern the
// rest of them establish.
//
// 版本 is the recovery surface: every write is recorded in `data/settings.history.jsonl` as
// the change itself - which key, from what, to what - and 还原 applies that record backwards
// through the same splice, so the bytes come back and the comments around them never moved.

import { invoke } from "@tauri-apps/api/core";

import { el, fill } from "../dom";
import {
  colour,
  cssColour,
  form,
  number,
  segmented,
  text,
  toggle,
} from "../form";
import { icon } from "../icons";
import { ipcMessage } from "../ipc";
import { toast } from "../toast";
import { button, chip, emptyState, note, panel, pathText } from "../ui";

// IPC — hoisted into ipc.ts by the integrator

/** Every setting this app offers a control for, resolved to the value in force. */
export interface Settings {
  annotationAbove: boolean;
  reveal: string;
  nameColor: string;
  textColor: string;
  rubyColor: string;
  countdownColor: string;
  displaySeconds: number;
  toggleDialog: string;
  toggleHold: string;
  idleStopSecs: number;
  /** The keys the files actually carry. Anything else is showing a built-in. */
  written: string[];
}

/** One setting, named. The field IS the discriminant, so "leave it alone" and "set it to
 *  null" cannot be confused - see the note on `SettingEdit` in `config_edit.rs`. */
export type SettingEdit =
  | { field: "annotationAbove"; value: boolean }
  | { field: "reveal"; value: string }
  | { field: "nameColor"; value: string }
  | { field: "textColor"; value: string }
  | { field: "rubyColor"; value: string }
  | { field: "countdownColor"; value: string }
  | { field: "displaySeconds"; value: number }
  | { field: "toggleDialog"; value: string }
  | { field: "toggleHold"; value: string }
  | { field: "idleStopSecs"; value: number };

/** One value of one leaf as the file carries it, or the absence of the member holding it.
 *  Absence is a state and not a missing value: these files use `null` for "deliberately not
 *  set", and a row that showed the two the same way would be wrong about which. */
export type Leaf = { set: string } | { absent: { member: string[] } };

/** One recorded change to one setting, and both of its ends. */
export interface Change {
  /** The handle 还原 sends back. Stable across the rotation that an index would not be. */
  seq: number;
  tsMs: number;
  /** `config.json` or `runtime.json`. */
  file: string;
  /** `["dialog", "reveal"]`. Its last key is the setting's name everywhere else here. */
  path: string[];
  before: Leaf;
  after: Leaf;
}

/** Also read by the pack page, for the values an unset pack field falls back to. This
 *  wrapper and `Settings` both move to `ipc.ts` at integration. */
export const settingsRead = (): Promise<Settings> => invoke("settings_read");

/** Validates, writes one leaf, records the change, and answers with the settings as they
 *  now are. Rejects with a sentence naming the field. */
const settingsWrite = (edit: SettingEdit): Promise<Settings> => invoke("settings_write", { edit });

const settingsHistory = (): Promise<Change[]> => invoke("settings_history");

/** Applies one recorded change backwards. */
const settingsRestore = (seq: number): Promise<Settings> => invoke("settings_restore", { seq });

// --- the screen ------------------------------------------------------------------------

/** The panel titles, and the group half of a 版本 row's first line: one owner per word. */
const DIALOG = "字幕外观";
const HOTKEYS = "快捷键";
const SERVICE = "服务";

interface Field {
  group: string;
  label: string;
}

/** Every setting, keyed by the key its file carries.
 *
 *  That key is what `Settings.written` lists, what a `Change.path` ends in and what the
 *  markers beside the controls are stored under, so this one table answers "what is this
 *  called" for the form and for the 版本 rows alike - and a row can name the setting a
 *  change was to instead of naming the file it was in, which is what made the old list
 *  three identical lines. */
const FIELDS: Record<SettingEdit["field"], Field> = {
  annotationAbove: { group: DIALOG, label: "原文置于上方" },
  reveal: { group: DIALOG, label: "文字动效" },
  displaySeconds: { group: DIALOG, label: "停留时间" },
  nameColor: { group: DIALOG, label: "说话人颜色" },
  textColor: { group: DIALOG, label: "正文颜色" },
  rubyColor: { group: DIALOG, label: "原文颜色" },
  countdownColor: { group: DIALOG, label: "倒计时条颜色" },
  toggleDialog: { group: HOTKEYS, label: "切换字幕显隐" },
  toggleHold: { group: HOTKEYS, label: "字幕常驻开关" },
  idleStopSecs: { group: SERVICE, label: "显存释放延时" },
};

/** The same table, indexed by whatever a recorded change actually ends in - that key for
 *  every setting this build has, and something a newer one wrote for anything else. */
const BY_KEY: Record<string, Field | undefined> = FIELDS;

const REVEAL = [
  { value: "typewriter", label: "打字机" },
  { value: "sweep", label: "扫光" },
  { value: "fade", label: "淡入" },
];

const REVEAL_HINT = "打字机：按音频时长匀速显现；扫光：保持排版固定；淡入：按句子逐条淡入。";

/** Local time, because the timestamp came back as epoch ms precisely so the webview - which
 *  knows the machine's timezone - would be the one to format it.
 *
 *  Time of day for a change made today, which is every change a person is about to undo;
 *  the date as well for anything older, because `22:07` alone would then be a lie about
 *  when. */
function when(ms: number): string {
  const at = new Date(ms);
  const today = at.toDateString() === new Date().toDateString();
  return at.toLocaleString(
    undefined,
    today
      ? { hour: "2-digit", minute: "2-digit", hour12: false }
      : { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false },
  );
}

/** A leaf as the row shows it: the value, or 默认值 when the key was not in the file - the
 *  same word the marker beside the control uses for the same state. */
function valueText(leaf: Leaf): string {
  if (!("set" in leaf)) return "默认值";
  // The quotes belong to the file, not to the value, and this row is about the value. Every
  // recorded value is a colour, one of three words, a hotkey or a number, so none of them can
  // carry an escape that slicing the quotes off would break.
  const raw = leaf.set;
  return raw.startsWith('"') && raw.endsWith('"') ? raw.slice(1, -1) : raw;
}

export function createSettingsScreen(): HTMLElement {
  let settings: Settings | null = null;
  let changes: Change[] | null = null;

  const dialog = panel({ title: DIALOG });
  const keys = panel({ title: HOTKEYS });
  const service = panel({ title: SERVICE });
  const history = panel({
    title: "版本",
    actions: [
      button({
        label: "刷新",
        glyph: "arrow-clockwise",
        small: true,
        kind: "quiet",
        onClick: () => void loadHistory(),
      }),
    ],
  });

  /** Whether a key is in the file rather than showing a built-in. Kept per row so the marker
   *  can be corrected in place after a write, which is the one thing a save changes on a
   *  page that must not re-render under the user's cursor. */
  const markers: Record<string, HTMLElement> = {};

  function marker(key: keyof Settings): HTMLElement {
    const node = el("span");
    markers[key] = node;
    return node;
  }

  function paintMarkers(): void {
    const current = settings;
    if (current === null) return;
    for (const [key, node] of Object.entries(markers)) {
      fill(node, current.written.includes(key) ? null : chip("默认值", "idle"));
    }
  }

  /** The subtitle mock, in a host of its own so a write can redraw it without redrawing the
   *  controls above it - which would move focus out of whatever the user is still typing in. */
  const previewHost = el("div");

  function paintPreview(): void {
    const current = settings;
    fill(previewHost, current === null ? null : preview(current));
  }

  /** Every control's save goes through here: one edit, then the things that are now stale -
   *  the built-in markers, the mock and the change list. The acknowledgement is not here: it
   *  is `toast("已保存", "ok")`, raised by the control itself in `form.ts`, so every form in
   *  this app says it in the same place and one drag says it once. */
  async function apply(edit: SettingEdit): Promise<void> {
    settings = await settingsWrite(edit);
    paintMarkers();
    paintPreview();
    refreshHistory();
  }

  /** The 版本 list after a write, once per burst.
   *
   *  A stepper writes on every press, and a drag from 6 to 12 would otherwise be twelve
   *  reads of the history and twelve rebuilds of the list it merges into one entry anyway. */
  let pending = 0;
  function refreshHistory(): void {
    window.clearTimeout(pending);
    pending = window.setTimeout(() => void loadHistory(), 300);
  }

  function renderDialog(): void {
    const current = settings;
    if (current === null) {
      fill(dialog.body, skeleton(4));
      return;
    }
    fill(
      dialog.body,
      form(
        toggle({
          key: "set-annotation-above",
          label: FIELDS.annotationAbove.label,
          hint: "设置发音原文的显示位置；关闭后固定置于下方。",
          value: current.annotationAbove,
          meta: marker("annotationAbove"),
          save: (value) => apply({ field: "annotationAbove", value }),
        }),
        segmented({
          key: "set-reveal",
          label: FIELDS.reveal.label,
          hint: REVEAL_HINT,
          value: current.reveal,
          options: REVEAL,
          meta: marker("reveal"),
          save: (value) => apply({ field: "reveal", value: value ?? "typewriter" }),
        }),
        number({
          key: "set-display-seconds",
          label: FIELDS.displaySeconds.label,
          value: current.displaySeconds,
          min: 0.5,
          max: 600,
          step: 0.5,
          unit: "秒",
          meta: marker("displaySeconds"),
          save: (value) => apply({ field: "displaySeconds", value: value ?? 6 }),
        }),
        colour({
          key: "set-name-color",
          label: FIELDS.nameColor.label,
          value: current.nameColor,
          meta: marker("nameColor"),
          save: (value) => apply({ field: "nameColor", value: value ?? "#a48bff" }),
        }),
        colour({
          key: "set-text-color",
          label: FIELDS.textColor.label,
          value: current.textColor,
          meta: marker("textColor"),
          save: (value) => apply({ field: "textColor", value: value ?? "#f2f2f2" }),
        }),
        colour({
          key: "set-ruby-color",
          label: FIELDS.rubyColor.label,
          hint: "格式支持 #rgb、#rrggbb 或 #aarrggbb（前两位为透明度通道）。",
          value: current.rubyColor,
          meta: marker("rubyColor"),
          save: (value) => apply({ field: "rubyColor", value: value ?? "#9effffff" }),
        }),
        colour({
          key: "set-countdown-color",
          label: FIELDS.countdownColor.label,
          value: current.countdownColor,
          meta: marker("countdownColor"),
          save: (value) => apply({ field: "countdownColor", value: value ?? "#d98b6cef" }),
        }),
      ),
      previewHost,
    );
    paintPreview();
  }

  /** The four colours, the annotation side and the countdown, as one line of subtitle.
   *
   *  Rebuilt from the answer the write came back with rather than live-bound to the controls:
   *  the value in the mock is then always a value that reached the file, so the mock can
   *  never show a colour the presenter is not about to draw.
   *
   *  Only the mock is rebuilt. The controls above it keep their DOM, and with it the caret
   *  of whatever the user is still typing in. */
  function preview(current: Settings): HTMLElement {
    const stage = el(
      "div",
      { class: "preview" },
      el("div", { class: "preview__backdrop", "aria-hidden": "true" }),
      el(
        "div",
        { class: "preview__dialog" },
        el(
          "div",
          { class: "preview__character" },
          el("div", { class: "preview__avatar" }, icon("microphone-stage")),
          el("span", { class: "preview__name", text: "霞沢美游" }),
        ),
        el(
          "div",
          { class: "preview__content" },
          current.annotationAbove
            ? [
                el("span", { class: "preview__ruby", text: "おはよう、司令官さん。" }),
                el("span", { class: "preview__text", text: "早上好，指挥官。" }),
              ]
            : [
                el("span", { class: "preview__text", text: "早上好，指挥官。" }),
                el("span", { class: "preview__ruby", text: "おはよう、司令官さん。" }),
              ],
        ),
        el("div", { class: "preview__countdown" }),
      ),
    );
    stage.style.setProperty("--preview-name", cssColour(current.nameColor));
    stage.style.setProperty("--preview-text", cssColour(current.textColor));
    stage.style.setProperty("--preview-ruby", cssColour(current.rubyColor));
    stage.style.setProperty("--preview-countdown", cssColour(current.countdownColor));
    return stage;
  }

  function renderKeys(): void {
    const current = settings;
    if (current === null) {
      fill(keys.body, skeleton(2));
      return;
    }
    fill(
      keys.body,
      form(
        text({
          key: "set-toggle-dialog",
          label: FIELDS.toggleDialog.label,
          hint: "必须包含修饰键（Ctrl/Alt/Shift），避免拦截系统全局输入。",
          value: current.toggleDialog,
          mono: true,
          placeholder: "Ctrl+Alt+D",
          meta: marker("toggleDialog"),
          validate: hotkey,
          save: (value) => apply({ field: "toggleDialog", value }),
        }),
        text({
          key: "set-toggle-hold",
          label: FIELDS.toggleHold.label,
          hint: "必须包含修饰键（Ctrl/Alt/Shift），避免拦截系统全局输入。",
          value: current.toggleHold,
          mono: true,
          placeholder: "Ctrl+Alt+H",
          meta: marker("toggleHold"),
          validate: hotkey,
          save: (value) => apply({ field: "toggleHold", value }),
        }),
      ),
    );
  }

  function renderService(): void {
    const current = settings;
    if (current === null) {
      fill(service.body, skeleton(1));
      return;
    }
    fill(
      service.body,
      form(
        number({
          key: "set-idle-stop",
          label: FIELDS.idleStopSecs.label,
          hint: "达到延时后释放模型缓存；持续空闲将终止引擎子进程。设为 0 表示常驻显存。",
          value: current.idleStopSecs,
          min: 0,
          max: 86400,
          step: 60,
          integer: true,
          unit: "秒",
          meta: marker("idleStopSecs"),
          save: (value) => apply({ field: "idleStopSecs", value: value ?? 900 }),
        }),
      ),
      // The one standing note on this screen: it is the single control here that does not
      // apply immediately, and waiting for an effect that never comes is the costly version.
      note("warn", "修改将在下次服务启动时生效"),
    );
  }

  function renderHistory(): void {
    if (changes === null) {
      fill(history.body, skeleton(2));
      return;
    }
    if (changes.length === 0) {
      fill(
        history.body,
        emptyState({
          glyph: "clock-counter-clockwise",
          title: "暂无改动记录",
          lines: [
            el(
              "p",
              null,
              "改动任何设置后，这里会出现可以还原的记录；记录写在 ",
              pathText("data\\settings.history.jsonl"),
              "。",
            ),
          ],
        }),
      );
      return;
    }
    // The scroller, not the panel: the list grows with every write, and a panel that grows
    // with it pushes the window past the screen.
    fill(history.body, el("div", { class: "history" }, changes.map(historyRow)));
  }

  /** What the change was to: the setting's own name, or the path when the entry is one this
   *  build has no control for - a key a newer version wrote, or a hand-edited history. */
  function settingName(change: Change): string {
    const field = BY_KEY[change.path[change.path.length - 1]];
    return field === undefined
      ? `${change.file} · ${change.path.join(".")}`
      : `${field.group} · ${field.label}`;
  }

  function historyRow(change: Change): HTMLElement {
    const tail = el("div", { class: "history__tail" });

    function render(confirming: boolean): void {
      fill(
        tail,
        el("span", { class: "history__time", text: when(change.tsMs) }),
        confirming
          ? [
              button({
                label: "确认还原",
                kind: "danger",
                glyph: "check",
                small: true,
                onClick: () => {
                  void settingsRestore(change.seq)
                    .then((next) => {
                      settings = next;
                      // A restore can put back a value every control on the page reads from,
                      // so this is the one case that redraws them.
                      renderDialog();
                      renderKeys();
                      renderService();
                      paintMarkers();
                      paintPreview();
                      void loadHistory();
                      toast(`已还原 ${change.file}`, "ok");
                    })
                    .catch((err: unknown) => {
                      render(false);
                      toast(`还原配置失败：${ipcMessage(err)}`, "fail");
                    });
                },
              }),
              button({ label: "取消", kind: "quiet", small: true, onClick: () => render(false) }),
            ]
          : button({
              label: "还原",
              glyph: "clock-counter-clockwise",
              small: true,
              kind: "quiet",
              onClick: () => render(true),
            }),
      );
    }
    render(false);

    return el(
      "div",
      { class: "history__row" },
      el(
        "div",
        { class: "history__what" },
        el("span", { class: "history__field", text: settingName(change) }),
        // The values as the file carries them, which is what the record is of - and what
        // makes the row worth reading instead of three identical timestamps.
        el("span", {
          class: "history__delta",
          dir: "ltr",
          text: `${valueText(change.before)} → ${valueText(change.after)}`,
        }),
      ),
      tail,
    );
  }

  async function loadSettings(): Promise<void> {
    try {
      settings = await settingsRead();
    } catch (err: unknown) {
      toast(`加载设置失败：${ipcMessage(err)}`, "fail");
      return;
    }
    renderDialog();
    renderKeys();
    renderService();
    paintMarkers();
  }

  async function loadHistory(): Promise<void> {
    try {
      changes = await settingsHistory();
    } catch (err: unknown) {
      toast(`加载改动记录失败：${ipcMessage(err)}`, "fail");
      changes = [];
    }
    renderHistory();
  }

  renderDialog();
  renderKeys();
  renderService();
  renderHistory();
  void loadSettings();
  void loadHistory();

  // Both files are hand-editable by design, and the runtime and the presenter both re-read
  // them. The shell builds this screen once and afterwards only hides it, so without this a
  // file changed behind the panel's back would keep showing its values from boot.
  document.addEventListener("app:navigate", (ev: Event) => {
    const { to } = (ev as CustomEvent<{ to: string }>).detail;
    if (to !== "settings") return;
    void loadSettings();
    void loadHistory();
  });

  return el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: "设置" }),
      ),
    ),
    dialog.root,
    keys.root,
    service.root,
    history.root,
  );
}

/** The same rule `config_edit::hotkey` applies, on the way in rather than on the way out. */
function hotkey(value: string): string | null {
  const parts = value
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const modifiers = ["ctrl", "control", "alt", "shift", "win", "super", "meta"];
  const isModifier = (part: string): boolean => modifiers.includes(part.toLowerCase());
  if (
    parts.length < 2 ||
    !parts.slice(0, -1).every(isModifier) ||
    isModifier(parts[parts.length - 1])
  ) {
    return "快捷键格式：修饰键+按键，如 Ctrl+Alt+D。";
  }
  return null;
}

/** Boxes the size of the rows that are coming, so the panel does not resize under the user
 *  when the answer lands. */
function skeleton(rows: number): HTMLElement {
  return el(
    "div",
    { class: "skeletons", "aria-hidden": "true" },
    Array.from({ length: rows }, () => el("div", { class: "skeleton" })),
  );
}
