// 训练: the panel side of `scripts/training/`, which is where the work happens — and where it
// is started. An agent runs the six steps; this screen is what the human watches.
//
// That is the inversion this file exists to serve. The pipeline is an hour of GPU time driven
// from a shell, so the panel does not own it: every step writes its own record
// (`--status-file`, `scripts/training/_layout.py`), and this screen reads
// `data\logs\training-<pack id>.status.json` plus the `.jsonl` beside it. It therefore shows
// runs it never started — which is the normal case — and there is no start button, no corpus
// picker and no knobs, because none of those decisions are made here.
//
// So the page is one thing: observability. Which runs exist, which stage each reached, the live
// metric strip, the console, the checkpoints, and the files on disk.
//
// There is no handover-prompt card here, and re-adding one is a regression. It held some sixty
// lines of prose and PowerShell — the six-step pipeline, the `--json` protocol, the two
// constraints that cost real time when ignored — which is exactly what
// `skills/voice-core-voice-training/SKILL.md` is, a file that ships inside the install and is
// written to be read by an agent. A window cannot keep a second copy of that in sync with it,
// and a wall of text is not an interface: the 状态 screen's 使用说明 card hands an agent that
// file in one sentence, and that is the whole handover.
//
// The one act left is installing a chosen checkpoint, and it is here because choosing among
// candidates is a human judgement — the 训练成果 table exists to serve exactly that.
//
// Nothing here explains itself in prose. A label plus its value is the explanation; visible
// text is reserved for a failure with its remedy, a destructive confirmation, and an empty
// state saying what will appear here.

import { el, fill } from "../dom";
import { formatBytes, formatDuration, formatElapsed } from "../format";
import { icon, type IconName } from "../icons";
import {
  installTrainedPack,
  ipcMessage,
  trainingDiscard,
  trainingLog,
  trainingRuns,
  trainingScratch,
  TRAIN_STAGES,
  type ScratchEntry,
  type ScratchTree,
  type TrainingRun,
  type TrainingStageStatus,
  type TrainStage,
} from "../ipc";
import { refreshVoices } from "../state";
import { toast } from "../toast";
import {
  blockedButton,
  button,
  chip,
  emptyState,
  note,
  openButton,
  panel,
  pathText,
  type Tone,
} from "../ui";

/** Result-oriented, and free of the engine's vocabulary: "DACVAE" and "manifest" belong in the
 *  console, where someone who needs them is already looking. */
const STAGE_LABEL: Record<TrainStage, string> = {
  dataset: "语料校验",
  latents: "音频特征提取",
  train: "训练适配器",
  samples: "生成试听样本",
  score: "音色相似度评分",
  install: "安装音色包",
};

/** What the step produces, not what it does. A stage's own `message` replaces this the moment
 *  it starts, so this line is only ever read before that step has run. */
const STAGE_HINT: Record<TrainStage, string> = {
  dataset: "dataset.jsonl 与质检报告",
  latents: "DACVAE 潜变量 (.pt) 与训练清单",
  train: "LoRA 权重训练（高 GPU 负载阶段）",
  samples: "固定随机种子 · 每个检查点各一组样本",
  score: "GE2E d-vector · CPU",
  install: "归档检查点并生成 voicepack.json",
};

/** The words the status file uses, in the words the screen uses. `interrupted` is the one the
 *  event stream cannot produce: a stage whose process died without a terminal event. */
const STATE_LABEL: Record<string, string> = {
  pending: "待执行",
  running: "进行中",
  ok: "已完成",
  skip: "已跳过",
  fail: "失败",
  interrupted: "已中断",
};

const STATE_GLYPH: Record<string, IconName> = {
  pending: "circle-dashed",
  running: "spinner-gap",
  ok: "check-circle",
  skip: "recycle",
  fail: "warning-circle",
  interrupted: "stop",
};

/** The stylesheet names these states in the vocabulary a wizard uses, not the one a job status
 *  uses (`app.css`: `--todo/--active/--done/--fail/--skip`). Mapping here rather than renaming
 *  either side keeps the file's own words in the file's reader and the stylesheet's in the
 *  stylesheet — and without this map every state rule silently applies to nothing.
 *  `interrupted` draws as `todo`: nothing is happening in that step, and it is not a failure. */
