// 训练: the panel side of `scripts/training/`, which is where the work actually happens.
//
// Five steps run as one job — dataset, latents, train, samples, score — and installing the
// result is a sixth the user asks for separately, because training produces several
// candidates and picking one is a decision. The backend relays each step's own progress
// stream on `train://event`, in the shape bootstrap established, so this screen renders
// events and computes nothing: the step that measured a number is the step that reports it.
//
// Five cards, in the order the work happens: 准备 (can this machine do it), 语料 (what it
// will learn from), 参数 (the four knobs that are safe to move), 进度 (the live run), 结果
// (which checkpoint, and installing it). The progress card is built once and mutated in
// place — a fifty-minute run emits an event per training step, and rebuilding that subtree
// each time would move focus off the cancel button someone is reaching for.


import { el, fill, type Child } from "../dom";
import { formatElapsed } from "../format";
import { icon, type IconName } from "../icons";
import {
  cancelTraining,
  installTrainedPack,
  ipcMessage,
  onTrainEvent,
  pickFile,
  pickFolder,
  startTraining,
  trainingPreflight,
  trainingResult,
  TRAIN_STAGES,
  type TrainEvent,
  type TrainStage,
  type TrainingPreflight,
  type TrainingResult,
} from "../ipc";
import { refreshVoices, status } from "../state";
import { toast } from "../toast";
import {
  blockedButton,
  button,
  chip,
  emptyState,
  expander,
  field,
  note,
  panel,
  pathText,
  type Tone,
} from "../ui";
// ---------------------------------------------------------------------------------------

type StageState = "pending" | "running" | "ok" | "skip" | "fail";

/** Result-oriented, and free of the engine's vocabulary: "DACVAE" and "manifest" belong in
 *  the console, where someone who needs them is already looking. */
const STAGE_LABEL: Record<TrainStage, string> = {
  dataset: "检查语料",
  latents: "编码音频",
  train: "训练适配器",
  samples: "生成试听",
  score: "相似度评分",
  install: "安装音色包",
};

const STAGE_HINT: Record<TrainStage, string> = {
  dataset: "逐个读取片段，写出数据集与 QA 报告",
  latents: "用引擎自己的编解码器把音频编成潜变量",
  train: "占用显卡最久的一步",
  samples: "同一批文本、同一个随机种子，每个检查点各生成一遍",
  score: "与原始录音比较说话人相似度，在 CPU 上跑",
  install: "选定一个检查点，复制并登记为音色包",
};

const STATE_LABEL: Record<StageState, string> = {
  pending: "待执行",
  running: "进行中",
  ok: "完成",
  skip: "已跳过",
  fail: "失败",
};

const STATE_GLYPH: Record<StageState, IconName> = {
  pending: "circle-dashed",
  running: "spinner-gap",
  ok: "check-circle",
  skip: "recycle",
  fail: "warning-circle",
};

/** The stylesheet names these states in the vocabulary a wizard uses, not the one a job
 *  status uses (`app.css`: `--todo/--active/--done/--fail/--skip`). Mapping here rather than
 *  renaming either side keeps the job's own words in the job's code and the stylesheet's in
 *  the stylesheet — and without this map every state rule silently applies to nothing. */
const STATE_CLASS: Record<StageState, string> = {
  pending: "todo",
  running: "active",
  ok: "done",
  skip: "skip",
  fail: "fail",
};

/** Beyond this the console is scrollback nobody reads and DOM the window pays for on every
 *  layout. The full transcript of every step is in data\logs either way. */
const LOG_CAP = 2000;

/** `lora.yaml`'s own defaults, which are the values a reference run on an RTX 5060 Ti
 *  actually used. The panel starts from those rather than from round numbers. */
const DEFAULTS = { batch: 16, steps: 2000, rate: 0.0001, save: 500 } as const;

/** Below this, batch 16 at bf16 does not fit and the warning is worth showing before an hour
 *  of GPU time ends in an out-of-memory. */
const BATCH16_MIB = 15000;

interface StageModel {
  state: StageState;
  message: string;
  done: number | null;
  total: number | null;
  startedAt: number | null;
  endedAt: number | null;
}

interface WizardRow {
  root: HTMLElement;
  dot: HTMLElement;
  label: HTMLElement;
  sub: HTMLElement;
}

function blankStage(): StageModel {
  return { state: "pending", message: "", done: null, total: null, startedAt: null, endedAt: null };
}

