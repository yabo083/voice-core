// One voice pack's own configuration, as a secondary page inside 音色.
//
// The list used to be the whole screen and a pack was a row you could delete. Everything a
// pack decides about itself - who it is, what its subtitles look like, how it is
// synthesised, what emotion it carries - lived in a file the panel would only show you. So
// this page exists to edit that file, and every control on it writes the PACK'S OWN
// `voicepack.json`, never the registry entry: the manifest is what wins at read time
// (`docs/voicepack-spec.md`), so writing the form into `config.json` would produce an edit
// that silently does nothing.
//
// Two properties this page has to keep, both of them invisible when they work:
//
//   - A field this build has never heard of survives a save. The write is a byte-span
//     splice of one leaf, so the rest of the file is never even parsed on the way through -
//     and the keys this build does not know are listed at the bottom, because a promise
//     nobody can see is not one.
//   - Where a value comes from stays visible. `GET /api/voices` reports, per field, which
//     file won it; a control whose value is currently decided by the registry or by a
//     built-in says so, because editing it here moves that decision into the pack.
//
// A secondary page and not a window: the pack is a thing inside 音色, the back affordance
// goes back to the list, and Esc does the same. Nothing about it is a route - main.ts has
// one rail entry for 音色 and this is a view inside that screen.

import { invoke } from "@tauri-apps/api/core";

import { el, fill, type Child } from "../dom";
import { t } from "../i18n";
import {
  colour,
  cssColour,
  file,
  form,
  group,
  number,
  provenance,
  rawFile,
  segmented,
  tags,
  text,
} from "../form";
import { icon } from "../icons";
import {
  importAvatar,
  ipcMessage,
  packEffective,
  packManifestFile,
  type ConfigFile,
  type EffectivePack,
  type Pack,
  type PackKind,
} from "../ipc";
import { refreshVoices, status } from "../state";
import { toast } from "../toast";
import { button, chip, note, openButton, panel, pathText } from "../ui";
import { settingsRead, type Settings } from "./settings";

// IPC — hoisted into ipc.ts by the integrator

/** One pack as its editing page needs it: what its manifest says (`null` per field means
 *  the manifest is silent), where that manifest is, and what a reader sees today. */
export interface PackConfig {
  id: string;
  path: string;
  manifestPath: string;
  manifestExists: boolean;
  /** False when the pack sits on media this app cannot write. */
  writable: boolean;
  schema: number | null;
  name: string | null;
  character: string | null;
  kind: string | null;
  languages: string[] | null;
  engine: string | null;
  avatar: string | null;
  dialog: {
    nameColor: string | null;
    textColor: string | null;
    rubyColor: string | null;
    countdownColor: string | null;
    reveal: string | null;
    displaySeconds: number | null;
  };
  synthesis: {
    numSteps: number | null;
    seed: number | null;
    temperature: number | null;
  };
  expression: {
    emotion: string | null;
    cfgScaleCaption: number | null;
  };
  /** Top-level manifest keys this build has never heard of. */
  unknown: string[];
  /** The identity in force today, merged the way the runtime merges it. */
  effective: Pack;
}

/** One field of the manifest, named. Every nullable field writes an explicit `null` rather
 *  than losing its key — see the note on `PackEdit` in `config_edit.rs`. */
export type PackEdit =
  | { field: "name"; value: string }
  | { field: "character"; value: string | null }
  | { field: "kind"; value: string }
  | { field: "languages"; value: string[] }
  | { field: "engine"; value: string }
  | { field: "avatar"; value: string | null }
  | { field: "nameColor"; value: string | null }
  | { field: "textColor"; value: string | null }
  | { field: "rubyColor"; value: string | null }
  | { field: "countdownColor"; value: string | null }
  | { field: "reveal"; value: string | null }
  | { field: "displaySeconds"; value: number | null }
  | { field: "numSteps"; value: number | null }
  | { field: "seed"; value: number | null }
  | { field: "temperature"; value: number | null }
  | { field: "emotion"; value: string | null }
  | { field: "cfgScaleCaption"; value: number | null };

export interface Preview {
  requestId: string;
  audioId: string;
  bytes: number;
  durationMs: number;
  totalMs: number;
  coldStart: boolean;
  /** Event-stream subscribers at the moment of synthesis. Zero means nothing played it. */
  presenters: number;
}

const packConfig = (id: string): Promise<PackConfig | null> => invoke("pack_config", { id });

const packConfigWrite = (id: string, edit: PackEdit): Promise<PackConfig> =>
  invoke("pack_config_write", { id, edit });