const STATE_CLASS: Record<string, string> = {
  pending: "todo",
  running: "active",
  ok: "done",
  skip: "skip",
  fail: "fail",
  interrupted: "todo",
};

const STATE_TONE: Record<string, Tone> = {
  pending: "idle",
  running: "run",
  ok: "ok",
  skip: "reuse",
  fail: "fail",
  interrupted: "warn",
};

/** Beyond this the console is scrollback nobody reads and DOM the window pays for on every
 *  layout. The full transcript is the `.jsonl` either way, and the file panel opens it. */
const LOG_CAP = 2000;

/** How often the two files are re-read. The status files are a couple of KiB each and the
 *  transcript read resumes at a byte offset, so a poll costs a seek; the scratch tree is
 *  measured only when a stage changes, because walking `latents\` is thousands of stats. */
const POLL_MS = 2000;

/** What each file of a run is for. Keyed by the name on disk because that is what the backend
 *  reports — it measures files, it does not name them in Chinese. */
const ARTEFACT: Record<string, string> = {
  "dataset.jsonl": "数据集清单 (dataset.jsonl)",
  "dataset.jsonl.qa.json": "语料质检报告 (dataset.jsonl.qa.json)",
  "train_manifest.jsonl": "训练样本清单 (train_manifest.jsonl)",
  latents: "潜变量特征目录 (latents)",
  lora: "模型检查点目录 (lora)",
  samples: "试听音频样本 (samples)",
  score: "相似度评估报告 (score)",
  "installed.txt": "安装记录 (installed.txt)",
};

/** Directories all draw as a folder — which of them is which is what the label is for. */
const ARTEFACT_GLYPH: Record<string, IconName> = {
  "dataset.jsonl": "database",
  "dataset.jsonl.qa.json": "info",
  "train_manifest.jsonl": "database",
  "installed.txt": "check",
};

export interface TrainingScreen extends HTMLElement {
  /** Lives in the shell, below the scroll region, so the one action on this page cannot scroll
   *  away. */
  commandBar: HTMLElement;
}

