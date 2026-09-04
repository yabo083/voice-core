// Deploy: the screen that answers "do I have to download 4.8 GiB again?" with no.
//
// One card for the environment, one for the seven bootstrap stages, and the primary
// actions in the shell's command bar so they cannot scroll away mid-download.
//
// The environment card is a single list rather than the old "what we found" plus
// "point at what you already have" pair. Those two panels described one thing - the
// state of each dependency - and splitting them forced a paragraph of prose to
// explain how they related. Merged, every row carries its own outcome on the right:
// a chip when the runtime handles it, a button when the user has to. Scanning the
// right edge is the whole instruction set.
//
// The stage list is a spotlight: pending rows are one muted line, the running row is
// the only one with a message and a bar, finished rows collapse to a chip. Seven rows
// each showing a description, a state, a bar and a re-run button is a wall, and a
// wall is where a failure hides.
//
// The stage rows and the log head are built once and mutated in place. Rebuilding
// them per event would move focus off a control the user was about to press, and
// during the models stage there is roughly one event per second for an hour.

import { el, fill, type Child } from "../dom";
import { formatBytes, formatElapsed, formatGiB, formatPercent } from "../format";
import { icon, type IconName } from "../icons";
import {
  STAGES,
  cancelProvision,
  ipcMessage,
  onBootstrapEvent,
  pickFolder,
  provision,
  type BootstrapEvent,
  type Inventory,
  type Stage,
} from "../ipc";
import { inventory, refreshInventory, refreshVoices } from "../state";
import { toast } from "../toast";
import {
  blockedButton,
  button,
  chip,
  expander,
  navigate,
  note,
  panel,
  openButton,
  pathText,
  type Tone,
} from "../ui";

type StageState = "pending" | "running" | "ok" | "skip" | "fail";

/** Result-oriented, and free of the engine's vocabulary: "DACVAE" and
 *  "runtime.json" belong in the log, where someone who needs them is already
 *  looking. */
const STAGE_LABEL: Record<Stage, string> = {
  preflight: "环境检查",
  engine: "引擎源码",
  codec: "音频编解码器",
  venv: "Python 环境",
  models: "模型权重",
  layout: "写入配置",
  smoke: "试跑一句",
};

const STATE_LABEL: Record<StageState, string> = {
  pending: "待执行",
  running: "进行中",
  ok: "完成",
  skip: "已复用",
  fail: "失败",
};

const STATE_TONE: Record<StageState, Tone> = {
  pending: "idle",
  running: "run",
  ok: "ok",
  skip: "reuse",
  fail: "fail",
};

const STATE_GLYPH: Record<StageState, IconName> = {
  pending: "circle-dashed",
  running: "spinner-gap",
  ok: "check-circle",
  skip: "recycle",
  fail: "warning-circle",
};

/** Beyond this the log pane is scrollback nobody reads and DOM the window pays for
 *  on every layout. The full transcript stays in data/logs either way. */
const LOG_CAP = 2000;

interface StageModel {
  state: StageState;
  message: string;
  remedy: string | null;
  done: number | null;
  total: number | null;
  startedAt: number | null;
  endedAt: number | null;
  /** `log` lines that carried a remedy: preflight reports six checks that way, and
   *  a failing check inside a stage that still ends `ok` must stay visible. */
  notes: { message: string; remedy: string }[];
}

interface StageRow {
  root: HTMLElement;
  glyph: HTMLElement;
  chipSlot: HTMLElement;
  message: HTMLElement;
  elapsed: HTMLElement;
  progressWrap: HTMLElement;
  bar: HTMLProgressElement;
  barText: HTMLElement;
  detail: HTMLElement;
  retry: HTMLElement;
}

function blankStage(): StageModel {
  return {
    state: "pending",
    message: "",
    remedy: null,
    done: null,
    total: null,
    startedAt: null,
    endedAt: null,
    notes: [],
  };
}