/** `POST /api/speak` with this pack and one line, through the host. The pack's own
 *  `expression` is applied server-side, so the preview is the product of the file. */
const speakPreview = (id: string, text: string): Promise<Preview> =>
  invoke("speak_preview", { id, text });

// --- vocabulary ------------------------------------------------------------------------

/** The pack kinds a human can name. Module-bound like the dictionary itself: the panel's
 *  language is pinned at boot and a change reloads. */
export function kindLabel(kind: PackKind): string {
  switch (kind) {
    case "lora-adapter":
      return t.voice.kindLora;
    case "speaker-embedding":
      return t.voice.kindEmbedding;
    case "reference-audio":
      return t.voice.kindReference;
  }
}

export const PACK_KINDS: PackKind[] = ["lora-adapter", "speaker-embedding", "reference-audio"];

const REVEAL = () => [
  { value: "typewriter", label: t.voice.revealTypewriter },
  { value: "sweep", label: t.voice.revealSweep },
  { value: "fade", label: t.voice.revealFade },
];

/** A handful of the 45 the checkpoint understands, named so the field is usable without
 *  opening the model card. They go straight into the spoken text and repeat for emphasis. */
const EMOTION_HINT = () => t.voice.emotionHint;

const PREVIEW_LINE = "おはよう、司令官さん。";

// --- the page --------------------------------------------------------------------------