export interface TrainingScreen extends HTMLElement {
  /** Lives in the shell, below the scroll region, so 取消 cannot scroll away mid-run. */
  commandBar: HTMLElement;
}

export function createTrainingScreen(): TrainingScreen {
  const stages: Record<TrainStage, StageModel> = {
    dataset: blankStage(),
    latents: blankStage(),
    train: blankStage(),
    samples: blankStage(),
    score: blankStage(),
    install: blankStage(),
  };

  let preflight: TrainingPreflight | null = null;
  let result: TrainingResult | null = null;
  let running = false;
  let ticker = 0;
  let autoScroll = true;
  let chosen: string | null = null;
  /** Which voice the live job belongs to. The id field is editable, and an event arriving
   *  after someone started typing in it must still read the running job's directory. */
  let livePack: string | null = null;
  let audioDir: string | null = null;
  let transcripts: string | null = null;
  let avatar: string | null = null;

  // ------------------------------------------------------------------------------ 准备
  const ready = expander({ title: "准备", id: "train-ready", open: true });

  function readyRow(glyph: IconName, label: string, detail: string, right: Child): HTMLElement {
    return el(
      "div",
      { class: "inv" },
      icon(glyph, "inv__icon"),
      el(
        "div",
        { class: "inv__main" },
        el("p", { class: "inv__label", text: label }),
        el("p", { class: "inv__value", text: detail }),
      ),
      right,
    );
  }

  function renderReady(): void {
    if (preflight === null) {
      fill(ready.body, el("p", { class: "field__hint", text: "正在检查本机环境…" }));
      ready.tail.textContent = "检查中";
      return;
    }
    const state = preflight;
    const vram =
      state.vram_total_mib === null
        ? "未知"
        : `${gib(state.vram_free_mib)} 可用 / ${gib(state.vram_total_mib)}`;

    fill(
      ready.body,
      el(
        "div",
        { class: "inv" },
        icon("terminal-window", "inv__icon"),
        el(
          "div",
          { class: "inv__main" },
          el("p", { class: "inv__label", text: "训练用 Python" }),
          state.python === null
            ? el("p", { class: "inv__value", text: "没有找到解释器" })
            : el("div", { class: "inv__value" }, pathText(state.python, 64)),
        ),
        state.python === null
          ? chip("缺失", "fail", "warning-circle")
          : chip("就绪", "ok", "check-circle"),
      ),
      readyRow(
        "file-code",
        "训练依赖",
        state.missing.length === 0
          ? "torch、datasets、peft、soundfile、resemblyzer、yaml 均可导入"
          : `缺少 ${state.missing.join("、")}`,
        state.missing.length === 0
          ? chip("齐全", "ok", "check-circle")
          : chip(`缺 ${state.missing.length} 个`, "fail", "warning-circle"),
      ),
      readyRow(
        "graphics-card",
        "显卡",
        state.gpu_name === null
          ? "torch 看不到 CUDA 设备"
          : `${state.gpu_name} · CUDA ${state.cuda ?? "?"} · 显存 ${vram}`,
        state.gpu_name === null
          ? chip("不可用", "fail", "warning-circle")
          : chip("可用", "ok", "check-circle"),
      ),
      readyRow(
        "pulse",
        "后端占用",
        state.runtime_reachable
          ? state.model_loaded
            ? "后端正持有模型；第一个用显卡的步骤开始前会自动请求它释放"
            : "后端在运行，但没有加载模型"
          : "后端没在运行，显卡是空的",
        state.model_loaded
          ? chip("持有显存", "warn", "warning")
          : chip("无占用", "ok", "check-circle"),
      ),
      ...state.blockers.map((blocker) => note("fail", "不能开始训练", el("p", { text: blocker }))),
      state.vram_total_mib !== null && state.vram_total_mib < BATCH16_MIB
        ? note(
            "warn",
            "显存可能不够",
            el("p", {
              text: `批大小 16 在 bf16 下约需 14 GiB，这张卡是 ${gib(state.vram_total_mib)}。先把「批大小」调小，而不是硬跑。`,
            }),
          )
        : null,
    );
    ready.tail.textContent =
      state.blockers.length === 0 ? "可以开始" : `${state.blockers.length} 项阻塞`;
  }

  // ------------------------------------------------------------------------------ 语料
  const corpus = panel({
    title: "语料",
    hint: "一个文件夹的片段，加上每个片段的文本。48 kHz 单声道 16 位 WAV 最不损失什么；其他格式引擎会自己重采样。",
  });
  const audioValue = el("div", { class: "inv__value" });
  const transcriptValue = el("div", { class: "inv__value" });
  const transcriptSlot = el("span", {});
  const speakerInput = el("input", { class: "input", type: "text", placeholder: "my-voice" });
  const qaBody = el("div", { class: "panel__body" });

  function renderCorpus(): void {
    fill(
      audioValue,
      audioDir === null ? el("span", { text: "还没有选择" }) : pathText(audioDir, 64),
    );
    fill(
      transcriptValue,
      transcripts === null
        ? el("span", { text: "不选则使用音频旁边的 <片段名>.txt" })
        : pathText(transcripts, 64),
    );
    fill(
      transcriptSlot,
      transcripts === null
        ? button({
            label: "选择",
            glyph: "file-code",
            small: true,
            kind: "quiet",
            onClick: () => {
              void pickFile("选择文本对照文件", ["jsonl", "json", "csv", "tsv"])
                .then((picked) => {
                  if (picked === null) return;
                  transcripts = picked;
                  renderCorpus();
                })
                .catch((err: unknown) => toast(ipcMessage(err), "fail"));
            },
          })
        : button({
            glyph: "x",
            name: "清除文本对照",
            title: "清除",
            small: true,
            kind: "quiet",
            onClick: () => {
              transcripts = null;
              renderCorpus();
            },
          }),
    );
  }

  function renderQa(): void {
    const qa = result?.qa ?? null;
    if (qa === null) {
      fill(
        qaBody,
        el("p", {
          class: "field__hint",
          text: "「检查语料」这一步完成后，这里显示它量出来的数字。",
        }),
      );
      return;
    }
    fill(
      qaBody,
      el(
        "div",
        { class: "metrics" },
        metric("可用片段", String(qa.count)),
        metric("总时长", `${qa.total_minutes} 分钟`),
        metric("时长 p05 / p95", `${qa.duration_p05_s}s / ${qa.duration_p95_s}s`),
        metric("最长", `${qa.duration_max_s}s`),
        metric("采样率", `${qa.sample_rates.join("、")} Hz`),
        metric("声道", qa.channels.join("、")),
        metric("编码", qa.subtypes.join("、")),
      ),
      qa.problems.length === 0
        ? null
        : note(
            "warn",
            `${qa.problems.length} 个片段有问题，但仍会参与训练`,
            findings(qa.problems.map((item) => `${item.clip}: ${item.issue}`)),
          ),
      qa.skipped.length === 0
        ? null
        : note(
            "info",
            `${qa.skipped.length} 个片段被跳过`,
            findings(qa.skipped.map((item) => `${item.clip}: ${item.reason}`)),
          ),
    );
  }

  fill(
    corpus.body,
    el(
      "div",
      { class: "inv" },
      icon("folder-open", "inv__icon"),
      el(
        "div",
        { class: "inv__main" },
        el("p", { class: "inv__label", text: "音频文件夹" }),
        audioValue,
      ),
      button({
        label: "选择",
        glyph: "folder-open",
        small: true,
        onClick: () => {
          void pickFolder("选择音频文件夹")
            .then((picked) => {
              if (picked === null) return;
              audioDir = picked;
              renderCorpus();
              renderControls();
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
    ),
    el(
      "div",
      { class: "inv" },
      icon("file-code", "inv__icon"),
      el(
        "div",
        { class: "inv__main" },
        el("p", { class: "inv__label", text: "文本对照（可选）" }),
        transcriptValue,
      ),
      transcriptSlot,
    ),
    field(
      "train-speaker",
      "说话人标识",
      speakerInput,
      "训练 LoRA 一定要填：它是让训练器把同一个人的另一个片段当成参考音频的唯一依据。留空对 LoRA 是错的。",
    ),
    qaBody,
  );

  // ------------------------------------------------------------------------------ 参数
  const knobs = expander({ title: "参数", id: "train-knobs", open: false, tail: "四项" });
  const batchInput = numberInput(DEFAULTS.batch, 1, 64, 1);
  const stepsInput = numberInput(DEFAULTS.steps, 100, 20000, 100);
  const rateInput = numberInput(DEFAULTS.rate, 0.000001, 0.01, 0.00001);
  const saveInput = numberInput(DEFAULTS.save, 50, 20000, 50);

  fill(
    knobs.body,
    field(
      "train-batch",
      "批大小",
      batchInput,
      "默认 16：bf16 下约需 14 GiB 显存，参考机器的 16 GiB 刚好装得下。显存不足先降这一项。",
    ),
    field(
      "train-steps",
      "步数预算",
      stepsInput,
      "默认 2000：在 RTX 5060 Ti 上约 50–90 分钟。参考运行里最好的验证损失出现在 1000 步，2000 步已经过拟合——所以这是预算，不是答案。",
    ),
    field(
      "train-rate",
      "学习率",
      rateInput,
      "默认 0.0001，上游默认值。调大收敛更快，也更容易把音色学坏；共享的文本编码器另有固定的 0.00001，不受这里影响。",
    ),
    field(
      "train-save",
      "检查点间隔",
      saveInput,
      "默认每 500 步存一个检查点，每个约 100 MiB。验证固定每 500 步一次，最好的那个由验证损失挑出来。",
    ),
    note(
      "info",
      "其余参数是固定的",
      el("p", {
        text: "num_workers 2 与 persistent_workers 是 Windows 上不让显卡饿着的设置，model 段必须与基础检查点逐字一致——两者都不在这里暴露。",
      }),
    ),
  );

  // ------------------------------------------------------------------------------ 进度
  const progress = panel({ title: "进度" });

  function wizardRow(index: number, stage: TrainStage): WizardRow {
    const dot = icon(STATE_GLYPH.pending, "wizard__dot");
    const num = el("span", { class: "wizard__num" }, dot);
    const label = el("p", { class: "wizard__label", text: STAGE_LABEL[stage] });
    const sub = el("p", { class: "wizard__sub", text: STAGE_HINT[stage] });
    const root = el(
      "div",
      {
        class: `wizard__step wizard__step--${STATE_CLASS.pending}`,
        "aria-label": `第 ${index} 步：${STAGE_LABEL[stage]}`,
      },
      num,
      el("div", { class: "wizard__body" }, label, sub),
    );
    return { root, dot: num, label, sub };
  }

  const wizardRows: Record<TrainStage, WizardRow> = {
    dataset: wizardRow(1, "dataset"),
    latents: wizardRow(2, "latents"),
    train: wizardRow(3, "train"),
    samples: wizardRow(4, "samples"),
    score: wizardRow(5, "score"),
    install: wizardRow(6, "install"),
  };

  const metrics = el("div", { class: "metrics" });
  const bar = el("progress", { class: "bar", max: "1", value: "0" });
  const barText = el("span", { class: "metric__label" });
  const barCell = el("div", { class: "metric", hidden: true }, bar, barText);
  const remedySlot = el("div", {});
  const logLines = el("ol", {
    class: "console",
    role: "log",
    // Deliberately not a live region: the train stage scrolls faster than speech, and
    // announcing it would bury the stage transitions that matter.
    "aria-live": "off",
    tabindex: "0",
    onscroll: () => {
      autoScroll = logLines.scrollHeight - logLines.clientHeight - logLines.scrollTop < 24;
    },
  });

  fill(
    progress.body,
    el("div", { class: "wizard" }, TRAIN_STAGES.map((stage) => wizardRows[stage].root)),
    metrics,
    barCell,
    remedySlot,
    logLines,
  );

  function renderWizard(): void {
    for (const stage of TRAIN_STAGES) {
      const model = stages[stage];
      const row = wizardRows[stage];
      row.root.className = `wizard__step wizard__step--${STATE_CLASS[model.state]}`;
      fill(row.dot, icon(STATE_GLYPH[model.state], "wizard__dot"));
      const elapsed =
        model.startedAt === null || model.state === "pending"
          ? ""
          : ` · ${formatElapsed((model.endedAt ?? Date.now()) - model.startedAt)}`;
      row.label.textContent = `${STAGE_LABEL[stage]}${elapsed}`;
      row.sub.textContent =
        model.state === "pending"
          ? STAGE_HINT[stage]
          : model.message === ""
            ? STATE_LABEL[model.state]
            : model.message;
    }
  }

  /** The running stage's message, in cells, plus the bar.
   *
   *  Splitting is layout, not parsing: `run_training.py` writes
   *  `step 100/2000   loss 0.8147   2.38s/step   ETA 1:15:13`, self-labelling fields
   *  separated by three spaces, because it is the side that read the tqdm bar. Nothing here
   *  goes near a bar. */
  function renderMetrics(): void {
    const live = TRAIN_STAGES.map((stage) => stages[stage]).find(
      (model) => model.state === "running",
    );
    if (live === undefined || live.message === "") {
      fill(metrics);
      barCell.hidden = true;
      return;
    }
    fill(metrics, ...live.message.split(/\s{3,}/).map((cell) => metric(null, cell)));

    if (live.done === null) {
      barCell.hidden = true;
      return;
    }
    barCell.hidden = false;
    if (live.total !== null && live.total > 0) {
      bar.max = live.total;
      bar.value = live.done;
      barText.textContent = `${live.done} / ${live.total}`;
    } else {
      // No total yet: an indeterminate bar is honest, a bar pinned at 0 is not.
      bar.removeAttribute("value");
      barText.textContent = `已完成 ${live.done}`;
    }
  }

  function appendLog(event: TrainEvent): void {
    logLines.appendChild(
      el(
        "li",
        { class: `console__line console__line--${event.event}` },
        el("code", { class: "console__stage", dir: "ltr", text: event.stage }),
        el("span", { class: "console__text", text: event.message }),
      ),
    );
    while (logLines.childElementCount > LOG_CAP && logLines.firstChild !== null) {
      logLines.removeChild(logLines.firstChild);
    }
    if (autoScroll) logLines.scrollTop = logLines.scrollHeight;
  }

  // ------------------------------------------------------------------------------ 结果
  const results = panel({
    title: "结果",
    hint: "按验证损失排序，最好的一个已经选中。相似度看「下界」而不是均值：均值会藏住偶尔跑偏的那几句。",
  });
  const idInput = el("input", { class: "input", type: "text", placeholder: "my-voice" });
  const nameInput = el("input", { class: "input", type: "text", placeholder: "My Voice (LoRA)" });
  const characterInput = el("input", { class: "input", type: "text" });
  const avatarValue = el("div", { class: "inv__value" });
  const table = el("div", { class: "table" });
  const installSlot = el("div", { class: "panel__actions" });
  const overwriteBox = el("input", { type: "checkbox" });
  const overwriteSlot = el("div", {});

  function renderAvatar(): void {
    fill(
      avatarValue,
      avatar === null ? el("span", { text: "不选则音色包没有头像" }) : pathText(avatar, 64),
    );
  }

  /** The confirmation that lets a second run of this voice delete the first one's work.
   *
   *  Rendered beside the very table it would empty, and only while there is something to
   *  lose: `at_risk` counts the checkpoints no pack was installed from, and the backend
   *  refuses the call until this is ticked whatever the panel thinks. */
  function renderOverwrite(): void {
    const risk = result?.at_risk ?? 0;
    if (risk === 0) {
      overwriteBox.checked = false;
      fill(overwriteSlot);
      return;
    }
    fill(
      overwriteSlot,
      note(
        "warn",
        `上一次训练留下 ${risk} 个还没有安装的检查点`,
        el("p", {
          text: "「开始训练」会先清空这个音色的暂存目录，那些检查点就没有了。想留下哪一个，先在上面选中它并点「安装为音色包」。",
        }),
        field(
          "train-overwrite",
          `我知道，开始训练时删掉这 ${risk} 个检查点`,
          overwriteBox,
        ),
      ),
    );
  }

  function renderResults(): void {
    renderOverwrite();
    const items = result?.checkpoints ?? [];
    if (items.length === 0) {
      fill(
        table,
        emptyState({
          glyph: "magic-wand",
          title: "还没有检查点",
          lines: [
            el("p", {
              text: "「训练适配器」每存下一个检查点，这里就多一行；评分完成后再补上相似度。",
            }),
          ],
        }),
      );
      fill(installSlot);
      return;
    }

    const best = items.find((item) => item.best) ?? items[0];
    if (chosen === null || !items.some((item) => item.path === chosen)) chosen = best.path;

    fill(
      table,
      el(
        "div",
        { class: "table__head" },
        el("span", { class: "table__cell", text: "检查点" }),
        el("span", { class: "table__cell", text: "步数" }),
        el("span", { class: "table__cell", text: "验证损失" }),
        el("span", { class: "table__cell", text: "相似度下界" }),
      ),
      ...items.map((item) =>
        el(
          "button",
          {
            class: `table__row${item.path === chosen ? " is-selected" : ""}`,
            type: "button",
            role: "radio",
            "aria-checked": String(item.path === chosen),
            onclick: () => {
              chosen = item.path;
              renderResults();
            },
          },
          el("span", { class: "table__cell", dir: "ltr", text: item.name }),
          el("span", { class: "table__cell", text: item.step === null ? "—" : String(item.step) }),
          el("span", {
            class: "table__cell",
            text: item.val_loss === null ? "—" : item.val_loss.toFixed(6),
          }),
          el(
            "span",
            { class: "table__cell" },
            item.lower_bound === null
              ? el("span", { text: "—" })
              : chip(item.lower_bound.toFixed(4), lowerBoundTone(item.lower_bound)),
          ),
        ),
      ),
    );

    fill(
      installSlot,
      running
        ? blockedButton({ label: "安装为音色包", glyph: "check" }, "有任务正在运行")
        : button({
            label: "安装为音色包",
            glyph: "check",
            kind: "primary",
            onClick: () => void install(),
          }),
    );
  }

  fill(
    results.body,
    overwriteSlot,
    table,
    field(
      "train-pack-id",
      "音色包 id",
      idInput,
      "会成为 voicepacks 下的目录名和 API 里的标识，只能用字母、数字、点、短横线和下划线。",
    ),
    field("train-pack-name", "显示名称", nameInput, "面板和字幕弹窗里显示的名字。留空则用 id。"),
    field("train-pack-character", "角色名", characterInput, "字幕弹窗显示的说话人。"),
    el(
      "div",
      { class: "inv" },
      icon("file-plus", "inv__icon"),
      el(
        "div",
        { class: "inv__main" },
        el("p", { class: "inv__label", text: "头像（可选）" }),
        avatarValue,
      ),
      button({
        label: "选择",
        glyph: "file-plus",
        small: true,
        kind: "quiet",
        onClick: () => {
          void pickFile("选择头像", ["png", "jpg", "jpeg", "webp", "bmp"])
            .then((picked) => {
              if (picked === null) return;
              avatar = picked;
              renderAvatar();
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
    ),
    installSlot,
  );

  // ------------------------------------------------------------------------- command bar
  const cmdLeft = el("div", { class: "cmdbar__left" });
  const cmdRight = el("div", { class: "cmdbar__right" });
  const commandBar = el("div", { class: "cmdbar" }, cmdLeft, cmdRight);

  /** Why the start button is not live, in one sentence, or null. The backend validates every
   *  one of these again — this is the half that can say so before the click. */
  function startBlocker(): string | null {
    if (preflight === null) return "正在检查本机环境";
    if (preflight.blockers.length > 0) return preflight.blockers[0];
    if (audioDir === null) return "先选择音频文件夹";
    if (idInput.value.trim() === "") return "先填音色包 id";
    if ((result?.at_risk ?? 0) > 0 && !overwriteBox.checked) {
      return `先安装要保留的检查点，或勾选删掉这 ${result?.at_risk ?? 0} 个`;
    }
    return null;
  }

  function renderControls(): void {
    if (running) {
      fill(
        cmdLeft,
        button({
          label: "取消",
          kind: "danger",
          glyph: "x",
          onClick: () => {
            void cancelTraining().catch((err: unknown) => toast(ipcMessage(err), "fail"));
          },
        }),
      );
      // One sentence, only true while a job is in flight, which is exactly a tooltip.
      fill(cmdRight, blockedButton({ label: "进行中…" }, "关掉窗口不会中断训练"));
      renderResults();
      return;
    }

    fill(
      cmdLeft,
      button({
        label: "重新检查环境",
        glyph: "arrow-clockwise",
        kind: "quiet",
        onClick: () => void loadPreflight(),
      }),
    );
    const reason = startBlocker();
    fill(
      cmdRight,
      reason === null
        ? button({
            label: "开始训练",
            kind: "primary",
            glyph: "magic-wand",
            onClick: () => void start(),
          })
        : blockedButton({ label: "开始训练", glyph: "magic-wand" }, reason),
    );
    renderResults();
  }

  // ----------------------------------------------------------------------------- actions
  async function loadPreflight(): Promise<void> {
    try {
      preflight = await trainingPreflight();
    } catch (err: unknown) {
      toast(`训练环境检查失败：${ipcMessage(err)}`, "fail");
      return;
    }
    // A panel restarted mid-training re-attaches to the live job instead of offering to start
    // a second one. The event listener has been up since the first frame either way.
    if (preflight.running && preflight.pack_id !== null) {
      running = true;
      livePack = preflight.pack_id;
      if (idInput.value.trim() === "") idInput.value = preflight.pack_id;
      await loadResult(preflight.pack_id);
      restoreRequest();
      if (ticker === 0) ticker = window.setInterval(renderWizard, 250);
    }
    renderReady();
    renderControls();
  }

  async function loadResult(packId: string): Promise<void> {
    try {
      result = await trainingResult(packId);
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
      return;
    }
    renderQa();
    renderResults();
  }

  /** The fields the live run was started with, so a restarted panel does not ask the user to
   *  remember what they typed an hour ago. */
  function restoreRequest(): void {
    const request = result?.request ?? null;
    if (request === null) return;
    audioDir = request.audio_dir;
    transcripts = request.transcripts;
    speakerInput.value = request.speaker_id;
    batchInput.value = String(request.batch_size);
    stepsInput.value = String(request.max_steps);
    rateInput.value = String(request.learning_rate);
    saveInput.value = String(request.save_every);
    if (nameInput.value.trim() === "") nameInput.value = request.display_name;
    if (characterInput.value.trim() === "") characterInput.value = request.character ?? "";
    avatar = request.avatar;
    renderCorpus();
    renderAvatar();
  }

  async function start(): Promise<void> {
    if (running || audioDir === null) return;
    const packId = idInput.value.trim();
    livePack = packId;
    running = true;
    for (const stage of TRAIN_STAGES) stages[stage] = blankStage();
    result = null;
    chosen = null;
    fill(logLines);
    fill(remedySlot);
    // The checklist did its job; the spotlight belongs on the steps now.
    ready.setOpen(false);
    knobs.setOpen(false);
    renderQa();
    renderWizard();
    renderMetrics();
    renderControls();
    if (ticker === 0) ticker = window.setInterval(renderWizard, 250);

    try {
      await startTraining({
        audio_dir: audioDir,
        transcripts,
        speaker_id: speakerInput.value.trim(),
        pack_id: packId,
        display_name: nameInput.value.trim(),
        character: characterInput.value.trim() || null,
        avatar,
        batch_size: knob(batchInput, DEFAULTS.batch),
        max_steps: knob(stepsInput, DEFAULTS.steps),
        learning_rate: knob(rateInput, DEFAULTS.rate),
        save_every: knob(saveInput, DEFAULTS.save),
        overwrite: overwriteBox.checked,
      });
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
    } finally {
      await settle(packId);
    }
  }

  async function install(): Promise<void> {
    if (chosen === null) return;
    const packId = idInput.value.trim();
    livePack = packId;
    running = true;
    stages.install = blankStage();
    renderWizard();
    renderControls();
    if (ticker === 0) ticker = window.setInterval(renderWizard, 250);
    try {
      await installTrainedPack({
        checkpoint: chosen,
        pack_id: packId,
        display_name: nameInput.value.trim(),
        character: characterInput.value.trim() || null,
        avatar,
      });
      await refreshVoices();
      toast(`${packId} 已安装，后端会在下次列出音色时读到它`, "ok");
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
    } finally {
      await settle(packId);
    }
  }

  /** The job's processes are gone. A stage still marked running was cancelled or died without
   *  a terminal event, and leaving it spinning forever would be a lie. */
  async function settle(packId: string): Promise<void> {
    running = false;
    if (ticker !== 0) {
      window.clearInterval(ticker);
      ticker = 0;
    }
    for (const stage of TRAIN_STAGES) {
      const model = stages[stage];
      if (model.state !== "running") continue;
      model.state = "pending";
      model.message = "已中断";
      model.endedAt = Date.now();
    }
    if (packId !== "") await loadResult(packId);
    renderWizard();
    renderMetrics();
    renderControls();
    void loadPreflight();
  }

  function apply(event: TrainEvent): void {
    const model = stages[event.stage] as StageModel | undefined;
    if (model === undefined) return;

    if (event.event === "progress") {
      model.done = event.done;
      model.total = event.total;
      if (event.message !== "") model.message = event.message;
      // Every training step arrives here, so this path touches the wizard row's text and the
      // bar and nothing else.
      renderWizard();
      renderMetrics();
      return;
    }

    appendLog(event);

    if (event.event === "start") {
      model.state = "running";
      model.message = event.message;
      model.done = null;
      model.total = null;
      model.startedAt = event.ts;
      model.endedAt = null;
    } else if (event.event === "log") {
      if (event.message !== "") model.message = event.message;
      if (event.remedy !== null) {
        fill(remedySlot, note("warn", event.message, el("p", { class: "remedy", text: event.remedy })));
      }
    } else {
      model.state = event.event;
      model.message = event.message;
      model.endedAt = event.ts;
      if (model.startedAt === null) model.startedAt = event.ts;
      fill(
        remedySlot,
        event.remedy === null
          ? null
          : note("fail", "怎么解决", el("p", { class: "remedy", text: event.remedy })),
      );
      // A finished step wrote files this screen shows: the QA report after `dataset`, the
      // checkpoints after `train`, their scores after `score`.
      if (livePack !== null && REREAD[event.stage] === true) void loadResult(livePack);
    }
    renderWizard();
    renderMetrics();
  }

  const root = el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: "训练" }),
      ),
    ),
    ready.root,
    corpus.root,
    knobs.root,
    progress.root,
    results.root,
  );

  idInput.addEventListener("input", renderControls);
  // On commit rather than per keystroke: this asks the backend what the named run left on
  // disk, which is how the confirmation appears before the click rather than after it.
  idInput.addEventListener("change", () => {
    const packId = idInput.value.trim();
    if (running || packId === "") return;
    void loadResult(packId);
  });
  overwriteBox.addEventListener("change", renderControls);

  renderCorpus();
  renderAvatar();
  renderQa();
  renderWizard();
  renderMetrics();
  renderReady();
  renderControls();
  void onTrainEvent(apply);
  void loadPreflight();
  // 后端占用 is a claim about the card, and it goes stale the moment somebody starts or stops
  // the service from another screen — which is exactly what a user does right before training.
  // On the transition only: `status` polls every second, and preflight probes the interpreter,
  // imports six packages and asks the driver, which is not a once-per-second question.
  let backendUp = status.value.reachable;
  status.subscribe((next) => {
    if (next.reachable === backendUp) return;
    backendUp = next.reachable;
    if (!running) void loadPreflight();
  });

  return Object.assign(root, { commandBar });
}

// ------------------------------------------------------------------------------ helpers --

/** Which finished stages left a file on disk worth re-reading. */
const REREAD: Partial<Record<TrainStage, true>> = { dataset: true, train: true, score: true };

function metric(label: string | null, value: string): HTMLElement {
  return el(
    "div",
    { class: "metric" },
    label === null ? null : el("span", { class: "metric__label", text: label }),
    el("span", { class: "metric__value", text: value }),
  );
}

function numberInput(value: number, min: number, max: number, step: number): HTMLInputElement {
  return el("input", {
    class: "input",
    type: "number",
    min: String(min),
    max: String(max),
    step: String(step),
    value: String(value),
  });
}

/** A number the user may have emptied. The backend rejects an out-of-range knob either way;
 *  this only keeps a blank field from becoming NaN on the wire. */
function knob(input: HTMLInputElement, fallback: number): number {
  const parsed = Number(input.value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

/** The QA report's own cut: past ten, a list of findings stops being something a human reads.
 *  All of them are in the report on disk. */
function findings(lines: string[]): HTMLElement {
  return el(
    "ul",
    { class: "stage__notes" },
    lines.slice(0, 10).map((line) => el("li", { class: "stage__note" }, el("p", { text: line }))),
    lines.length > 10
      ? el("li", { class: "stage__note" }, el("p", { text: `…以及另外 ${lines.length - 10} 条` }))
      : null,
  );
}

/** 0.6 is not a threshold the engine has; it is where the reference corpus's own leave-one-out
 *  p10 sat (0.704) minus the room a good run needs, so it separates "worth listening to" from
 *  "look at this before installing it". */
function lowerBoundTone(value: number): Tone {
  return value >= 0.6 ? "ok" : "warn";
}

function gib(mib: number | null): string {
  return mib === null ? "未知" : `${(mib / 1024).toFixed(1)} GiB`;
}