export interface DeployScreen extends HTMLElement {
  /** Lives in the shell, below the scroll region. */
  commandBar: HTMLElement;
  /** Reopened from 状态 after provisioning: gains a back arrow, and is called what
   *  it is at that point - an environment check, not a deployment. */
  setTransient(on: boolean): void;
  /** A run is in flight or has just finished. The shell asks before re-routing away
   *  from this screen: a late `detect()` answer must not yank a user off a live
   *  provision or off the summary it just produced. */
  isBusy(): boolean;
}

export function createDeployScreen(): DeployScreen {
  const stages = new Map<Stage, StageModel>(STAGES.map((stage) => [stage, blankStage()]));
  const rows = new Map<Stage, StageRow>();
  const chosen: {
    engine_root: string | null;
    hf_home: string | null;
    voice_packs: string | null;
  } = { engine_root: null, hf_home: null, voice_packs: null };

  let running = false;
  let ticker = 0;
  let logCount = 0;
  let autoScroll = true;
  let finished = false;
  let transient = false;

  // ------------------------------------------------------------- environment
  const env = expander({ title: "环境与依赖", id: "deploy-env", open: true });

  /** One row, one outcome. `right` is a chip when nothing is expected of the user
   *  and a button when something is. */
  function envRow(glyph: IconName, label: string, body: Child[], right: Child): HTMLElement {
    return el(
      "div",
      { class: "inv" },
      icon(glyph, "inv__icon"),
      el("div", { class: "inv__main" }, el("p", { class: "inv__label", text: label }), body),
      right,
    );
  }

  /** A dependency the user may point at instead of letting bootstrap fetch it.
   *  Once chosen, the path replaces the button and can be cleared. */
  function pickRow(
    key: "engine_root" | "hf_home" | "voice_packs",
    glyph: IconName,
    label: string,
    dialogTitle: string,
    found: string | null,
    foundChip: HTMLElement,
  ): HTMLElement {
    if (found !== null && found !== "") {
      return envRow(glyph, label, [el("div", { class: "inv__value" }, pathText(found), openButton(found))], foundChip);
    }
    const picked = chosen[key];
    if (picked !== null) {
      return envRow(
        glyph,
        label,
        [el("div", { class: "inv__value" }, pathText(picked, 64))],
        button({
          glyph: "x",
          name: `清除${label}`,
          title: "清除",
          small: true,
          kind: "quiet",
          onClick: () => {
            chosen[key] = null;
            renderEnv(inventory.value);
          },
        }),
      );
    }
    return envRow(
      glyph,
      label,
      [el("p", { class: "inv__miss", text: "未安装" })],
      button({
        label: "选择目录…",
        small: true,
        onClick: () => {
          void pickFolder(dialogTitle)
            .then((path) => {
              if (path === null) return;
              chosen[key] = path;
              renderEnv(inventory.value);
            })
            .catch((err: unknown) => toast(ipcMessage(err), "fail"));
        },
      }),
    );
  }

  function renderEnv(inv: Inventory | null): void {
    if (inv === null) {
      env.tail.textContent = "正在检测…";
      fill(
        env.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3, 4, 5].map(() => el("div", { class: "skeleton" })),
        ),
        el("p", { class: "sr-only", role: "status", text: "正在检测本机环境" }),
      );
      return;
    }

    const present = inv.models.filter((model) => model.present);
    const reusableGiB = present.reduce((sum, model) => sum + model.gib, 0);
    const short = inv.needs_gib > inv.disk_free_gib;
    const pythonReady = inv.engine_python !== null && inv.python_ok;

    // The tail is what makes collapsing this card safe: the one sentence worth
    // keeping is that nothing has to be downloaded twice.
    fill(
      env.tail,
      reusableGiB > 0 ? chip(`${formatGiB(reusableGiB)} 可复用`, "reuse", "recycle") : null,
      short ? chip("空间不足", "fail", "warning-circle") : null,
    );

    fill(
      env.body,
      pickRow(
        "engine_root",
        "folder-open",
        "引擎源码",
        "选择现成的引擎目录",
        inv.engine_root,
        chip("复用", "reuse", "recycle"),
      ),
      envRow(
        "cpu",
        "Python 与 CUDA",
        [
          el("p", {
            class: "inv__text",
            text:
              inv.engine_python === null
                ? inv.cuda === null
                  ? "将由「Python 环境」这一步创建"
                  : `将由「Python 环境」这一步创建 · CUDA ${inv.cuda}`
                : inv.cuda === null
                  ? "未检测到 CUDA，合成会非常慢"
                  : `CUDA ${inv.cuda}`,
          }),
          inv.engine_python === null ? null : el("div", { class: "inv__value" }, pathText(inv.engine_python)),
        ],
        pythonReady
          ? chip("就绪", "ok", "check-circle")
          : inv.engine_python === null
            ? chip("待安装", "idle", "circle-dashed")
            : chip("需重建", "warn", "warning"),
      ),
      pickRow(
        "hf_home",
        "database",
        `模型权重 ${present.length} / ${inv.models.length}`,
        "选择现成的模型缓存目录",
        inv.hf_cache,
        chip(`${formatGiB(reusableGiB)} 复用`, "reuse", "recycle"),
      ),
      inv.models.length === 0
        ? null
        : el(
            "ul",
            { class: "models" },
            inv.models.map((model) =>
              el(
                "li",
                { class: `models__item${model.present ? " is-present" : ""}` },
                icon(model.present ? "check-circle" : "circle-dashed", "models__icon"),
                el("code", { class: "models__repo", dir: "ltr", text: model.repo }),
                el("span", { class: "models__size", text: formatGiB(model.gib) }),
              ),
            ),
          ),
      inv.packs.length > 0
        ? envRow(
            "microphone-stage",
            `音色包 ${inv.packs.length}`,
            [
              el("p", {
                class: "inv__text",
                text: inv.packs.map((pack) => pack.character ?? pack.name).join("、"),
              }),
            ],
            chip("就绪", "ok", "check-circle"),
          )
        : pickRow(
            "voice_packs",
            "microphone-stage",
            "音色包",
            "选择现成的音色包目录",
            null,
            chip("就绪", "ok", "check-circle"),
          ),
      // A bar, not a sentence. The track is the free space on the drive and the fill
      // is what this deployment wants from it; detect() reports free and needed, not
      // capacity, so drawing a used/total split would mean inventing the total. Below
      // 1.5% the fill is a fixed marker instead of a proportion, because 0.5 GiB of
      // 382 GiB has no drawable width and a hairline pinned at zero reads as broken.
      //
      // Nothing left to fetch means the question "does it fit" does not exist, so the
      // row goes away rather than reporting 0.00 GiB against a full-size bar.
      inv.needs_gib <= 0
        ? null
        : el(
            "div",
            { class: "disk" },
            el(
              "p",
              { class: "disk__line" },
              el("strong", { text: formatGiB(inv.needs_gib) }),
              el("span", { text: `本次需要 · 可用 ${formatGiB(inv.disk_free_gib)}` }),
            ),
            el(
              "div",
              { class: `track${short ? " track--short" : ""}` },
              el("i", {
                class: "track__need",
                style:
                  inv.disk_free_gib <= 0 || short
                    ? "width:100%"
                    : `width:${Math.max(1.5, (inv.needs_gib / inv.disk_free_gib) * 100).toFixed(1)}%`,
              }),
            ),
          ),
    );
  }

  // ------------------------------------------------------------------- stages
  const stagesPanel = panel({ title: "部署步骤" });
  const stagesTail = el("span", { class: "panel__tail" });
  stagesPanel.root.querySelector(".panel__head")?.appendChild(stagesTail);

  const summary = el("div", { class: "runsummary" });
  const stageList = el("ol", { class: "stages" });

  function buildRow(stage: Stage): StageRow {
    const glyph = el("span", { class: "stage__glyph" }, icon(STATE_GLYPH.pending));
    const chipSlot = el("span", { class: "stage__chip" });
    const message = el("p", { class: "stage__message", hidden: true });
    const elapsed = el("span", { class: "stage__elapsed" });
    const bar = el("progress", { class: "bar", max: "1" });
    const barText = el("span", { class: "stage__bartext" });
    const progressWrap = el("div", { class: "stage__progress", hidden: true }, bar, barText);
    const detail = el("div", { class: "stage__detail" });
    const retry = el("span", { class: "stage__retry" });

    const root = el(
      "li",
      { class: "stage is-pending", "data-stage": stage },
      glyph,
      el(
        "div",
        { class: "stage__main" },
        el(
          "div",
          { class: "stage__head" },
          el("h3", { class: "stage__title", text: STAGE_LABEL[stage] }),
          elapsed,
          chipSlot,
          retry,
        ),
        message,
        progressWrap,
        detail,
      ),
    );

    return { root, glyph, chipSlot, message, elapsed, progressWrap, bar, barText, detail, retry };
  }

  for (const stage of STAGES) {
    const row = buildRow(stage);
    rows.set(stage, row);
    stageList.appendChild(row.root);
  }

  function renderRow(stage: Stage): void {
    const model = stages.get(stage);
    const row = rows.get(stage);
    if (model === undefined || row === undefined) return;

    row.root.className = `stage is-${model.state}`;
    fill(row.glyph, icon(STATE_GLYPH[model.state], model.state === "running" ? "spin" : undefined));

    // A pending row is a title and nothing else. Chips, timers and descriptions on a
    // step that has not started are seven copies of the same non-information.
    fill(
      row.chipSlot,
      model.state === "pending" ? null : chip(STATE_LABEL[model.state], STATE_TONE[model.state]),
    );
    const showMessage = (model.state === "running" || model.state === "fail") && model.message !== "";
    row.message.hidden = !showMessage;
    row.message.textContent = showMessage ? model.message : "";

    const elapsedMs = model.startedAt === null ? 0 : (model.endedAt ?? Date.now()) - model.startedAt;
    // Reused steps report "复用", not "0 秒": the number would invite the reader to
    // wonder what went wrong.
    row.elapsed.textContent =
      model.startedAt === null || model.state === "pending" || model.state === "skip"
        ? ""
        : formatElapsed(elapsedMs);

    // Bytes in the models stage, item counts everywhere else: the unit comes from
    // the stage, not from the magnitude of the number.
    const asBytes = stage === "models";
    if (model.state === "running" && model.done !== null) {
      row.progressWrap.hidden = false;
      if (model.total !== null && model.total > 0) {
        row.bar.max = model.total;
        row.bar.value = model.done;
        row.barText.textContent = asBytes
          ? `${formatBytes(model.done)} / ${formatBytes(model.total)} · ${formatPercent(model.done, model.total)}`
          : `${model.done} / ${model.total} · ${formatPercent(model.done, model.total)}`;
      } else {
        // No total yet: an indeterminate bar is honest, a bar pinned at 0 is not.
        row.bar.removeAttribute("value");
        row.barText.textContent = asBytes ? `已完成 ${formatBytes(model.done)}` : `已完成 ${model.done} 项`;
      }
    } else {
      row.progressWrap.hidden = true;
    }

    fill(
      row.detail,
      model.remedy === null ? null : note("fail", "怎么解决", el("p", { class: "remedy", text: model.remedy })),
      model.notes.length === 0
        ? null
        : el(
            "ul",
            { class: "stage__notes" },
            model.notes.map((entry) =>
              el(
                "li",
                { class: "stage__note" },
                icon("warning", "stage__noteicon"),
                el("div", {}, el("p", { text: entry.message }), el("p", { class: "remedy", text: entry.remedy })),
              ),
            ),
          ),
    );

    // Only a failed step offers a re-run. A quiet re-run button on all seven rows is
    // seven affordances for something nobody does, sitting next to the one that matters.
    fill(
      row.retry,
      model.state === "fail"
        ? button({
            label: "重试这一步",
            glyph: "arrow-clockwise",
            small: true,
            disabled: running,
            onClick: () => void run(stage, false),
          })
        : null,
    );
  }

  // ------------------------------------------------------------------ log pane
  const logList = el("ol", { class: "log" });
  const logScroll = el(
    "div",
    {
      class: "logscroll",
      role: "log",
      // Deliberately not a live region: during the models stage this scrolls faster
      // than speech, and announcing it would bury the stage transitions that matter.
      "aria-live": "off",
      tabindex: "0",
      onscroll: () => {
        const atBottom = logScroll.scrollHeight - logScroll.clientHeight - logScroll.scrollTop < 24;
        if (atBottom === autoScroll) return;
        autoScroll = atBottom;
        renderScrollState();
      },
    },
    logList,
  );

  const logBody = el("div", { class: "logpane__body", id: "deploy-log-body", hidden: true }, logScroll);
  const bottomBtn = button({
    label: "回到底部",
    glyph: "arrow-down",
    small: true,
    kind: "quiet",
    disabled: true,
    onClick: () => {
      autoScroll = true;
      logScroll.scrollTop = logScroll.scrollHeight;
      renderScrollState();
    },
  });
  const logToggle = button({
    label: "显示详细日志",
    // One caret, rotated by CSS on aria-expanded: swapping the glyph would rebuild
    // the button and drop focus while a user is toggling it.
    glyph: "caret-right",
    kind: "quiet",
    small: true,
    expanded: false,
    controls: "deploy-log-body",
    onClick: () => {
      const open = Boolean(logBody.hidden);
      setLogOpen(open);
      if (open && autoScroll) logScroll.scrollTop = logScroll.scrollHeight;
    },
  });

  function setLogOpen(open: boolean): void {
    logBody.hidden = !open;
    logToggle.setAttribute("aria-expanded", String(open));
    const label = logToggle.querySelector("span");
    if (label !== null) label.textContent = open ? "隐藏详细日志" : "显示详细日志";
    // A scroll control belongs to a visible pane; kept on screen while collapsed it
    // is a dead control next to a closed drawer.
    bottomBtn.hidden = !open;
  }
  setLogOpen(false);

  function renderScrollState(): void {
    bottomBtn.disabled = autoScroll;
  }
  renderScrollState();

  function appendLog(event: BootstrapEvent): void {
    logList.appendChild(
      el(
        "li",
        { class: `logline logline--${event.event}` },
        el("code", { class: "logline__stage", dir: "ltr", text: event.stage }),
        el("span", { class: "logline__text", text: event.message }),
      ),
    );
    logCount += 1;
    while (logList.childElementCount > LOG_CAP && logList.firstChild !== null) {
      logList.removeChild(logList.firstChild);
    }
    if (autoScroll && !logBody.hidden) logScroll.scrollTop = logScroll.scrollHeight;
  }

  // --------------------------------------------------------------- command bar
  const cmdLeft = el("div", { class: "cmdbar__left" });
  const cmdRight = el("div", { class: "cmdbar__right" });
  const commandBar = el("div", { class: "cmdbar" }, cmdLeft, cmdRight);

  function renderControls(): void {
    if (running) {
      fill(
        cmdLeft,
        button({
          label: "取消",
          kind: "danger",
          glyph: "x",
          onClick: () => void cancelProvision().catch((err: unknown) => toast(ipcMessage(err), "fail")),
        }),
      );
      // The hint that used to be a paragraph under the stage list. It is one sentence
      // and it is only true while a run is in flight, which is exactly a tooltip.
      fill(cmdRight, blockedButton({ label: "进行中…" }, "关掉窗口不会中断部署"));
    } else if (finished) {
      fill(cmdLeft, null);
      fill(
        cmdRight,
        button({
          label: "完成",
          kind: "primary",
          glyph: "check",
          onClick: (ev: MouseEvent) => navigate("status", ev),
        }),
      );
    } else {
      fill(
        cmdLeft,
        button({ label: "仅检查", glyph: "check", onClick: () => void run(null, true) }),
        button({
          label: "重新检测",
          glyph: "arrow-clockwise",
          kind: "quiet",
          onClick: () => void refreshInventory(),
        }),
      );
      fill(
        cmdRight,
        button({
          label: transient ? "重新部署" : "开始部署",
          kind: "primary",
          glyph: "download-simple",
          onClick: () => void run(null, false),
        }),
      );
    }
    for (const stage of STAGES) renderRow(stage);
  }

  function tick(): void {
    for (const stage of STAGES) {
      const model = stages.get(stage);
      if (model === undefined || model.state !== "running" || model.startedAt === null) continue;
      const row = rows.get(stage);
      if (row !== undefined) row.elapsed.textContent = formatElapsed(Date.now() - model.startedAt);
    }
    renderStagesTail();
  }

  function renderStagesTail(): void {
    if (!running && !finished) {
      stagesTail.textContent = `${STAGES.length} 步`;
      return;
    }
    const index = STAGES.findIndex((stage) => stages.get(stage)?.state === "running");
    const started = STAGES.filter((stage) => stages.get(stage)?.startedAt !== null);
    const spent = started.reduce((sum, stage) => {
      const model = stages.get(stage);
      if (model?.startedAt == null) return sum;
      return sum + ((model.endedAt ?? Date.now()) - model.startedAt);
    }, 0);
    stagesTail.textContent =
      index === -1
        ? `${STAGES.length} 步 · ${formatElapsed(spent)}`
        : `第 ${index + 1} 步 / ${STAGES.length} · 已用 ${formatElapsed(spent)}`;
  }

  function apply(event: BootstrapEvent): void {
    const model = stages.get(event.stage);
    if (model === undefined) return;

    if (event.event === "progress") {
      model.done = event.done;
      model.total = event.total;
      if (event.message !== "") model.message = event.message;
      renderRow(event.stage);
      return;
    }

    appendLog(event);

    if (event.event === "start") {
      model.state = "running";
      model.message = event.message;
      model.remedy = null;
      model.notes = [];
      model.done = null;
      model.total = null;
      model.startedAt = event.ts;
      model.endedAt = null;
    } else if (event.event === "log") {
      if (event.message !== "") model.message = event.message;
      if (event.remedy !== null) model.notes.push({ message: event.message, remedy: event.remedy });
    } else {
      model.state = event.event;
      model.message = event.message;
      model.remedy = event.remedy;
      model.endedAt = event.ts;
      if (model.startedAt === null) model.startedAt = event.ts;
    }
    renderRow(event.stage);
    renderStagesTail();
  }

  function renderSummary(runError: string | null, checkOnly: boolean): void {
    if (runError !== null) {
      fill(summary, note("fail", "这次运行没能开始", el("p", { text: runError })));
      return;
    }

    const done = STAGES.filter((stage) => stages.get(stage)?.state === "ok").length;
    const reused = STAGES.filter((stage) => stages.get(stage)?.state === "skip").length;
    const failed = STAGES.filter((stage) => stages.get(stage)?.state === "fail");

    if (failed.length > 0) {
      fill(
        summary,
        note(
          "warn",
          `${failed.length} 个步骤失败`,
          el("p", {
            text: `${failed.map((stage) => STAGE_LABEL[stage]).join("、")}。修好后点那一步的「重试这一步」，不必从头再来。`,
          }),
        ),
      );
      return;
    }
    if (done + reused === 0) {
      // Cancelled runs land here with nothing terminal reported. Calling that a
      // finished deployment would claim an install that did not happen.
      fill(summary, note("warn", "已取消，环境没有变更"));
      return;
    }
    if (checkOnly) {
      fill(summary, note("reuse", `检查通过：${done + reused} 项就绪`));
      return;
    }
    fill(
      summary,
      el(
        "div",
        { class: "banner" },
        icon("check-circle", "banner__icon"),
        el("p", { class: "banner__text", text: "部署完成，引擎可以合成了" }),
        el("span", { class: "banner__meta", text: reused > 0 ? `${reused} 步复用` : "" }),
      ),
    );
  }

  async function run(only: Stage | null, checkOnly: boolean): Promise<void> {
    if (running) return;
    running = true;
    finished = false;
    fill(summary, null);

    // A -Only run emits events for that stage alone, so every other row must keep
    // whatever it last reported instead of being blanked to 待执行.
    if (only === null) {
      for (const stage of STAGES) stages.set(stage, blankStage());
    } else {
      stages.set(only, blankStage());
    }
    // The checklist did its job; the spotlight belongs on the steps now.
    if (only === null && !checkOnly) env.setOpen(false);
    renderControls();
    renderStagesTail();
    if (ticker === 0) ticker = window.setInterval(tick, 250);

    try {
      await provision({
        engine_root: chosen.engine_root,
        hf_home: chosen.hf_home,
        voice_packs: chosen.voice_packs,
        only,
        check_only: checkOnly,
      });
      renderSummary(null, checkOnly);
    } catch (err: unknown) {
      const message = ipcMessage(err);
      renderSummary(message, checkOnly);
      toast(message, "fail");
    } finally {
      running = false;
      if (ticker !== 0) {
        window.clearInterval(ticker);
        ticker = 0;
      }
      // The process is gone, so a stage still marked running was cancelled or died
      // without a terminal event. Leaving it spinning forever would be a lie.
      for (const stage of STAGES) {
        const model = stages.get(stage);
        if (model?.state !== "running") continue;
        model.state = "pending";
        model.message = "已中断";
        model.endedAt = Date.now();
      }
      const ok = STAGES.every((stage) => {
        const state = stages.get(stage)?.state;
        return state === "ok" || state === "skip";
      });
      // "完成" is offered only for a real deployment that really finished: after a
      // check-only pass nothing changed, so there is nothing to move on from.
      finished = ok && !checkOnly && only === null;
      renderControls();
      renderStagesTail();
      // The run changed what is on disk; the environment card above and the Voices
      // screen must not keep showing the pre-run picture.
      void refreshInventory();
      void refreshVoices();
    }
  }

  fill(
    stagesPanel.body,
    summary,
    stageList,
    el("div", { class: "logpane" }, el("div", { class: "logpane__head" }, logToggle, bottomBtn), logBody),
  );

  const backSlot = el("span", { class: "screen__back" });
  const title = el("h1", { class: "screen__title", tabindex: "-1", text: "部署" });

  void onBootstrapEvent(apply);
  inventory.subscribe(renderEnv);
  renderControls();
  renderStagesTail();

  const root = el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el("div", { class: "screen__titles" }, el("div", { class: "screen__titlerow" }, backSlot, title)),
    ),
    env.root,
    stagesPanel.root,
  );

  function setTransient(on: boolean): void {
    if (transient === on) return;
    transient = on;
    title.textContent = on ? "环境检查" : "部署";
    fill(
      backSlot,
      on
        ? button({
            glyph: "arrow-left",
            name: "返回状态",
            kind: "quiet",
            small: true,
            onClick: (ev: MouseEvent) => navigate("status", ev),
          })
        : null,
    );
    // Reopening from 状态 is a check, not a resumed deployment: the previous run's
    // banner and its "完成" button do not belong to this visit.
    if (on) {
      finished = false;
      fill(summary, null);
      env.setOpen(true);
    }
    renderControls();
  }

  return Object.assign(root, { commandBar, setTransient, isBusy: () => running || finished });
}