export function createTrainingScreen(): TrainingScreen {
  let runs: TrainingRun[] = [];
  /** Which run the observability half is about. The live one by default. */
  let selected: string | null = null;
  let tree: ScratchTree | null = null;
  /** Which checkpoint 安装为音色包 would install. */
  let chosen: string | null = null;
  /** Where the console has read up to in the transcript, in bytes. */
  let logOffset = 0;
  /** `stage|state|live` at the last tree measurement, so the walk happens on a boundary rather
   *  than once per poll. */
  let treeKey = "";
  /** The backend's refusal to delete a scratch tree, held so the panel can show it beside the
   *  button that would override it. */
  let discardRefusal: string | null = null;
  /** An install is in flight. Nothing else on this page starts a process. */
  let installing = false;
  let autoScroll = true;
  let ticker = 0;

  function current(): TrainingRun | null {
    return runs.find((run) => run.pack_id === selected) ?? null;
  }

  // ---------------------------------------------------------------------------- 运行
  // Every run with a status file, including the ones this window never saw start.
  const runsPanel = panel({ title: "训练运行" });
  const runsTable = el("div", { class: "table" });
  const runsState = el("div", { class: "panel__body" });

  function renderRuns(): void {
    if (runs.length === 0) {
      fill(
        runsTable,
        emptyState({
          glyph: "magic-wand",
          title: "暂无训练运行",
          lines: [
            el("p", {
              text: "让 agent 按 voice-core-voice-training 技能执行训练，它每一步写下的状态文件会让这次运行出现在这里。",
            }),
          ],
        }),
      );
      fill(runsState);
      return;
    }

    fill(
      runsTable,
      el(
        "div",
        { class: "table__head" },
        el("span", { class: "table__cell", text: "音色包 ID" }),
        el("span", { class: "table__cell", text: "阶段" }),
        el("span", { class: "table__cell", text: "状态" }),
        el("span", { class: "table__cell", text: "更新于" }),
      ),
      ...runs.map((run) =>
        el(
          "button",
          {
            class: `table__row${run.pack_id === selected ? " is-selected" : ""}`,
            type: "button",
            role: "radio",
            "aria-checked": String(run.pack_id === selected),
            onclick: () => void select(run.pack_id),
          },
          el("span", { class: "table__cell", dir: "ltr", text: run.pack_id }),
          el("span", { class: "table__cell", text: stageLabel(run.stage) }),
          el(
            "span",
            { class: "table__cell" },
            chip(stateLabel(run.state), STATE_TONE[run.state] ?? "idle"),
          ),
          el("span", {
            class: "table__cell",
            text: `${formatDuration(Math.max(0, Date.now() - run.updated))} 前`,
          }),
        ),
      ),
    );

    const run = current();
    if (run === null) {
      fill(runsState);
      return;
    }
    const position = run.total !== null && run.done !== null ? ` · ${run.done}/${run.total}` : "";
    const sentence = run.live
      ? `正在运行 · ${stageLabel(run.stage)}${position}`
      : run.failure !== null
        ? `上次运行在阶段「${stageLabel(run.failed_stage ?? "")}」失败`
        : run.state === "interrupted"
          ? `上次运行在阶段「${stageLabel(run.stage)}」被中断`
          : `上次运行至阶段「${stageLabel(run.stage)}」· ${stateLabel(run.state)}`;

    fill(
      runsState,
      el(
        "div",
        { class: run.live ? "livestate livestate--up" : "livestate" },
        el("span", { class: "livestate__dot" }),
        el("span", { text: sentence }),
        el("span", {
          class: "field__hint",
          text: `PID ${run.pid} · 更新于 ${formatDuration(Math.max(0, Date.now() - run.updated))} 前`,
        }),
      ),
      run.failure === null
        ? null
        : note(
            "fail",
            run.failure,
            el("p", {
              class: "remedy",
              text: run.remedy ?? "标准错误流 (stderr) 中包含详细错误堆栈",
            }),
          ),
    );
  }

  fill(runsPanel.body, runsTable, runsState);

  // ---------------------------------------------------------------------------- 进度
  const progress = panel({ title: "训练进度" });

  interface WizardRow {
    root: HTMLElement;
    dot: HTMLElement;
    label: HTMLElement;
    sub: HTMLElement;
  }

  function wizardRow(index: number, stage: TrainStage): WizardRow {
    const dot = icon(STATE_GLYPH.pending, "wizard__dot");
    const num = el("span", { class: "wizard__num" }, dot);
    const label = el("p", { class: "wizard__label", text: STAGE_LABEL[stage] });
    const sub = el("p", { class: "wizard__sub", text: STAGE_HINT[stage] });
    const root = el(
      "div",
      {
        class: `wizard__step wizard__step--${STATE_CLASS.pending}`,
        "aria-label": `步骤 ${index}：${STAGE_LABEL[stage]}`,
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

  const liveTiles = el("div", { class: "tiles tiles--compact" });
  const bar = el("progress", { class: "bar", max: "1", value: "0" });
  const barText = el("span", { class: "stage__bartext" });
  const progressWrap = el("div", { class: "stage__progress", hidden: true }, bar, barText);
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
    liveTiles,
    progressWrap,
    logLines,
  );

  /** The six steps, filled in from the record on disk. A panel opened forty minutes into a run
   *  someone else started shows forty minutes of run. */
  function renderWizard(): void {
    const run = current();
    for (const stage of TRAIN_STAGES) {
      const row = rowFor(run, stage);
      const view = wizardRows[stage];
      const state = row?.state ?? "pending";
      view.root.className = `wizard__step wizard__step--${STATE_CLASS[state] ?? "todo"}`;
      fill(view.dot, icon(STATE_GLYPH[state] ?? "circle-dashed", "wizard__dot"));
      const elapsed =
        row === null || row.started === null || state === "pending"
          ? ""
          : ` · ${formatElapsed((row.ended ?? Date.now()) - row.started)}`;
      view.label.textContent = `${STAGE_LABEL[stage]}${elapsed}`;
      view.sub.textContent =
        row === null || state === "pending"
          ? STAGE_HINT[stage]
          : row.message === ""
            ? stateLabel(state)
            : row.message;
    }
  }

  /** The running stage's message, in cells, plus the bar.
   *
   *  Splitting is layout, not parsing: `run_training.py` writes
   *  `step 100/2000   loss 0.8147   2.38s/step   ETA 1:15:13`, self-labelling fields separated
   *  by three spaces, because it is the side that read the tqdm bar. Nothing here goes near a
   *  bar. */
  function renderMetrics(): void {
    const live = current()?.stages.find((row) => row.state === "running") ?? null;
    if (live === null || live.message === "") {
      fill(liveTiles);
      progressWrap.hidden = true;
      return;
    }
    fill(liveTiles, ...live.message.split(/\s{3,}/).map((cell) => tile(cell)));

    if (live.done === null) {
      progressWrap.hidden = true;
      return;
    }
    progressWrap.hidden = false;
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

  /** One transcript line, as the step wrote it.
   *
   *  `progress` is skipped on purpose: a 2000-step run emits thousands of them and the live
   *  strip above already shows the latest. What is left is the stage boundaries and the
   *  sentences a step chose to say, which is what a console is for. */
  function appendLog(line: string): void {
    let event: { stage?: string; event?: string; message?: string };
    try {
      event = JSON.parse(line) as typeof event;
    } catch {
      // Not protocol: something wrote to this file that is not one of the six steps. Showing it
      // verbatim is more useful than dropping it.
      event = { stage: "", event: "log", message: line };
    }
    const kind = event.event ?? "log";
    if (kind === "progress") return;
    logLines.appendChild(
      el(
        "li",
        { class: `console__line console__line--${kind}` },
        el("code", { class: "console__stage", dir: "ltr", text: event.stage ?? "" }),
        el("span", { class: "console__text", text: event.message ?? "" }),
      ),
    );
    while (logLines.childElementCount > LOG_CAP && logLines.firstChild !== null) {
      logLines.removeChild(logLines.firstChild);
    }
    if (autoScroll) logLines.scrollTop = logLines.scrollHeight;
  }

  // ---------------------------------------------------------------------------- 成果
  const results = panel({
    title: "训练成果",
    hint: "已按验证损失升序排序，默认选中最优检查点",
  });
  const table = el("div", { class: "table" });

  function renderResults(): void {
    const items = tree?.checkpoints ?? [];
    if (items.length === 0) {
      chosen = null;
      fill(
        table,
        emptyState({
          glyph: "magic-wand",
          title: "暂无检查点生成",
          lines: [
            el("p", {
              text: "模型训练过程中将按保存间隔生成检查点，评分阶段将追加相似度评估指标。",
            }),
          ],
        }),
      );
      renderControls();
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
    renderControls();
  }

  fill(results.body, table);

  // ---------------------------------------------------------------------------- 文件
  // The run as it exists on disk. The two log files are named apart from the rest because a
  // discard keeps them: they are the record, and the tree is regenerable.
  const files = panel({ title: "产物文件" });
  const fileList = el("div", { class: "stage__detail" });
  const fileActions = el("div", { class: "panel__actions" });

  /** One measured file or directory: what it is, how big, and a way into it. */
  function fileRow(item: ScratchEntry, label: string): HTMLElement {
    const glyph: IconName = item.dir ? "folder-open" : (ARTEFACT_GLYPH[item.name] ?? "file-code");
    return el(
      "div",
      { class: "filerow" },
      el(
        "div",
        { class: "filerow__lead" },
        icon(glyph, "filerow__icon"),
        el("span", { class: "filerow__name", dir: "ltr", title: item.path, text: item.name }),
        el("span", {
          class: "field__hint",
          text: item.dir && item.exists ? `${label} · ${item.files} 个文件` : label,
        }),
      ),
      el(
        "div",
        { class: "filerow__tail" },
        el("span", {
          class: "filerow__size",
          text: item.exists ? formatBytes(item.bytes) : "未生成",
        }),
        item.exists ? openButton(item.path) : null,
      ),
    );
  }

  function renderFiles(): void {
    if (tree === null) {
      fill(fileList);
      fill(fileActions);
      return;
    }
    const scratch = tree;

    fill(
      fileList,
      scratch.exists
        ? el("div", {}, ...scratch.entries.map((item) => fileRow(item, ARTEFACT[item.name] ?? "")))
        : emptyState({
            glyph: "folder-open",
            title: "暂无暂存目录",
            lines: [pathText(scratch.dir, 72)],
          }),
      el("p", { class: "field__hint", text: "日志文件（清理暂存区时将予以保留）" }),
      el(
        "div",
        {},
        fileRow(scratch.transcript, "事件日志"),
        fileRow(scratch.status, "状态数据 (JSON)"),
      ),
      discardRefusal === null
        ? null
        : note(
            "warn",
            "暂存目录中存在未安装的检查点",
            el("p", { text: discardRefusal }),
            el(
              "div",
              { class: "panel__actions" },
              button({
                label: "确认清理并删除未保存检查点",
                glyph: "trash",
                kind: "danger",
                onClick: () => void discard(true),
              }),
            ),
          ),
      !scratch.exists
        ? null
        : el("p", {
            class: "panel__meta",
            text: `${formatBytes(scratch.bytes)} · ${scratch.dir}`,
          }),
    );

    const run = current();
    fill(
      fileActions,
      !scratch.exists
        ? null
        : run?.live === true
          ? blockedButton({ label: "清理暂存", glyph: "trash", small: true }, "该音色包正在训练")
          : button({
              label: "清理暂存",
              glyph: "trash",
              kind: "danger",
              small: true,
              onClick: () => void discard(false),
            }),
    );
  }

  fill(files.body, fileList, fileActions);

  // ------------------------------------------------------------------------- command bar
  const cmdLeft = el("div", { class: "cmdbar__left" });
  const cmdRight = el("div", { class: "cmdbar__right" });
  const commandBar = el("div", { class: "cmdbar" }, cmdLeft, cmdRight);

  /** Why 安装为音色包 is not live, in one sentence, or null. */
  function installBlocker(): string | null {
    if (installing) return "正在安装音色包";
    if (current() === null) return "请先选择一次训练运行";
    if (chosen === null) return "本次运行尚无可安装的检查点";
    return null;
  }

  function renderControls(): void {
    fill(
      cmdLeft,
      button({
        label: "刷新",
        glyph: "arrow-clockwise",
        kind: "quiet",
        onClick: () => void poll(),
      }),
    );
    const reason = installBlocker();
    fill(
      cmdRight,
      reason === null
        ? button({
            label: "安装为音色包",
            kind: "primary",
            glyph: "check",
            onClick: () => void install(),
          })
        : blockedButton({ label: "安装为音色包", glyph: "check" }, reason),
    );
  }

  // ----------------------------------------------------------------------------- actions
  /** One read of the runs, and of the selected run's transcript. */
  async function poll(): Promise<void> {
    try {
      runs = await trainingRuns();
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
      return;
    }
    if (selected === null || !runs.some((run) => run.pack_id === selected)) {
      selected = runs[0]?.pack_id ?? null;
      treeKey = "";
      logOffset = 0;
      fill(logLines);
    }
    renderRuns();
    renderWizard();
    renderMetrics();

    const run = current();
    if (run === null) {
      tree = null;
      renderResults();
      renderFiles();
      stopTicker();
      return;
    }
    // A stage boundary is where the tree on disk changed shape; between boundaries measuring it
    // again would walk `latents\` for the same answer.
    const key = `${run.stage}|${run.state}|${String(run.live)}`;
    if (key !== treeKey) {
      treeKey = key;
      await refreshTree(run.pack_id);
    }
    await pumpLog(run.pack_id);
    // A live run's stage elapsed time moves between polls; a finished one's does not.
    if (run.live) startTicker();
    else stopTicker();
  }

  async function select(packId: string): Promise<void> {
    if (packId === selected) return;
    selected = packId;
    chosen = null;
    tree = null;
    treeKey = "";
    logOffset = 0;
    discardRefusal = null;
    autoScroll = true;
    fill(logLines);
    renderRuns();
    renderWizard();
    renderMetrics();
    await poll();
  }

  async function refreshTree(packId: string): Promise<void> {
    try {
      tree = await trainingScratch(packId);
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
      return;
    }
    renderResults();
    renderFiles();
  }

  async function pumpLog(packId: string): Promise<void> {
    let tailed;
    try {
      tailed = await trainingLog(packId, logOffset);
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
      return;
    }
    // The transcript shrank: the first stage of a new run truncated it, so the console is
    // showing a run that no longer exists.
    if (tailed.offset < logOffset) fill(logLines);
    logOffset = tailed.offset;
    for (const line of tailed.lines) appendLog(line);
  }

  async function install(): Promise<void> {
    const run = current();
    if (run === null || chosen === null || installing) return;
    installing = true;
    renderControls();
    try {
      await installTrainedPack({ checkpoint: chosen, pack_id: run.pack_id });
      await refreshVoices();
      toast(`音色包 ${run.pack_id} 已安装，将在下次服务加载时生效`, "ok");
    } catch (err: unknown) {
      toast(ipcMessage(err), "fail");
    } finally {
      installing = false;
      treeKey = "";
      await poll();
    }
  }

  /** Delete one run's scratch tree.
   *
   *  `confirmed` is asked for the way the backend asks: it refuses first and names what would
   *  be lost, and that refusal is what the panel puts in front of the user. */
  async function discard(confirmed: boolean): Promise<void> {
    const run = current();
    if (run === null) return;
    try {
      const freed = await trainingDiscard(run.pack_id, confirmed);
      discardRefusal = null;
      toast(`已清理 ${run.pack_id} 的暂存目录，释放存储空间 ${formatBytes(freed)}`, "ok");
    } catch (err: unknown) {
      discardRefusal = ipcMessage(err);
      renderFiles();
      return;
    }
    treeKey = "";
    await poll();
  }

  function startTicker(): void {
    if (ticker === 0) ticker = window.setInterval(renderWizard, 250);
  }

  function stopTicker(): void {
    if (ticker === 0) return;
    window.clearInterval(ticker);
    ticker = 0;
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
    runsPanel.root,
    progress.root,
    results.root,
    files.root,
  );

  renderRuns();
  renderWizard();
  renderMetrics();
  renderResults();
  renderFiles();
  renderControls();
  void poll();
  window.setInterval(() => void poll(), POLL_MS);

  return Object.assign(root, { commandBar });
}

// ------------------------------------------------------------------------------ helpers --

/** One cell of a `.tiles--compact` strip, unlabelled: this strip renders `step 100/2000`, which
 *  labels itself — the step that printed it already said what it is. */
function tile(value: string): HTMLElement {
  return el("div", { class: "tile" }, el("span", { class: "tile__value", text: value }));
}

/** The status file's stage row for one step, or null when the file has no row for it — which is
 *  what a status written by an older build would look like. */
function rowFor(run: TrainingRun | null, stage: TrainStage): TrainingStageStatus | null {
  return run?.stages.find((row) => row.stage === stage) ?? null;
}

/** A stage name in Chinese, or the name itself when the file says something this build does not
 *  know: an unknown stage is data, not a reason to render nothing. */
function stageLabel(stage: string): string {
  return STAGE_LABEL[stage as TrainStage] ?? stage;
}

function stateLabel(state: string): string {
  return STATE_LABEL[state] ?? state;
}

/** 0.6 is not a threshold the engine has; it is where the reference corpus's own leave-one-out
 *  p10 sat (0.704) minus the room a good run needs, so it separates "worth listening to" from
 *  "look at this before installing it". */
function lowerBoundTone(value: number): Tone {
  return value >= 0.6 ? "ok" : "warn";
}
