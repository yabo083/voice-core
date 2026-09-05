// 配置: the two files that decide how this program behaves, shown as they are on disk.
//
// Read-only, deliberately. config.json is JSONC written for a human — comments above
// every key, a `//` note next to the line somebody changed — and the one thing in this
// app allowed to write it is `config_edit`'s span splice, driven by the 音色 screen. A
// settings form here would either round-trip the file through a parser (deleting the
// prose that explains it) or become a second way to do what 音色 already does. So this
// screen shows the bytes and hands the file to an editor.
//
// The provenance column is the runtime's own verdict. `src/packs.rs::hydrate` is the only
// implementation of the precedence and it now records, as it merges, which file won each
// field; this screen renders that answer. Comparing the two files here to work it out
// again would be a second implementation of the one rule — the exact failure this screen
// exists to make visible.

import { el, fill, type Child } from "../dom";
import { formatBytes } from "../format";
import {
  configFiles,
  ipcMessage,
  packEffective,
  packManifestFile,
  type ConfigFile,
  type EffectivePack,
  type Pack,
} from "../ipc";
import { refreshVoices, status, voices } from "../state";
import { toast } from "../toast";
import {
  button,
  chip,
  emptyState,
  expander,
  field,
  navigate,
  note,
  panel,
  openButton,
  pathText,
  type Tone,
} from "../ui";

// --- JSONC, coloured --------------------------------------------------------

type TokenKind = "key" | "str" | "num" | "bool" | "null" | "punct" | "comment" | "plain";

interface Token {
  kind: TokenKind;
  text: string;
}

const WS = /\s/;
const DIGIT = /[0-9]/;
/** Everything that can appear after a number's first character, including the exponent
 *  and the sign inside it. Deliberately permissive: `1e-` is malformed JSON, and a
 *  viewer's job is to show it, not to reject it. */
const NUMBER_BODY = /[0-9eE+.\-]/;
const PUNCT = "{}[],:";
const CRLF = /\r\n?/g;

/** Classifies runs of text; never parses and never rejects, because the reason to open a
 *  config file is often that it is wrong.
 *
 *  Strings are consumed with their escapes, so `\"` does not end one and a `//` inside one
 *  is not a comment. An unterminated string stops at its line rather than recolouring the
 *  rest of the file, and an unclosed block comment runs to the end because that is what it
 *  does to the parser too. Anything unrecognised — indentation, the Chinese prose in the
 *  shipped config, a stray byte — comes out as plain text, which is what keeps a broken
 *  file readable. */
