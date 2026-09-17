// Deploy: the screen that answers "do I have to download 4.8 GiB again?" with no.
//
// Three pages on one horizontal track, switched with a transform transition:
//
//   准备    - the environment list. What is on this machine, what is missing, and
//             where an existing copy can be pointed at instead of downloaded.
//   需下载   - what an install would fetch, item by item with sizes, and - once a
//             run starts - the seven bootstrap stages and the log. The whole
//             provision flow lives here: nothing about the event stream moved.
//   完成    - the handoff. It exists for the moment the environment passes
//             envComplete(); the page says so and the screen routes itself to
//             状态. There is nothing to press and nothing left to do here.
//
// The stage rows and the log head are built once and mutated in place. Rebuilding
// them per event would move focus off a control the user was about to press, and
// during the models stage there is roughly one event per second for an hour. The
// same rule holds the pager together: switching a stage flips one attribute and
// re-renders nothing, so a detect() answer landing mid-transition cannot flash.
//
// Each stage owns its own actions in the shell's pinned command bar - the bar's
// slot is the shell's, the buttons are the page's. They cannot scroll away
// mid-download, which is the whole reason they live below the scroll region.

import { el, fill, type Child } from "../dom";
import { t } from "../i18n";
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
import { envComplete, inventory, refreshInventory, refreshVoices } from "../state";
import { toast } from "../toast";
import {
  blockedButton,
  button,
  chip,
  navigate,
  note,
  panel,
  openButton,
  pathText,
  type Tone,
} from "../ui";

type StageState = "pending" | "running" | "ok" | "skip" | "fail";

/** The screen's three pages, in walking order. Named apart from the bootstrap
 *  `Stage`s, which belong to the run panel on the second page. */
const STEPS = ["prepare", "download", "done"] as const;
type Step = (typeof STEPS)[number];

/** How long the 完成 page stays readable before the screen hands off to 状态.
 *  Long enough to register "done", short enough that it reads as the same
 *  motion, not as a page the user has to dismiss. */
const HANDOFF_DELAY = 1200;

/** Result-oriented, and free of the engine's vocabulary: "DACVAE" and
 *  "runtime.json" belong in the log, where someone who needs them is already
 *  looking. */
const STAGE_LABEL: Record<Stage, () => string> = {
  preflight: () => t.deploy.stagePreflight,
  engine: () => t.deploy.stageEngine,
  codec: () => t.deploy.stageCodec,
  venv: () => t.deploy.stageVenv,
  models: () => t.deploy.stageModels,
  layout: () => t.deploy.stageLayout,
  smoke: () => t.deploy.stageSmoke,
};

const STATE_LABEL: Record<StageState, () => string> = {
  pending: () => t.deploy.statePending,
  running: () => t.deploy.stateRunning,
  ok: () => t.deploy.stateOk,
  skip: () => t.deploy.stateSkip,
  fail: () => t.deploy.stateFail,
};

