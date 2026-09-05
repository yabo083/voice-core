// Status: is it up, what is it holding, and where does this install live.
//
// The runtime being down is the normal state on first run, so `reachable: false`
// renders as a calm fact with a Start button in the command bar. Two different
// sources are deliberately shown side by side: `stack://state` is what this app
// knows about the processes it spawned, and /api/status is the runtime's own report
// about itself. When they disagree, the runtime has just started or just died, and
// hiding that would make a 5 s window look like a bug.
//
// This screen is also where the deployment lives after it has been done once: the
// 环境 card carries the four dependencies as chips and a 检查环境 button, which is the
// only door back to the deploy page once its rail item retires.
//
// There is deliberately no API card here, and no paste-ready speak call: the endpoint, the
// token and the call itself were a reference manual rendered into a window. An agent reads
// skills\voice-core-tts\SKILL.md, or skills\voice-core-voice-training\SKILL.md when the job is
// making a new voice, and both ship beside the binary; an integrator reads docs\api.md, which
// ships too. So the 使用说明 card carries only the two sentences that hand an agent one of those
// files, and the 环境 card carries the pair of directories, because opening a folder is not
// something either document can do.

import { el, fill } from "../dom";
import { dirName, formatBytes, formatDuration, formatPercent } from "../format";
import { icon, type IconName } from "../icons";
import { ipcMessage, startStack, stopStack, type Inventory, type StackState } from "../ipc";
import { inventory, refreshStatus, stack, status, tick, usage } from "../state";
import { toast } from "../toast";
import {
  button,
  chip,
  copyRow,
  emptyState,
  navigate,
  note,
  panel,
  openButton,
  pathText,
  type Tone,
} from "../ui";

/** A directory a human opens by hand: the label, the path itself, and the button that
 *  reveals it in Explorer. */
function pathRow(label: string, path: string): HTMLElement {
  return el(
    "div",
    { class: "wiring__path" },
    el("span", { text: label }),
    pathText(path, 60),
    openButton(path),
  );
}

function tile(label: string, value: string, sub?: string, tone: Tone = "idle"): HTMLElement {
  return el(
    "div",
    { class: `tile tile--${tone}` },
    el("p", { class: "tile__label", text: label }),
    el("p", { class: "tile__value", text: value }),
    sub === undefined ? null : el("p", { class: "tile__sub", text: sub }),
  );
}

export interface StatusScreen extends HTMLElement {
  commandBar: HTMLElement;
}