export function createVoiceDetail(pack: Pack, onBack: () => void): HTMLElement {
  let config: PackConfig | null = null;
  let effective: EffectivePack | null = null;
  let globals: Settings | null = null;
  let manifest: ConfigFile | null = null;

  const title = el("h2", { class: "detail__title", tabindex: "-1", text: shownName(pack) });
  const subtitle = el("code", { class: "detail__subtitle", dir: "ltr", text: pack.id });
  const actions = el("div", { class: "detail__actions" });
  const body = el("div", { class: "detail__body" });

  const root = el(
    "section",
    { class: "detail" },
    el(
      "header",
      { class: "detail__head" },
      el(
        "div",
        { class: "detail__lead" },
        el(
          "button",
          {
            class: "detail__back",
            type: "button",
            "aria-label": t.voice.backToList,
            title: t.voice.backTitle,
            onclick: onBack,
          },
          icon("arrow-left"),
        ),
        el("div", { class: "detail__titles" }, title, subtitle),
      ),
      actions,
    ),
    body,
  );

  // Esc goes back, which is what a secondary page owes a keyboard: the back arrow is one
  // target at the top of a page that scrolls.
  root.addEventListener("keydown", (ev: KeyboardEvent) => {
    if (ev.key !== "Escape") return;
    ev.preventDefault();
    onBack();
  });

  const identity = panel({ title: t.voice.identity });
  // The one fact the controls cannot show: an empty field is not "off", it is "follow 设置".
  const style = panel({ title: t.voice.style, hint: t.voice.styleHint });
  const synthesis = panel({ title: t.voice.synthesis });
  const expression = panel({ title: t.voice.expression });
  const audition = panel({ title: t.voice.audition });
  // Kept, unlike the settings screen's: a manifest can hold keys this build's form does not
  // render, and seeing them is the whole point.
  const raw = panel({
    title: t.voice.raw,
    actions: [
      button({
        label: t.voice.reload,
        glyph: "arrow-clockwise",
        small: true,
        kind: "quiet",
        onClick: () => void load(),
      }),
    ],
  });

  fill(
    actions,
    openButton(pack.path, t.voice.openPackDir),
  );

  /** Whichever file currently decides this field, or null when the runtime is not up to
   *  say. Section-granular for dialog / synthesis / expression, which is how
   *  `src/packs.rs` reports them. */
  function source(field: string): string | null {
    return effective?.sources?.[field] ?? null;
  }

  /** The marker beside a control, plus the sentence that explains why it matters. */
  function mark(field: string): Child {
    const from = source(field);
    if (from === null) return null;
    return provenance(
      from,
      from === "pack"
        ? t.voice.provPackHint
        : from === "config"
          ? t.voice.provConfigHint
          : t.voice.provDerivedHint,
    );
  }

  /** The subtitle mock, in a host of its own so a write can redraw it without redrawing the
   *  colour pickers above it and moving the caret out of one. */
  const stageHost = el("div");

  function paintStage(): void {
    fill(stageHost, config === null ? null : stage(config));
  }

  /** One edit, then the pieces a write makes stale: the mock, the merged view and its
   *  provenance, the raw file, the list behind this page (a name change shows up there), and
   *  the title above. */
  async function apply(edit: PackEdit): Promise<void> {
    config = await packConfigWrite(pack.id, edit);
    title.textContent = shownName(config.effective);
    paintStage();
    void refreshVoices();
    void refreshSide();
  }

  async function refreshSide(): Promise<void> {
    const [merged, shown] = await Promise.all([
      packEffective(pack.id).catch(() => null),
      packManifestFile(pack.id).catch(() => null),
    ]);
    effective = merged;
    manifest = shown;
    renderRaw();
  }

  /** Why a control is inert, or undefined when it is not. */
  function blocked(): string | undefined {
    if (config === null) return t.voice.blockedLoading;
    return config.writable
      ? undefined
      : t.voice.blockedReadonly;
  }

  function renderIdentity(): void {
    const current = config;
    if (current === null) {
      fill(identity.body, skeleton(4));
      return;
    }
    const disabled = blocked();
    fill(
      identity.body,
      el(
        "div",
        { class: "cfg__path" },
        pathText(current.manifestPath, 64),
        current.manifestExists
          ? chip(t.voice.manifestOk, "ok", "check-circle")
          : chip(t.voice.manifestInit, "idle"),
      ),
      form(
        text({
          key: `pack-${current.id}-name`,
          label: t.voice.nameLabel,
          value: current.name ?? current.effective.name,
          disabled,
          meta: mark("name"),
          validate: (value) => (value.trim() === "" ? t.voice.nameEmpty : null),
          save: (value) => apply({ field: "name", value }),
        }),
        text({
          key: `pack-${current.id}-character`,
          label: t.voice.characterLabel,
          hint: t.voice.characterHint,
          value: current.character ?? current.effective.character ?? "",
          placeholder: current.effective.name,
          disabled,
          meta: mark("character"),
          save: (value) => apply({ field: "character", value: value.trim() === "" ? null : value.trim() }),
        }),
        segmented({
          key: `pack-${current.id}-kind`,
          label: t.voice.kindLabel,
          value: current.kind ?? current.effective.kind,
          options: PACK_KINDS.map((kind) => ({
            value: kind,
            label: kindLabel(kind),
          })),
          disabled,
          meta: mark("kind"),
          save: (value) => apply({ field: "kind", value: value ?? "lora-adapter" }),
        }),
        tags({
          key: `pack-${current.id}-languages`,
          label: t.voice.languagesLabel,
          hint: t.voice.languagesHint,
          value: current.languages ?? current.effective.languages,
          placeholder: t.voice.languagesPlaceholder,
          disabled,
          meta: mark("languages"),
          validate: (value) => (value.length === 0 ? t.voice.languagesEmpty : null),
          save: (value) => apply({ field: "languages", value }),
        }),
        text({
          key: `pack-${current.id}-engine`,
          label: t.voice.engineLabel,
          hint: t.voice.engineHint,
          value: current.engine ?? current.effective.engine,
          mono: true,
          disabled,
          meta: mark("engine"),
          validate: (value) => (value.trim() === "" ? t.voice.engineEmpty : null),
          save: (value) => apply({ field: "engine", value }),
        }),
        file({
          key: `pack-${current.id}-avatar`,
          label: t.voice.avatarLabel,
          hint: t.voice.avatarHint,
          value: current.avatar,
          glyph: "microphone-stage",
          extensions: ["png", "jpg", "jpeg", "webp", "bmp"],
          pickLabel: t.voice.avatarPick,
          disabled,
          meta: mark("avatar"),
          bring: (picked) => importAvatar(picked, current.path),
          save: (value) => apply({ field: "avatar", value }),
        }),
      ),
    );
  }

  function renderStyle(): void {
    const current = config;
    if (current === null) {
      fill(style.body, skeleton(4));
      return;
    }
    const disabled = blocked();
    const dialog = current.dialog;
    const from = mark("dialog");
    fill(
      style.body,
      group(
        t.voice.coloursGroup,
        form(
          colour({
            key: `pack-${current.id}-name-color`,
            label: t.voice.nameColorLabel,
            value: dialog.nameColor,
            fallback: globals?.nameColor,
            unset: true,
            disabled,
            meta: from,
            save: (value) => apply({ field: "nameColor", value }),
          }),
          colour({
            key: `pack-${current.id}-text-color`,
            label: t.voice.textColorLabel,
            value: dialog.textColor,
            fallback: globals?.textColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "textColor", value }),
          }),
          colour({
            key: `pack-${current.id}-ruby-color`,
            label: t.voice.rubyColorLabel,
            hint: t.voice.rubyColorHint,
            value: dialog.rubyColor,
            fallback: globals?.rubyColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "rubyColor", value }),
          }),
          colour({
            key: `pack-${current.id}-countdown-color`,
            label: t.voice.countdownColorLabel,
            value: dialog.countdownColor,
            fallback: globals?.countdownColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "countdownColor", value }),
          }),
        ),
      ),
      group(
        t.voice.motionGroup,
        form(
          segmented({
            key: `pack-${current.id}-reveal`,
            label: t.voice.revealLabel,
            value: dialog.reveal,
            options: REVEAL(),
            unset: t.voice.inheritGlobal,
            disabled,
            save: (value) => apply({ field: "reveal", value }),
          }),
          number({
            key: `pack-${current.id}-display-seconds`,
            label: t.voice.displaySecondsLabel,
            value: dialog.displaySeconds,
            min: 0.5,
            max: 600,
            step: 0.5,
            nullable: true,
            unit: t.voice.secondsUnit,
            placeholder: globals === null ? t.voice.inheritGlobal : String(globals.displaySeconds),
            disabled,
            save: (value) => apply({ field: "displaySeconds", value }),
          }),
        ),
      ),
      stageHost,
    );
    paintStage();
  }

  /** This pack's line, with this pack's colours: the same mock the 设置 screen shows, fed
   *  from the merged values rather than the manifest's, because what a viewer sees is the
   *  merge and an unset colour has to preview as the global it falls back to. */
  function stage(current: PackConfig): HTMLElement {
    const colours = {
      name: current.dialog.nameColor ?? globals?.nameColor ?? "#a48bff",
      text: current.dialog.textColor ?? globals?.textColor ?? "#f2f2f2",
      ruby: current.dialog.rubyColor ?? globals?.rubyColor ?? "#9effffff",
      countdown: current.dialog.countdownColor ?? globals?.countdownColor ?? "#d98b6cef",
    };
    const node = el(
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
          el("span", { class: "preview__name", text: shownName(current.effective) }),
        ),
        el(
          "div",
          { class: "preview__content" },
          el("span", { class: "preview__text", text: "早上好，指挥官。" }),
          el("span", { class: "preview__ruby", text: PREVIEW_LINE }),
        ),
        el("div", { class: "preview__countdown" }),
      ),
    );
    node.style.setProperty("--preview-name", cssColour(colours.name));
    node.style.setProperty("--preview-text", cssColour(colours.text));
    node.style.setProperty("--preview-ruby", cssColour(colours.ruby));
    node.style.setProperty("--preview-countdown", cssColour(colours.countdown));
    return node;
  }

  function renderSynthesis(): void {
    const current = config;
    if (current === null) {
      fill(synthesis.body, skeleton(3));
      return;
    }
    const disabled = blocked();
    fill(
      synthesis.body,
      form(
        number({
          key: `pack-${current.id}-num-steps`,
          label: t.voice.stepsLabel,
          hint: t.voice.stepsHint,
          value: current.synthesis.numSteps,
          min: 1,
          max: 200,
          integer: true,
          nullable: true,
          placeholder: "32",
          disabled,
          meta: mark("synthesis"),
          save: (value) => apply({ field: "numSteps", value }),
        }),
        number({
          key: `pack-${current.id}-seed`,
          label: t.voice.seedLabel,
          hint: t.voice.seedHint,
          value: current.synthesis.seed,
          min: 0,
          max: 4294967295,
          integer: true,
          nullable: true,
          placeholder: t.voice.seedPlaceholder,
          disabled,
          save: (value) => apply({ field: "seed", value }),
        }),
        number({
          key: `pack-${current.id}-temperature`,
          label: "temperature",
          value: current.synthesis.temperature,
          min: 0,
          max: 2,
          step: 0.05,
          nullable: true,
          placeholder: t.voice.temperaturePlaceholder,
          disabled,
          save: (value) => apply({ field: "temperature", value }),
        }),
      ),
    );
  }

  function renderExpression(): void {
    const current = config;
    if (current === null) {
      fill(expression.body, skeleton(2));
      return;
    }
    const disabled = blocked();
    fill(
      expression.body,
      form(
        text({
          key: `pack-${current.id}-emotion`,
          label: t.voice.emotionLabel,
          hint: EMOTION_HINT(),
          value: current.expression.emotion ?? "",
          placeholder: t.voice.emotionPlaceholder,
          disabled,
          meta: mark("expression"),
          save: (value) => apply({ field: "emotion", value: value.trim() === "" ? null : value }),
        }),
        number({
          key: `pack-${current.id}-cfg-caption`,
          label: t.voice.cfgLabel,
          hint: t.voice.cfgHint,
          value: current.expression.cfgScaleCaption,
          min: 0,
          max: 10,
          step: 0.5,
          nullable: true,
          placeholder: "3",
          disabled,
          save: (value) => apply({ field: "cfgScaleCaption", value }),
        }),
      ),
    );
  }

  function renderAudition(): void {
    const line = el("input", {
      class: "input",
      type: "text",
      value: PREVIEW_LINE,
      spellcheck: "false",
      "aria-label": t.voice.auditionInput,
    });
    const result = el("div", { class: "preview__result" });
    let running = false;

    // The service can come up or go down while this page is open, and the page is built
    // once. So the check happens on the click rather than on the render: no subscription to
    // leak when the page is dropped, and never a button that is wrong about the world.
    const go = button({
      label: t.voice.auditionGo,
      kind: "primary",
      glyph: "play",
      onClick: () => {
        if (running) return;
        if (!status.value.reachable) {
          fill(
            result,
            icon("warning-circle"),
            el("span", { text: t.voice.auditionNotRunning }),
          );
          return;
        }
        running = true;
        fill(result, icon("spinner-gap", "spin"), el("span", { text: t.voice.auditionBusy }));
        void speakPreview(pack.id, line.value)
          .then((answer) => {
            fill(
              result,
              icon("check-circle"),
              el("span", {
                text: `${t.voice.auditionResult((answer.durationMs / 1000).toFixed(2), (answer.bytes / 1024).toFixed(0), answer.totalMs)}${answer.coldStart ? t.voice.auditionCold : ""}`,
              }),
              el("code", { class: "path", dir: "ltr", text: answer.requestId }),
            );
            if (answer.presenters === 0) {
              toast(t.voice.auditionNoPresenter, "info");
            }
          })
          .catch((err: unknown) => {
            fill(result, icon("warning-circle"), el("span", { text: ipcMessage(err) }));
          })
          .finally(() => {
            running = false;
          });
      },
    });

    fill(
      audition.body,
      el("div", { class: "cfg__path" }, line, go),
      result,
    );
  }

  function renderRaw(): void {
    const shown = manifest;
    const current = config;
    const open = (() => {
      const head = raw.body.querySelector<HTMLElement>("#pack-manifest-raw");
      return head !== null && !head.hidden;
    })();
    fill(
      raw.body,
      shown === null ? null : rawFile(shown, "pack-manifest-raw", open),
      current === null || current.unknown.length === 0
        ? null
        : note(
            "info",
            t.voice.unknownTitle,
            el("p", {
              text: t.voice.unknownBody(current.unknown.join("、")),
            }),
          ),
    );
  }

  function renderAll(): void {
    renderIdentity();
    renderStyle();
    renderSynthesis();
    renderExpression();
    renderRaw();
  }

  async function load(): Promise<void> {
    try {
      const [own, merged, shown, global] = await Promise.all([
        packConfig(pack.id),
        packEffective(pack.id).catch(() => null),
        packManifestFile(pack.id).catch(() => null),
        settingsRead().catch(() => null),
      ]);
      config = own;
      effective = merged;
      manifest = shown;
      globals = global;
    } catch (err: unknown) {
      toast(t.voice.loadFailed(ipcMessage(err)), "fail");
      return;
    }
    if (config === null) {
      fill(body, note("fail", t.voice.notFound, el("p", { text: t.voice.notFoundBody(pack.id) })));
      return;
    }
    renderAll();
  }

  fill(
    body,
    identity.root,
    style.root,
    synthesis.root,
    expression.root,
    audition.root,
    raw.root,
  );
  renderAll();
  renderAudition();
  void load();

  // The heading, not the back button: a keyboard user arriving here should hear which pack
  // they are in before they hear how to leave.
  window.setTimeout(() => title.focus(), 0);
  return root;
}

/** What to call a pack on screen: the character speaks, the pack is filed. */
function shownName(pack: Pack): string {
  const shown = pack.character ?? pack.name;
  return shown === "" ? pack.id : shown;
}

function skeleton(rows: number): HTMLElement {
  return el(
    "div",
    { class: "skeletons", "aria-hidden": "true" },
    Array.from({ length: rows }, () => el("div", { class: "skeleton" })),
  );
}