/** The screen's own three stages, for the stepper and the pages' labels. */
const STEP_LABEL: Record<Step, () => string> = {
  prepare: () => t.deploy.stepPrepare,
  download: () => t.deploy.stepDownload,
  done: () => t.deploy.stepDone,
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
  let autoScroll = true;
  let finished = false;
  /** True while a manual 重新检测 is in flight. The probe behind detect() costs
   *  about five seconds, and when the answer matches what is already on screen
   *  the re-render is pixel-identical — without a busy state the button reads
   *  as dead. Blocked styling plus the skeleton list say "working" instead. */
  let detecting = false;
  let transient = false;
  let step: Step = "prepare";
  let routeTimer = 0;
  /** Reused steps of the last run, for the final page's meta line. */
  let lastReused = 0;

  // ------------------------------------------------------------- page 1: prepare
  const envPanel = panel({ title: t.deploy.envCard, hint: t.deploy.prepareHint, id: "deploy-env" });
  const envTail = el("span", { class: "panel__tail" });
  envPanel.root.querySelector(".panel__head")?.appendChild(envTail);

  /** One row, one outcome. `right` is a chip when nothing is expected of the user
   *  and a button when something is. Shared by both list pages: the same row
   *  grammar on 准备 and on 需下载 is what makes them read as one conversation. */
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
          name: t.common.clearField(label),
          title: t.common.clear,
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
      [el("p", { class: "inv__miss", text: t.deploy.notInstalled })],
      button({
        label: t.deploy.pickDir,
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
      envTail.textContent = t.deploy.detecting;
      fill(
        envPanel.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3, 4, 5].map(() => el("div", { class: "skeleton" })),
        ),
        el("p", { class: "sr-only", role: "status", text: t.deploy.detectingAria }),
      );
      return;
    }

    const present = inv.models.filter((model) => model.present);
    const reusableGiB = present.reduce((sum, model) => sum + model.gib, 0);
    const short = inv.needs_gib > inv.disk_free_gib;
    const pythonReady = inv.engine_python !== null && inv.python_ok;

    // The tail is what makes scanning the page fast: the one fact worth carrying is
    // that nothing has to be downloaded twice - or that the disk cannot hold it.
    fill(
      envTail,
      reusableGiB > 0 ? chip(t.deploy.reusable(formatGiB(reusableGiB)), "reuse", "recycle") : null,
      short ? chip(t.deploy.diskShort, "fail", "warning-circle") : null,
    );

    fill(
      envPanel.body,
      pickRow(
        "engine_root",
        "folder-open",
        t.deploy.engineSource,
        t.deploy.pickEngineDir,
        inv.engine_root,
        chip(t.deploy.reuse, "reuse", "recycle"),
      ),
      envRow(
        "cpu",
        t.deploy.pythonCuda,
        [
          el("p", {
            class: "inv__text",
            text:
              inv.engine_python === null
                ? inv.cuda === null
                  ? t.deploy.pyLater
                  : `${t.deploy.pyLater} · CUDA ${inv.cuda}`
                : inv.cuda === null
                  ? t.deploy.noCuda
                  : `CUDA ${inv.cuda}`,
          }),
          inv.engine_python === null ? null : el("div", { class: "inv__value" }, pathText(inv.engine_python)),
        ],
        pythonReady
          ? chip(t.deploy.ready, "ok", "check-circle")
          : inv.engine_python === null
            ? chip(t.deploy.notInstalled, "idle", "circle-dashed")
            : chip(t.deploy.rebuild, "warn", "warning"),
      ),
      pickRow(
        "hf_home",
        "database",
        t.deploy.weights(present.length, inv.models.length),
        t.deploy.pickModelsDir,
        inv.hf_cache,
        chip(t.deploy.reuseGiB(formatGiB(reusableGiB)), "reuse", "recycle"),
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
            t.deploy.packs(inv.packs.length),
            [
              el("p", {
                class: "inv__text",
                text: inv.packs.map((pack) => pack.character ?? pack.name).join("、"),
              }),
            ],
            chip(t.deploy.ready, "ok", "check-circle"),
          )
        : pickRow(
            "voice_packs",
            "microphone-stage",
            t.deploy.packsLabel,
            t.deploy.pickPacksDir,
            null,
            chip(t.deploy.ready, "ok", "check-circle"),
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
              el("span", { text: t.deploy.needLine(formatGiB(inv.disk_free_gib)) }),
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

  // ----------------------------------------------------------------- run stages
  const stagesPanel = panel({ title: t.deploy.stepsTitle });
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
          el("h3", { class: "stage__title", text: STAGE_LABEL[stage]() }),
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
      model.state === "pending" ? null : chip(STATE_LABEL[model.state](), STATE_TONE[model.state]),
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
        row.barText.textContent = asBytes ? t.deploy.doneBytes(formatBytes(model.done)) : t.deploy.doneItems(model.done);
      }
    } else {
      row.progressWrap.hidden = true;
    }

    fill(
      row.detail,
      model.remedy === null ? null : note("fail", t.deploy.remedyTitle, el("p", { class: "remedy", text: model.remedy })),
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
            label: t.deploy.retryStep,
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
    label: t.deploy.jumpBottom,
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
    label: t.deploy.expandLog,
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
    if (label !== null) label.textContent = open ? t.deploy.collapseLog : t.deploy.expandLog;
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
    while (logList.childElementCount > LOG_CAP && logList.firstChild !== null) {
      logList.removeChild(logList.firstChild);
    }
    if (autoScroll && !logBody.hidden) logScroll.scrollTop = logScroll.scrollHeight;
  }

  // ------------------------------------------------------------------ page 2: download
  const installPanel = panel({ title: t.deploy.installTitle, hint: t.deploy.installHint });

  function renderInstall(inv: Inventory | null): void {
    if (inv === null) {
      fill(
        installPanel.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3].map(() => el("div", { class: "skeleton" })),
        ),
        el("p", { class: "sr-only", role: "status", text: t.deploy.detectingAria }),
      );
      return;
    }

    const missingModels = inv.models.filter((model) => !model.present);
    const engineMissing = inv.engine_python === null;
    const pythonMissing = engineMissing || !inv.python_ok;

    fill(
      installPanel.body,
      engineMissing
        ? envRow(
            "folder-open",
            t.deploy.engineSource,
            [el("p", { class: "inv__miss", text: t.deploy.willFetchEngine })],
            chip(t.deploy.notInstalled, "idle", "circle-dashed"),
          )
        : null,
      pythonMissing
        ? envRow(
            "cpu",
            t.deploy.pythonCuda,
            [el("p", { class: "inv__miss", text: t.deploy.willBuildPython })],
            engineMissing
              ? chip(t.deploy.notInstalled, "idle", "circle-dashed")
              : chip(t.deploy.rebuild, "warn", "warning"),
          )
        : null,
      missingModels.length > 0
        ? el(
            "ul",
            { class: "models" },
            missingModels.map((model) =>
              el(
                "li",
                { class: "models__item" },
                icon("circle-dashed", "models__icon"),
                el("code", { class: "models__repo", dir: "ltr", text: model.repo }),
                el("span", { class: "models__size", text: formatGiB(model.gib) }),
              ),
            ),
          )
        : null,
      // The one number the list cannot carry per row: what the whole fetch adds up
      // to, against nothing - the free-space question stays on 准备, where the track
      // lives.
      inv.needs_gib > 0
        ? el("p", { class: "disk__line", text: t.deploy.installTotal(formatGiB(inv.needs_gib)) })
        : null,
      engineMissing || pythonMissing || missingModels.length > 0
        ? null
        : note("reuse", t.deploy.nothingToFetch),
    );
  }

  // ------------------------------------------------------------------- done page
  const donePanel = panel({ title: t.deploy.stepDone });

  function renderDone(inv: Inventory | null): void {
    if (inv === null) {
      fill(
        donePanel.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3].map(() => el("div", { class: "skeleton" })),
        ),
      );
      return;
    }
    if (!envComplete(inv)) {
      // Reachable only on foot - the rail retires this screen once the environment
      // completes, and the automatic handoff never fires early. Say what is open.
      fill(donePanel.body, note("warn", t.deploy.stepDoneBlocked));
      return;
    }
    fill(
      donePanel.body,
      el(
        "div",
        { class: "banner" },
        icon("check-circle", "banner__icon"),
        el("p", { class: "banner__text", text: finished ? t.deploy.doneBanner : t.deploy.envReady }),
        el("span", {
          class: "banner__meta",
          text: finished && lastReused > 0 ? t.deploy.reusedCount(lastReused) : "",
        }),
      ),
      // The handoff says itself: without this line, an automatic route reads as the
      // panel acting on its own.
      transient ? null : el("p", { class: "dpager__hint", text: t.deploy.doneHint }),
    );
  }

  // --------------------------------------------------------------- command bar
  const cmdLeft = el("div", { class: "cmdbar__left" });
  const cmdRight = el("div", { class: "cmdbar__right" });
  const commandBar = el("div", { class: "cmdbar" }, cmdLeft, cmdRight);

  function renderControls(): void {
    // Retry buttons grey out while a run is in flight, whichever page is showing.
    for (const stage of STAGES) renderRow(stage);

    if (running) {
      commandBar.hidden = false;
      fill(
        cmdLeft,
        button({
          label: t.deploy.cancel,
          kind: "danger",
          glyph: "x",
          onClick: () => void cancelProvision().catch((err: unknown) => toast(ipcMessage(err), "fail")),
        }),
      );
      // The hint that used to be a paragraph under the stage list. It is one sentence
      // and it is only true while a run is in flight, which is exactly a tooltip.
      fill(cmdRight, blockedButton({ label: t.deploy.runningNow }, t.deploy.backgroundHint));
      return;
    }

    if (step === "done") {
      // The final page has no actions: its only exit is the handoff to 状态, and a
      // button next to that would be a second answer to "what now".
      commandBar.hidden = true;
      fill(cmdLeft, null);
      fill(cmdRight, null);
      return;
    }

    commandBar.hidden = false;
    if (step === "prepare") {
      fill(
        cmdLeft,
        detecting
          ? blockedButton({ label: t.deploy.reDetect, glyph: "arrow-clockwise" }, t.deploy.detecting)
          : button({
              label: t.deploy.reDetect,
              glyph: "arrow-clockwise",
              kind: "quiet",
              onClick: () => void recheck(),
            }),
      );
      fill(cmdRight, null);
      return;
    }

    // The download page. While detect() has not answered, both actions are blocked
    // with the reason; once it has, the primary action is whichever of the two makes
    // sense: install what is missing, or - when nothing is - walk on to 完成.
    const inv = inventory.value;
    fill(
      cmdLeft,
      inv === null
        ? blockedButton({ label: t.deploy.checkOnly, glyph: "check" }, t.deploy.detecting)
        : button({ label: t.deploy.checkOnly, glyph: "check", onClick: () => void run(null, true) }),
    );
    fill(
      cmdRight,
      envComplete(inv)
        ? button({
            label: t.deploy.nextStage,
            kind: "primary",
            glyph: "caret-right",
            onClick: () => setStep("done"),
          })
        : inv === null
          ? blockedButton({ label: t.deploy.installNow, glyph: "download-simple" }, t.deploy.detecting)
          : button({
              label: t.deploy.installNow,
              kind: "primary",
              glyph: "download-simple",
              onClick: () => void run(null, false),
            }),
    );
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
      stagesTail.textContent = t.deploy.stepsTotal(STAGES.length);
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
        ? t.deploy.stepsTotalTime(STAGES.length, formatElapsed(spent))
        : t.deploy.stepIndex(index + 1, STAGES.length, formatElapsed(spent));
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
      fill(summary, note("fail", t.deploy.startFailed, el("p", { text: runError })));
      return;
    }

    const done = STAGES.filter((stage) => stages.get(stage)?.state === "ok").length;
    const reused = STAGES.filter((stage) => stages.get(stage)?.state === "skip").length;
    const failed = STAGES.filter((stage) => stages.get(stage)?.state === "fail");
    lastReused = reused;

    if (failed.length > 0) {
      fill(
        summary,
        note(
          "warn",
          t.deploy.failedCount(failed.length),
          el("p", {
            text: t.deploy.failedBody(failed.map((stage) => STAGE_LABEL[stage]()).join("、")),
          }),
        ),
      );
      return;
    }
    if (done + reused === 0) {
      // Cancelled runs land here with nothing terminal reported. Calling that a
      // finished deployment would claim an install that did not happen.
      fill(summary, note("warn", t.deploy.cancelled));
      return;
    }
    if (checkOnly) {
      fill(summary, note("reuse", t.deploy.checkPassed(done + reused)));
      return;
    }
    fill(
      summary,
      el(
        "div",
        { class: "banner" },
        icon("check-circle", "banner__icon"),
        el("p", { class: "banner__text", text: t.deploy.doneBanner }),
        el("span", { class: "banner__meta", text: reused > 0 ? t.deploy.reusedCount(reused) : "" }),
      ),
    );
  }

  async function run(only: Stage | null, checkOnly: boolean): Promise<void> {
    if (running) return;
    running = true;
    finished = false;
    if (routeTimer !== 0) {
      window.clearTimeout(routeTimer);
      routeTimer = 0;
    }
    fill(summary, null);

    // A -Only run emits events for that stage alone, so every other row must keep
    // whatever it last reported instead of being blanked to 待执行.
    if (only === null) {
      for (const stage of STAGES) stages.set(stage, blankStage());
    } else {
      stages.set(only, blankStage());
    }
    // The run's machinery belongs to the download page; once it has started, the
    // seven rows and the log stay there for the rest of the visit.
    stagesPanel.root.hidden = false;
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
        model.message = t.deploy.interrupted;
        model.endedAt = Date.now();
      }
      const ok = STAGES.every((stage) => {
        const state = stages.get(stage)?.state;
        return state === "ok" || state === "skip";
      });
      // The handoff page is earned only by a real deployment that really finished:
      // after a check-only pass nothing changed, so there is nothing to move on from.
      finished = ok && !checkOnly && only === null;
      renderControls();
      renderStagesTail();
      // The run changed what is on disk; the environment page and the Voices screen
      // must not keep showing the pre-run picture. When the answer completes the
      // environment, the inventory subscriber below walks the screen to 完成.
      void refreshInventory();
      void refreshVoices();
    }
  }

  /** A manual recheck: the button blocks and the environment list shows its
   *  skeletons for the ~5 s the interpreter probe costs, then the answer —
   *  identical or not — replaces the page. */
  async function recheck(): Promise<void> {
    if (detecting) return;
    detecting = true;
    renderControls();
    renderEnv(null);
    try {
      await refreshInventory();
    } finally {
      detecting = false;
      renderControls();
    }
  }

  fill(
    stagesPanel.body,
    summary,
    stageList,
    el("div", { class: "logpane" }, el("div", { class: "logpane__head" }, logToggle, bottomBtn), logBody),
  );
  stagesPanel.root.hidden = true;

  // ----------------------------------------------------------------- the pager
  const pages: Record<Step, HTMLElement> = {
    prepare: el("section", { class: "dpager__page", tabindex: "-1", "aria-label": STEP_LABEL.prepare() }, envPanel.root),
    download: el(
      "section",
      { class: "dpager__page", tabindex: "-1", "aria-label": STEP_LABEL.download() },
      installPanel.root,
      stagesPanel.root,
    ),
    done: el("section", { class: "dpager__page", tabindex: "-1", "aria-label": STEP_LABEL.done() }, donePanel.root),
  };

  const track = el("div", { class: "dpager__track" }, pages.prepare, pages.download, pages.done);
  const viewport = el("div", { class: "dpager", "data-stage": "prepare" }, track);

  /** Only the visible page takes focus and pointers; the off-screen pages must not
   *  be reachable by Tab from behind the clip. */
  function applyInert(): void {
    for (const name of STEPS) {
      if (name === step) pages[name].removeAttribute("inert");
      else pages[name].setAttribute("inert", "");
    }
  }

  /** The viewport pins itself to the active page's height, so the short final page
   *  does not inherit the env list's and the window does not jump when a page's
   *  content changes under a live detect. The ResizeObserver is what keeps this
   *  true while skeletons are replaced by rows. */
  function syncHeight(): void {
    viewport.style.height = `${pages[step].offsetHeight}px`;
  }
  const pageObserver = new ResizeObserver(() => syncHeight());
  for (const page of Object.values(pages)) pageObserver.observe(page);

  const stepButtons = {} as Record<Step, HTMLButtonElement>;
  const stepDots = {} as Record<Step, HTMLElement>;
  const stepList = el(
    "ol",
    { class: "dpager__steps", "aria-label": t.deploy.title },
    STEPS.map((name, index) => {
      const dot = el("span", { class: "dpager__dot" }, String(index + 1));
      const btn = el(
        "button",
        {
          class: "dpager__step",
          type: "button",
          "data-step": name,
          onclick: () => setStep(name),
        },
        dot,
        el("span", { class: "dpager__label", text: STEP_LABEL[name]() }),
      );
      stepButtons[name] = btn;
      stepDots[name] = dot;
      return el("li", {}, btn);
    }),
  );

  function renderSteps(): void {
    const current = STEPS.indexOf(step);
    for (const [index, name] of STEPS.entries()) {
      const btn = stepButtons[name];
      btn.classList.toggle("is-active", index === current);
      btn.classList.toggle("is-passed", index < current);
      if (index === current) btn.setAttribute("aria-current", "step");
      else btn.removeAttribute("aria-current");
      fill(stepDots[name], index < current ? icon("check") : String(index + 1));
    }
  }

  /** The one exit from the 完成 page: after a beat long enough to read the banner,
   *  the screen asks the shell for 状态. Guarded against every way the moment could
   *  have gone stale - the user moving on first, a transient visit, a new run. */
  function beginHandoff(): void {
    if (transient || routeTimer !== 0) return;
    routeTimer = window.setTimeout(() => {
      routeTimer = 0;
      if (step === "done" && !running && !transient && !screen.hidden) navigate("status");
    }, HANDOFF_DELAY);
  }

  function setStep(next: Step): void {
    if (step === next) return;
    step = next;
    viewport.setAttribute("data-stage", next);
    applyInert();
    renderSteps();
    renderControls();
    // The slide is a CSS transition on the track; this function only moves state,
    // so a mid-flight detect() answer cannot flash a half-built page.
    syncHeight();
    // Arriving at the final page with the environment already complete is the one
    // arrival that schedules its own exit - the page carries the "going to 状态"
    // line, so the promise has to be kept.
    if (next === "done" && envComplete(inventory.value)) beginHandoff();
    // Focus follows the page, not the control that caused the switch - the same
    // rule the shell applies when a screen changes.
    pages[next].focus({ preventScroll: true });
  }

  // -------------------------------------------------------------------- shell
  const backSlot = el("span", { class: "screen__back" });
  const title = el("h1", { class: "screen__title", tabindex: "-1", text: t.deploy.title });

  const screen = el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el("div", { class: "screen__titles" }, el("div", { class: "screen__titlerow" }, backSlot, title)),
    ),
    stepList,
    viewport,
  );

  function setTransient(on: boolean): void {
    if (transient === on) return;
    transient = on;
    title.textContent = on ? t.deploy.transientTitle : t.deploy.title;
    fill(
      backSlot,
      on
        ? button({
            glyph: "arrow-left",
            name: t.deploy.backToStatus,
            kind: "quiet",
            small: true,
            onClick: (ev: MouseEvent) => navigate("status", ev),
          })
        : null,
    );
    // Reopening from 状态 is a check, not a resumed deployment: the previous run's
    // banner, its handoff timer and its stage do not belong to this visit.
    if (on) {
      finished = false;
      if (routeTimer !== 0) {
        window.clearTimeout(routeTimer);
        routeTimer = 0;
      }
      fill(summary, null);
      setStep("prepare");
      renderDone(inventory.value);
    }
    renderControls();
  }

  // ------------------------------------------------------------------- wiring
  void onBootstrapEvent(apply);
  inventory.subscribe((inv) => {
    renderEnv(inv);
    renderInstall(inv);
    renderDone(inv);
    renderControls();

    // The environment just completed while this screen sits open - the normal shape
    // of a run's last detect(). Walk to the final page; setStep starts the handoff.
    // Never while a run is in flight (the run summary is still on screen), never in
    // transient mode (the user came here from 状态 on purpose), never twice, and
    // never while the screen is hidden - the shell's own landing() already routed
    // the first detect() answer, and a second router would fight it.
    if (inv === null || !envComplete(inv) || running || transient || screen.hidden || routeTimer !== 0) return;
    setStep("done");
  });
  renderSteps();
  applyInert();
  syncHeight();
  renderControls();
  renderStagesTail();

  return Object.assign(screen, { commandBar, setTransient, isBusy: () => running || finished });
}
