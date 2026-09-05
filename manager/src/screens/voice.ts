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

export const KIND_LABEL: Record<PackKind, string> = {
  "lora-adapter": "LoRA 适配器",
  "speaker-embedding": "说话人嵌入",
  "reference-audio": "参考音频",
};

const REVEAL = [
  { value: "typewriter", label: "打字机" },
  { value: "sweep", label: "扫光" },
  { value: "fade", label: "淡入" },
];

/** A handful of the 45 the checkpoint understands, named so the field is usable without
 *  opening the model card. They go straight into the spoken text and repeat for emphasis. */
const EMOTION_HINT =
  "支持在待念文本中嵌入情绪标记，重复输入可增强情绪表达：😆 喜び 😭 嗚咽 😱 悲鳴 😖 苦しげ 🥺 震え声 👂 囁き ⏩ 早口 🐢 ゆっくり 🤭 笑い 🫶 優しく";

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
            "aria-label": "返回音色列表",
            title: "返回 (Esc)",
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

  const identity = panel({ title: "身份信息" });
  // The one fact the controls cannot show: an empty field is not "off", it is "follow 设置".
  const style = panel({ title: "字幕样式", hint: "未配置项将继承全局设置" });
  const synthesis = panel({ title: "合成参数" });
  const expression = panel({ title: "情绪控制" });
  const audition = panel({ title: "语音试听" });
  // Kept, unlike the settings screen's: a manifest can hold keys this build's form does not
  // render, and seeing them is the whole point.
  const raw = panel({
    title: "源配置文件",
    actions: [
      button({
        label: "重新载入",
        glyph: "arrow-clockwise",
        small: true,
        kind: "quiet",
        onClick: () => void load(),
      }),
    ],
  });

  fill(
    actions,
    openButton(pack.path, "在文件资源管理器中打开音色包目录"),
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
        ? "当前属性由音色包内的 voicepack.json 定义。"
        : from === "config"
          ? "当前属性由全局 config.json 注册项定义；保存后将转由音色包自身配置管理。"
          : "当前属性使用系统默认或推导值；保存后将写入音色包自身配置。",
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
    if (config === null) return "正在加载音色包配置…";
    return config.writable
      ? undefined
      : "音色包所在路径为只读介质或受限网络共享，无法写入修改。";
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
          ? chip("独立配置 (voicepack.json)", "ok", "check-circle")
          : chip("未初始化配置（首次保存时自动生成）", "idle"),
      ),
      form(
        text({
          key: `pack-${current.id}-name`,
          label: "显示名称",
          value: current.name ?? current.effective.name,
          disabled,
          meta: mark("name"),
          validate: (value) => (value.trim() === "" ? "显示名称不能为空" : null),
          save: (value) => apply({ field: "name", value }),
        }),
        text({
          key: `pack-${current.id}-character`,
          label: "角色名称",
          hint: "字幕窗口展示的说话人名称；留空时默认采用音色包名称。",
          value: current.character ?? current.effective.character ?? "",
          placeholder: current.effective.name,
          disabled,
          meta: mark("character"),
          save: (value) => apply({ field: "character", value: value.trim() === "" ? null : value.trim() }),
        }),
        segmented({
          key: `pack-${current.id}-kind`,
          label: "模型类型",
          value: current.kind ?? current.effective.kind,
          options: (Object.keys(KIND_LABEL) as PackKind[]).map((kind) => ({
            value: kind,
            label: KIND_LABEL[kind],
          })),
          disabled,
          meta: mark("kind"),
          save: (value) => apply({ field: "kind", value: value ?? "lora-adapter" }),
        }),
        tags({
          key: `pack-${current.id}-languages`,
          label: "支持语言",
          hint: "服务运行时将校验请求语言，拒绝未匹配语种的合成请求。",
          value: current.languages ?? current.effective.languages,
          placeholder: "添加语种代码（如 ja）",
          disabled,
          meta: mark("languages"),
          validate: (value) => (value.length === 0 ? "请至少指定一种语言代码（如 ja）" : null),
          save: (value) => apply({ field: "languages", value }),
        }),
        text({
          key: `pack-${current.id}-engine`,
          label: "推理引擎",
          hint: "引擎标识标签；针对 Irodori 架构请填写 irodori。",
          value: current.engine ?? current.effective.engine,
          mono: true,
          disabled,
          meta: mark("engine"),
          validate: (value) => (value.trim() === "" ? "推理引擎不能为空" : null),
          save: (value) => apply({ field: "engine", value }),
        }),
        file({
          key: `pack-${current.id}-avatar`,
          label: "头像图标",
          hint: "头像文件将归档至音色包目录，便于整体打包迁移。",
          value: current.avatar,
          glyph: "microphone-stage",
          extensions: ["png", "jpg", "jpeg", "webp", "bmp"],
          pickLabel: "选择图像…",
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
        "配色方案",
        form(
          colour({
            key: `pack-${current.id}-name-color`,
            label: "说话人颜色",
            value: dialog.nameColor,
            fallback: globals?.nameColor,
            unset: true,
            disabled,
            meta: from,
            save: (value) => apply({ field: "nameColor", value }),
          }),
          colour({
            key: `pack-${current.id}-text-color`,
            label: "正文文本颜色",
            value: dialog.textColor,
            fallback: globals?.textColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "textColor", value }),
          }),
          colour({
            key: `pack-${current.id}-ruby-color`,
            label: "发音原文颜色",
            hint: "格式支持 #rgb、#rrggbb 或 #aarrggbb（前两位为透明度通道）。",
            value: dialog.rubyColor,
            fallback: globals?.rubyColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "rubyColor", value }),
          }),
          colour({
            key: `pack-${current.id}-countdown-color`,
            label: "倒计时条颜色",
            value: dialog.countdownColor,
            fallback: globals?.countdownColor,
            unset: true,
            disabled,
            save: (value) => apply({ field: "countdownColor", value }),
          }),
        ),
      ),
      group(
        "动效与停留",
        form(
          segmented({
            key: `pack-${current.id}-reveal`,
            label: "文字动效",
            value: dialog.reveal,
            options: REVEAL,
            unset: "继承全局",
            disabled,
            save: (value) => apply({ field: "reveal", value }),
          }),
          number({
            key: `pack-${current.id}-display-seconds`,
            label: "停留时间",
            value: dialog.displaySeconds,
            min: 0.5,
            max: 600,
            step: 0.5,
            nullable: true,
            unit: "秒",
            placeholder: globals === null ? "继承全局" : String(globals.displaySeconds),
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
          label: "推理步数 (Steps)",
          hint: "采样迭代步数：数值越高生成质量越稳定，耗时相应增加；默认值为 32。",
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
          label: "随机种子 (Seed)",
          hint: "固定随机种子可确保相同文本生成一致的音频表现。",
          value: current.synthesis.seed,
          min: 0,
          max: 4294967295,
          integer: true,
          nullable: true,
          placeholder: "随机生成 (-1)",
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
          placeholder: "使用引擎默认值",
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
          label: "情绪标记",
          hint: EMOTION_HINT,
          value: current.expression.emotion ?? "",
          placeholder: "输入表情符号（如 😭😭）",
          disabled,
          meta: mark("expression"),
          save: (value) => apply({ field: "emotion", value: value.trim() === "" ? null : value }),
        }),
        number({
          key: `pack-${current.id}-cfg-caption`,
          label: "情绪引导强度 (CFG Scale)",
          hint: "cfgScaleCaption 参数，默认值为 3.0；数值越高对上方情绪标签的贴合越强。",
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
      "aria-label": "试听文本输入",
    });
    const result = el("div", { class: "preview__result" });
    let running = false;

    // The service can come up or go down while this page is open, and the page is built
    // once. So the check happens on the click rather than on the render: no subscription to
    // leak when the page is dropped, and never a button that is wrong about the world.
    const go = button({
      label: "试听合成",
      kind: "primary",
      glyph: "play",
      onClick: () => {
        if (running) return;
        if (!status.value.reachable) {
          fill(
            result,
            icon("warning-circle"),
            el("span", { text: "运行时服务未启动，请前往「状态」页面启动服务后再进行试听。" }),
          );
          return;
        }
        running = true;
        fill(result, icon("spinner-gap", "spin"), el("span", { text: "正在合成音频…" }));
        void speakPreview(pack.id, line.value)
          .then((answer) => {
            fill(
              result,
              icon("check-circle"),
              el("span", {
                text: `${(answer.durationMs / 1000).toFixed(2)} 秒音频 · ${(answer.bytes / 1024).toFixed(0)} KiB · 耗时 ${answer.totalMs} ms${answer.coldStart ? "（含模型冷启动）" : ""}`,
              }),
              el("code", { class: "path", dir: "ltr", text: answer.requestId }),
            );
            if (answer.presenters === 0) {
              toast("音频合成完成；当前未检测到活跃的字幕客户端，未触发本地播放。启动字幕端后重试即可收听。", "info");
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
            "检测到当前版本未识别的配置字段",
            el("p", {
              text: `${current.unknown.join("、")}。保存配置时将精准修改对应项，其余未识别字段保持原样保留。`,
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
      toast(`加载音色包配置失败：${ipcMessage(err)}`, "fail");
      return;
    }
    if (config === null) {
      fill(body, note("fail", "音色包未找到", el("p", { text: `注册表中未找到音色包 ID：${pack.id}。` })));
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