export function createStatusScreen(): StatusScreen {
  let busy = false;

  const service = panel({ title: "服务" });
  const metrics = panel({ title: "运行指标" });
  const env = panel({
    title: "环境",
    actions: [
      button({
        label: "检查环境",
        kind: "quiet",
        glyph: "arrow-clockwise",
        onClick: (ev: MouseEvent) => navigate("deploy", ev),
      }),
    ],
  });

  // Two sentences and nothing else. The wall of prose this replaced was a document rendered
  // into a window; a skill file is the document, and it already ships.
  const guide = panel({ title: "使用说明", hint: "把其中一句粘贴给 agent，它自己去读技能文件。" });

  const cmdLeft = el("div", { class: "cmdbar__left" });
  const cmdRight = el("div", { class: "cmdbar__right" });
  const commandBar = el("div", { class: "cmdbar" }, cmdLeft, cmdRight);

  function provisioned(): boolean {
    const inv = inventory.value;
    return inv !== null && inv.engine_python !== null && inv.python_ok;
  }

  async function toggleStack(start: boolean): Promise<void> {
    if (busy) return;
    busy = true;
    renderControls();
    try {
      await (start ? startStack() : stopStack());
      await refreshStatus();
      // The runtime binds its port about a second after spawn. Waiting for the next
      // 5 s poll to prove it would make a working button look dead.
      window.setTimeout(() => void refreshStatus(), 1200);
    } catch (err: unknown) {
      toast(`${start ? "启动" : "停止"}服务失败：${ipcMessage(err)}`, "fail");
    } finally {
      busy = false;
      renderControls();
    }
  }

  /** The command bar carries the one thing this screen does to the machine. It sits in
   *  the primary slot on the right, where a wizard's forward action lives.
   *
   *  There is no "speak one line" button. It existed to prove the chain worked, and the
   *  chain is proven by the subtitle window appearing when anything calls the API - a
   *  button that duplicates what the CLI does, in a window that is not the product's
   *  interface for speaking, is a second way to do one thing. */
  function renderControls(): void {
    const up = stack.value.runtime;
    fill(cmdLeft, null);
    fill(
      cmdRight,
      !provisioned()
        ? null
        : up
          ? button({
              label: "停止服务",
              kind: "danger",
              glyph: "stop",
              disabled: busy,
              onClick: () => void toggleStack(false),
            })
          : button({
              label: "启动服务",
              kind: "primary",
              glyph: "play",
              disabled: busy,
              onClick: () => void toggleStack(true),
            }),
    );
  }

  /** A model is loaded, not "running", so the two words are per row rather than one
   *  vocabulary stretched over three different things. */
  function procRow(
    label: string,
    up: boolean,
    glyph: IconName,
    words: [string, string] = ["运行中", "已停止"],
  ): HTMLElement {
    return el(
      "div",
      { class: "procs__item" },
      icon(glyph, "procs__icon"),
      el("span", { class: "procs__label", text: label }),
      chip(up ? words[0] : words[1], up ? "ok" : "idle", up ? "check-circle" : "circle-dashed"),
    );
  }

  function renderService(): void {
    const current = status.value;
    const processes = stack.value;
    const body = current.body;

    fill(
      service.body,
      el(
        "div",
        { class: "procs" },
        procRow("运行时服务", processes.runtime, "pulse"),
        procRow("字幕窗口", processes.presenter, "microphone-stage"),
        procRow("音色模型", processes.model_loaded, "waveform", ["已加载", "未加载"]),
      ),
      current.reachable && body !== null
        ? el("p", {
            class: "panel__meta",
            text: `${body.name} ${body.runtimeVersion} · API v${body.apiVersion} · 运行时间 ${formatDuration(body.uptimeMs)}`,
          })
        : null,
      // Only a real error is worth a block: "not listening" is already said by the
      // three rows above and by the rail.
      !current.reachable && current.error !== null && processes.runtime
        ? note("fail", "运行时无响应", el("p", { class: "note__detail", text: current.error }))
        : null,
      body !== null && body.worker.missing.length > 0
        ? note(
            "warn",
            "存在缺失依赖或资源",
            el(
              "ul",
              { class: "missing" },
              body.worker.missing.map((path) => el("li", {}, pathText(path, 70))),
            ),
          )
        : null,
    );
  }

  /** Every figure here is the runtime's own, redrawn as each poll lands. Nothing is
   *  extrapolated between polls: a clock this window computed and a clock the runtime
   *  measured disagree by a few milliseconds, and the disagreement shows up as a number
   *  that counts backwards. */
  function renderMetrics(): void {
    const body = status.value.body;
    const use = usage.value;
    const running = stack.value.runtime;
    // This window's own working set is not part of the answer: the question a user has
    // is what the voice stack costs, and a panel measuring itself is noise.
    const mem =
      use === null || !running ? null : use.rssRuntimeMib + use.rssEngineMib + use.rssPresenterMib;

    if (body === null) {
      fill(
        metrics.body,
        el(
          "div",
          { class: "tiles" },
          tile("运行时服务", "已停止"),
          tile("语音引擎", "未启动"),
          tile("显存占用", "空闲"),
          tile("内存占用", "-"),
          tile("音色包", "-"),
        ),
      );
      return;
    }

    const worker = body.worker;
    const engineText = !worker.running
      ? "未启动"
      : worker.modelLoaded
        ? "已加载"
        : worker.ready
          ? "运行中（模型未加载）"
          : "启动中";
    const engineTone: Tone = !worker.running ? "idle" : worker.modelLoaded ? "ok" : "warn";

    // The card's own usage is the number that is always available. Per-process VRAM is
    // a driver privilege: a GeForce in WDDM mode refuses to break it down, so the panel
    // headlines what it can measure and only claims the engine's share when the driver
    // actually reports one.
    const card =
      use === null || use.gpuUsedMib === null || use.gpuTotalMib === null
        ? null
        : `${(use.gpuUsedMib / 1024).toFixed(1)} / ${(use.gpuTotalMib / 1024).toFixed(0)} GiB`;
    const vram =
      use !== null && use.engineGpuMib !== null
        ? `${(use.engineGpuMib / 1024).toFixed(2)} GiB`
        : (card ?? (worker.modelLoaded ? "模型驻留中" : worker.running ? "已释放" : "空闲"));
    const vramSub =
      use !== null && use.engineGpuMib !== null
        ? (card === null ? undefined : `显卡总计 ${card}`)
        : card === null
          ? undefined
          : worker.modelLoaded
            ? "显卡总占用（显卡驱动未提供进程粒度指标）"
            : "显卡总占用";

    fill(
      metrics.body,
      el(
        "div",
        { class: "tiles" },
        tile(
          "运行时服务",
          `运行时间 ${formatDuration(body.uptimeMs)}`,
          `${body.name} ${body.runtimeVersion}`,
          "ok",
        ),
        tile(
          "语音引擎",
          engineText,
          worker.running ? `运行时间 ${formatDuration(worker.uptimeMs)}` : undefined,
          engineTone,
        ),
        tile("显存占用", vram, vramSub, worker.modelLoaded ? "warn" : "idle"),
        tile(
          "内存占用",
          mem === null ? "-" : `${(mem / 1024).toFixed(2)} GiB`,
          use === null || !running ? undefined : `引擎占用 ${(use.rssEngineMib / 1024).toFixed(2)} GiB`,
        ),
        tile(
          "自动回收",
          body.idleStopMs === 0 ? "未启用" : formatDuration(body.idleStopMs),
          `已空闲 ${formatDuration(worker.idleMs)}`,
        ),
        tile("音色包", `${body.voicePacks} 个`, undefined, body.voicePacks === 0 ? "fail" : "ok"),
        tile("字幕客户端", `${body.presenters} 个`),
        tile("并发请求", `${body.inFlight} 个`),
      ),
      el(
        "div",
        { class: "spool" },
        el(
          "div",
          { class: "spool__head" },
          el("p", { class: "spool__label", text: "音频缓存池" }),
          el("span", {
            class: "spool__value",
            text: `${body.spool.entries} 个 · ${formatBytes(body.spool.bytes)} / ${formatBytes(body.spool.maxBytes)} · ${formatPercent(body.spool.bytes, body.spool.maxBytes)}`,
          }),
        ),
        el("progress", { class: "bar", max: String(body.spool.maxBytes), value: String(body.spool.bytes) }),
      ),
    );
  }

  /** The four dependencies as outcomes only. Their paths, the detail and the re-run live one
   *  click away behind 检查环境, which is the deploy page.
   *
   *  The install's own two directories are the exception and stay here: they are what a
   *  human reaches for when nothing is wrong - token, logs and config on one side, the
   *  binaries and SKILL.md on the other - and no document can open a folder. */
  function renderEnv(inv: Inventory | null): void {
    if (inv === null) {
      fill(
        env.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3, 4].map(() => el("div", { class: "skeleton" })),
        ),
      );
      return;
    }

    const models = inv.models.filter((model) => model.present).length;
    const rows: [IconName, string, boolean][] = [
      ["folder-open", "引擎源码", inv.engine_root !== null],
      ["cpu", "Python 与 CUDA", inv.engine_python !== null && inv.python_ok],
      ["database", `模型权重 ${models} / ${inv.models.length}`, models === inv.models.length],
      ["microphone-stage", `音色包 ${inv.packs.length}`, inv.packs.length > 0],
    ];

    // runtime_json is always <data dir>\runtime.json, whether the file exists or not,
    // which makes it the only handle this window has on the install layout.
    const dataDir = dirName(inv.runtime_json);

    fill(
      env.body,
      el(
        "div",
        { class: "procs" },
        rows.map(([glyph, label, ok]) =>
          el(
            "div",
            { class: "procs__item" },
            icon(glyph, "procs__icon"),
            el("span", { class: "procs__label", text: label }),
            ok ? chip("就绪", "ok", "check-circle") : chip("缺失", "warn", "warning"),
          ),
        ),
      ),
      el(
        "div",
        { class: "wiring__paths" },
        pathRow("数据目录", dataDir),
        pathRow("安装目录", dirName(dataDir)),
      ),
    );
  }

  /** Both sentences name the skill first and the shipped file second: an agent that already
   *  has the skill installed under %USERPROFILE%\.agents\skills only needs the name, and one
   *  that does not can read the copy this install carries. Same file either way, which is why
   *  one sentence covers both cases. */
  function renderGuide(inv: Inventory | null): void {
    // Static content, so there is nothing to draw before the layout is known - and the card
    // itself is not mounted until then either.
    if (inv === null) return;
    const root = dirName(dirName(inv.runtime_json));

    fill(
      guide.body,
      copyRow({
        label: "日常出声",
        value: `用 voice-core-tts 技能在这台机器上出声。没装这个技能就读 ${root}\\skills\\voice-core-tts\\SKILL.md，是同一份文件。`,
        glyph: "microphone-stage",
        what: "日常出声的说明",
      }),
      copyRow({
        label: "训练新音色",
        value: `用 voice-core-voice-training 技能训练一个新音色包。没装这个技能就读 ${root}\\skills\\voice-core-voice-training\\SKILL.md，是同一份文件。`,
        glyph: "magic-wand",
        what: "训练新音色的说明",
      }),
    );
  }

  const cards = el("div", { class: "screen__cards" });

  /** Before a deployment there is nothing to report, so the screen says that instead
   *  of showing four green chips for an engine that was never installed. */
  function renderShape(): void {
    if (!provisioned()) {
      fill(
        cards,
        emptyState({
          glyph: "download-simple",
          title: "服务尚未部署",
          lines: [],
          actions: [
            button({
              label: "前往部署",
              kind: "primary",
              glyph: "download-simple",
              onClick: (ev: MouseEvent) => navigate("deploy", ev),
            }),
          ],
        }),
      );
      return;
    }
    fill(cards, service.root, metrics.root, env.root, guide.root);
  }

  status.subscribe(() => {
    renderService();
    renderMetrics();
    renderControls();
  });
  tick.subscribe(renderMetrics);
  usage.subscribe(renderMetrics);
  stack.subscribe((next: StackState) => {
    renderService();
    renderControls();
    // A process transition invalidates the last /api/status observation, so ask
    // again instead of showing a stale worker block for up to 5 s.
    if (next.runtime) void refreshStatus();
  });
  inventory.subscribe((inv) => {
    renderEnv(inv);
    renderGuide(inv);
    renderShape();
    renderControls();
  });

  renderShape();
  renderControls();

  const root = el(
    "div",
    { class: "screen" },
    el(
      "header",
      { class: "screen__head" },
      el(
        "div",
        { class: "screen__titles" },
        el("h1", { class: "screen__title", tabindex: "-1", text: "状态" }),
      ),
    ),
    cards,
  );

  return Object.assign(root, { commandBar });
}