function tokenize(text: string): Token[] {
  const out: Token[] = [];
  // The unclassified run is tracked as an offset rather than accumulated into a string:
  // this walks whole config files character by character, and one growing string per file
  // is one allocation instead of thousands.
  let plainFrom = 0;
  let i = 0;

  const emit = (kind: TokenKind, from: number, to: number): void => {
    if (from > plainFrom) out.push({ kind: "plain", text: text.slice(plainFrom, from) });
    out.push({ kind, text: text.slice(from, to) });
    plainFrom = to;
  };

  while (i < text.length) {
    const ch = text[i];

    if (ch === '"') {
      const from = i;
      i += 1;
      while (i < text.length) {
        const c = text[i];
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
      while (ahead < text.length && WS.test(text[ahead])) ahead += 1;
      emit(text[ahead] === ":" ? "key" : "str", from, i);
      continue;
    }

    if (ch === "/" && text[i + 1] === "/") {
      const from = i;
      while (i < text.length && text[i] !== "\n") i += 1;
      emit("comment", from, i);
      continue;
    }

    if (ch === "/" && text[i + 1] === "*") {
      const from = i;
      i += 2;
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i += 1;
      i = Math.min(i + 2, text.length);
      emit("comment", from, i);
      continue;
    }

    if (DIGIT.test(ch) || (ch === "-" && i + 1 < text.length && DIGIT.test(text[i + 1]))) {
      const from = i;
      i += 1;
      while (i < text.length && NUMBER_BODY.test(text[i])) i += 1;
      emit("num", from, i);
      continue;
    }

    if (text.startsWith("true", i) || text.startsWith("false", i)) {
      const to = i + (ch === "t" ? 4 : 5);
      emit("bool", i, to);
      i = to;
      continue;
    }

    if (text.startsWith("null", i)) {
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
  if (text.length > plainFrom) out.push({ kind: "plain", text: text.slice(plainFrom) });
  return out;
}

/** The tokens as numbered lines.
 *
 *  The line number is a real element on the line it belongs to, not a CSS counter: it is
 *  the number a person types into an editor to get back to this line, so it has to be
 *  there when the file is read out or copied. Line breaks are structural here — a line is
 *  an element, not a `\n` — so `.jsonview__line` carries the layout. */
function jsonView(text: string, label: string): HTMLElement {
  const view = el("pre", { class: "jsonview", dir: "ltr", tabindex: "0", "aria-label": label });
  // CRLF is what PowerShell and Notepad write, and Chromium renders a lone \r inside
  // <pre> as a line break of its own, which would double-space the whole file.
  const body = text.replace(CRLF, "\n");
  let count = 0;

  function nextLine(): HTMLElement {
    count += 1;
    const row = el(
      "span",
      { class: "jsonview__line" },
      el("span", { class: "jsonview__gutter", "aria-hidden": "true", text: String(count) }),
    );
    view.appendChild(row);
    return row;
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

/** One file: what it is, how big, where, and its contents.
 *
 *  An expander because runtime.json is five lines and config.json is forty: the head
 *  answers "is it there and how big" while closed, which is the whole question for one of
 *  them most of the time. */
function fileBlock(file: ConfigFile, id: string): HTMLElement {
  const block = expander({
    title: file.label,
    id,
    tail: file.exists
      ? chip(formatBytes(file.bytes), "idle")
      : chip("文件不存在", "warn", "warning"),
  });

  fill(
    block.body,
    el("div", { class: "cfg__path" }, pathText(file.path, 64), openButton(file.path)),
    !file.exists
      ? emptyState({
          glyph: "info",
          title: "文件不存在",
          lines: [
            el("p", { text: "程序只在需要写它的时候写；现在按内置的默认行为运行。" }),
          ],
        })
      : file.text === "" && file.bytes > 0
        ? note(
            "warn",
            "这一刻读不出来",
            el("p", {
              text: "文件在，内容读不到——通常是编辑器正在原地保存。按“重新读取”再试一次。",
            }),
          )
        : jsonView(file.text, file.label),
  );
  return block.root;
}

// --- effective values -------------------------------------------------------

const SOURCE_WORDS = ["pack", "config", "derived"] as const;
type ConfigSource = (typeof SOURCE_WORDS)[number];

const SOURCE_LABEL: Record<ConfigSource, string> = {
  pack: "包内 voicepack.json",
  config: "config.json",
  derived: "推导 / 内置",
};

const SOURCE_TONE: Record<ConfigSource, Tone> = {
  pack: "ok",
  config: "accent",
  derived: "idle",
};

/** The order a person reads these in: who the voice is, then what it is, then where it
 *  lives. Anything not listed is appended rather than dropped — the table is the
 *  runtime's report, not this screen's idea of what a pack has. */
const FIELD_ORDER = [
  "name",
  "character",
  "kind",
  "languages",
  "engine",
  "avatar",
  "dialog",
  "synthesis",
  "path",
];

function isKnownSource(source: string): source is ConfigSource {
  return (SOURCE_WORDS as readonly string[]).includes(source);
}

/** Colour comes from `.src--*`, which is one vocabulary for provenance defined once. The
 *  chip tone under it says the same thing, so the row reads even where that colour is
 *  the only difference. A word this build has never heard of shows itself, marked. */
function sourceChip(source: string): HTMLElement {
  const node = isKnownSource(source)
    ? chip(SOURCE_LABEL[source], SOURCE_TONE[source])
    : chip(source, "warn", "warning");
  node.classList.add(`src--${source}`);
  return node;
}

/** A value as one table cell. An absolute path is middle-elided with the whole string in
 *  its title, the way every path in this app is shown; an object (dialog, synthesis) is
 *  compact JSON, because the file above the table already shows it in full. */
function valueCell(value: unknown): Child {
  if (value === undefined || value === null) return "未设置";
  if (typeof value === "string") {
    if (value === "") return "（空）";
    return /^[A-Za-z]:[\\/]/.test(value) ? pathText(value, 44) : value;
  }
  if (Array.isArray(value)) return value.length === 0 ? "（空）" : value.join(" / ");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/** The app's pending shape: boxes the size of the rows that are coming, so the panel does
 *  not resize under the user when the answer lands. */
function skeletons(rows: number): HTMLElement {
  return el(
    "div",
    { class: "skeletons", "aria-hidden": "true" },
    Array.from({ length: rows }, () => el("div", { class: "skeleton" })),
  );
}

export function createConfigScreen(): HTMLElement {
  let files: ConfigFile[] | null = null;
  let packs: Pack[] | null = null;
  let selected: string | null = null;
  let manifest: ConfigFile | null = null;
  let effective: EffectivePack | null = null;
  let loading = false;

  const program = panel({
    title: "程序配置",
    hint: "运行时和托盘都从这两个文件读。这里只显示，不改动——要改就用打开按钮，在编辑器里改。",
    actions: [
      button({
        label: "重新读取",
        kind: "quiet",
        glyph: "arrow-clockwise",
        onClick: () => void loadFiles(),
      }),
    ],
  });

  const packSection = panel({
    title: "音色包配置",
    hint: "包自己的描述文件，加上一张表：字段 · 生效值 · 由哪个文件决定。",
    actions: [
      button({
        label: "重新读取",
        kind: "quiet",
        glyph: "arrow-clockwise",
        onClick: () => {
          void refreshVoices();
          if (selected !== null) void loadPack(selected);
        },
      }),
    ],
  });

  const rule = panel({ title: "一句话说明" });

  async function loadFiles(): Promise<void> {
    try {
      files = await configFiles();
    } catch (err: unknown) {
      toast(`读取配置文件失败：${ipcMessage(err)}`, "fail");
      files = [];
    }
    renderFiles();
  }

  function renderFiles(): void {
    if (files === null) {
      fill(program.body, skeletons(2));
      return;
    }
    fill(
      program.body,
      files.map((file, index) => fileBlock(file, `config-file-${index}`)),
    );
  }

  async function loadPack(id: string): Promise<void> {
    loading = true;
    renderPack();
    try {
      const [file, merged] = await Promise.all([packManifestFile(id), packEffective(id)]);
      // A selection made while these were in flight owns the panel now: answering for the
      // pack the user has since left would redraw it under them.
      if (selected !== id) return;
      manifest = file;
      effective = merged;
    } catch (err: unknown) {
      if (selected !== id) return;
      toast(`读取音色包配置失败：${ipcMessage(err)}`, "fail");
      manifest = null;
      effective = null;
    } finally {
      if (selected === id) {
        loading = false;
        renderPack();
      }
    }
  }

  function select(id: string): void {
    selected = id;
    manifest = null;
    effective = null;
    void loadPack(id);
  }

  /** The table, or the reason there is none. */
  function effectiveBlock(): Child {
    const merged = effective;
    if (merged === null) {
      return status.value.reachable
        ? note(
            "info",
            "运行时没有报告这个包",
            el("p", {
              text: "它在运行，但 /api/voices 里没有这个 id。通常是 config.json 刚被改过，运行时还没读到。",
            }),
          )
        : note(
            "info",
            "运行时没在跑，所以没有生效值",
            el("p", {
              text: "上面这份列表来自 config.json。哪个字段由哪个文件决定，是运行时合并出来的结论；启动服务之后这里会出现一张表。",
            }),
          );
    }
    const sources = merged.sources;
    if (sources === undefined) {
      return note(
        "warn",
        "这个运行时不报告字段来源",
        el("p", {
          text: "它答复了这个包，但没有带 sources。跟面板一起装的那个运行时会带。",
        }),
      );
    }
    // Listed fields first, in reading order; then whatever this build has never heard of,
    // because the table is the runtime's report and must not silently drop a row.
    const shown = [
      ...FIELD_ORDER.filter((key) => key in sources),
      ...Object.keys(sources).filter((key) => !FIELD_ORDER.includes(key)),
    ];
    return el(
      "div",
      { class: "eff", role: "table", "aria-label": "生效值与来源" },
      shown.map((key) =>
        el(
          "div",
          { class: "eff__row", role: "row" },
          el("code", { class: "eff__k", role: "rowheader", dir: "ltr", text: key }),
          el("span", { class: "eff__v", role: "cell" }, valueCell(merged[key])),
          el("span", { class: "eff__src", role: "cell" }, sourceChip(sources[key])),
        ),
      ),
    );
  }

  function renderPack(): void {
    if (packs === null) {
      fill(packSection.body, skeletons(1));
      return;
    }
    if (packs.length === 0) {
      fill(
        packSection.body,
        emptyState({
          glyph: "microphone-stage",
          title: "还没有音色包",
          lines: [
            el("p", {
              text: "登记一个之后，这里会显示它自己的 voicepack.json，以及每个字段最终由哪个文件决定。",
            }),
          ],
          actions: [
            button({
              label: "去登记音色包",
              glyph: "plus",
              onClick: (ev: MouseEvent) => navigate("voices", ev),
            }),
          ],
        }),
      );
      return;
    }

    const picker = el(
      "select",
      {
        class: "input",
        onchange: (ev: Event) => select((ev.target as HTMLSelectElement).value),
      },
      packs.map((pack) => {
        const shown = pack.character ?? pack.name;
        return el("option", {
          value: pack.id,
          selected: pack.id === selected,
          text: shown === "" || shown === pack.id ? pack.id : `${shown}（${pack.id}）`,
        });
      }),
    );

    fill(
      packSection.body,
      field("config-pack", "音色包", picker),
      loading
        ? skeletons(2)
        : [
            manifest === null ? null : fileBlock(manifest, "config-manifest"),
            effectiveBlock(),
          ],
    );
  }

  function onVoices(list: Pack[] | null): void {
    packs = list;
    const ids = (list ?? []).map((pack) => pack.id);
    // A pack that was removed under us must not keep a stale table on screen; a pack that
    // is still there keeps the selection through a list refresh.
    if (selected === null || !ids.includes(selected)) {
      const first = ids.length > 0 ? ids[0] : null;
      selected = first;
      manifest = null;
      effective = null;
      if (first !== null) void loadPack(first);
    }
    renderPack();
  }

  fill(
    rule.body,
    note(
      "info",
      "配置只有两处",
      el("p", {
        text: "全局在 data\\config.json，每个音色包自己带 voicepack.json。同一个字段两处都写了，包内的赢。",
      }),
      el("p", {
        text: "两处都没写的是推导出来的（名字用 id、类型看磁盘上是什么）或者程序内置的行为，不是第三份配置。",
      }),
    ),
  );

  voices.subscribe(onVoices);
  // The merged view only exists while the runtime is up, so a service that starts (or stops)
  // after this screen was built changes the answer and nothing else would ask again. On the
  // transition only: `status` polls every second, and refetching per tick would hammer the
  // runtime for an answer that has not moved.
  let served = status.value.reachable;
  status.subscribe((next) => {
    if (next.reachable === served) return;
    served = next.reachable;
    if (selected !== null) void loadPack(selected);
  });
  void loadFiles();
  // The 音色 screen fetches the same list when it is built, but a screen that only works
  // because a sibling ran first is a screen that breaks the day it is opened alone.
  if (voices.value === null) void refreshVoices();

  return el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: "配置" }),
      ),
    ),
    program.root,
    packSection.root,
    rule.root,
  );
}
