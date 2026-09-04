// Status: is it up, what is it holding, and how does a caller reach it.
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

import { el, fill } from "../dom";
import { dirName, formatBytes, formatDuration, formatPercent } from "../format";
import { icon, type IconName } from "../icons";
import {
  RUNTIME_BASE_URL,
  ipcMessage,
  startStack,
  stopStack,
  type Inventory,
  type StackState,
} from "../ipc";
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

/** The sentence the copy-paste snippets use. Japanese because that is what the only
 *  backend speaks; the caller is the one who translates. */
const SPEAK_TEXT = "おかえりなさい、先生。";

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
  const wiring = panel({ title: "使用方式" });

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
      toast(`${start ? "启动" : "停止"}失败：${ipcMessage(err)}`, "fail");
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
        procRow("runtime 服务", processes.runtime, "pulse"),
        procRow("字幕弹窗", processes.presenter, "microphone-stage"),
        procRow("音色模型", processes.model_loaded, "waveform", ["已加载", "未加载"]),
      ),
      current.reachable && body !== null
        ? el("p", {
            class: "panel__meta",
            text: `${body.name} ${body.runtimeVersion} · API v${body.apiVersion} · 已运行 ${formatDuration(body.uptimeMs)}`,
          })
        : null,
      // Only a real error is worth a block: "not listening" is already said by the
      // three rows above and by the rail.
      !current.reachable && current.error !== null && processes.runtime
        ? note("fail", "运行时没有应答", el("p", { class: "note__detail", text: current.error }))
        : null,
      body !== null && body.worker.missing.length > 0
        ? note(
            "warn",
            "有资源不在位",
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
          tile("runtime 服务", "已停止"),
          tile("音色引擎", "未启动"),
          tile("显存", "未占用"),
          tile("内存", "-"),
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
          ? "已启动，模型未加载"
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
        : (card ?? (worker.modelLoaded ? "模型驻留中" : worker.running ? "已释放" : "未占用"));
    const vramSub =
      use !== null && use.engineGpuMib !== null
        ? (card === null ? undefined : `整卡 ${card}`)
        : card === null
          ? undefined
          : worker.modelLoaded
            ? "整卡占用；驱动不按进程细分"
            : "整卡占用";

    fill(
      metrics.body,
      el(
        "div",
        { class: "tiles" },
        tile(
          "runtime 服务",
          `已运行 ${formatDuration(body.uptimeMs)}`,
          `${body.name} ${body.runtimeVersion}`,
          "ok",
        ),
        tile(
          "音色引擎",
          engineText,
          worker.running ? `已运行 ${formatDuration(worker.uptimeMs)}` : undefined,
          engineTone,
        ),
        tile("显存", vram, vramSub, worker.modelLoaded ? "warn" : "idle"),
        tile(
          "内存",
          mem === null ? "-" : `${(mem / 1024).toFixed(2)} GiB`,
          use === null || !running ? undefined : `其中引擎 ${(use.rssEngineMib / 1024).toFixed(2)} GiB`,
        ),
        tile(
          "空闲回收",
          body.idleStopMs === 0 ? "已关闭" : formatDuration(body.idleStopMs),
          `已空闲 ${formatDuration(worker.idleMs)}`,
        ),
        tile("音色包", `${body.voicePacks} 个`, undefined, body.voicePacks === 0 ? "fail" : "ok"),
        tile("字幕订阅者", `${body.presenters} 个`),
        tile("进行中的请求", `${body.inFlight} 个`),
      ),
      el(
        "div",
        { class: "spool" },
        el(
          "div",
          { class: "spool__head" },
          el("p", { class: "spool__label", text: "音频暂存" }),
          el("span", {
            class: "spool__value",
            text: `${body.spool.entries} 个 · ${formatBytes(body.spool.bytes)} / ${formatBytes(body.spool.maxBytes)} · ${formatPercent(body.spool.bytes, body.spool.maxBytes)}`,
          }),
        ),
        el("progress", { class: "bar", max: String(body.spool.maxBytes), value: String(body.spool.bytes) }),
      ),
    );
  }

  /** The four dependencies as outcomes only. The detail, the paths and the re-run
   *  live one click away behind 检查环境, which is the deploy page. */
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
            ok ? chip("就绪", "ok", "check-circle") : chip("缺少", "warn", "warning"),
          ),
        ),
      ),
    );
  }

  function renderWiring(inv: Inventory | null): void {
    if (inv === null) {
      fill(
        wiring.body,
        el(
          "div",
          { class: "skeletons", "aria-hidden": "true" },
          [1, 2, 3].map(() => el("div", { class: "skeleton" })),
        ),
      );
      return;
    }

    // runtime_json is always <data dir>\runtime.json, whether the file exists or
    // not, which makes it the only handle this window has on the install layout.
    const dataDir = dirName(inv.runtime_json);
    const installRoot = dirName(dataDir);
    const tokenPath = `${dataDir}\\token.txt`;
    const cliPath = `${installRoot}\\bin\\voice-core.exe`;

    // Same reason the button sends one: a snippet without a voice is a snippet that
    // fails on paste.
    const voice = inv.packs[0]?.id ?? "<voice-pack-id>";
    const snippet = [
      `$t = (Get-Content -Raw '${tokenPath}').Trim()`,
      `curl.exe -s -X POST ${RUNTIME_BASE_URL}/api/speak \``,
      `  -H "Authorization: Bearer $t" -H "Content-Type: application/json" \``,
      `  --data-raw '{"text":"${SPEAK_TEXT}","voicePackId":"${voice}"}'`,
    ].join("\n");

    // The handoff to somebody else's agent, in English because that is the language a
    // model reasons in most reliably and this string is not read by the user. It
    // teaches nothing itself - it points at the skill file that ships in the install
    // tree, because a prompt goes stale and a file next to the binary does not. The
    // three facts it does state are the ones an agent gets wrong on its own: prefer the
    // CLI, HTTP is for building on the runtime, and a cold start is not a hang.
    const prompt = [
      "This machine has voice-core: local TTS that can make you speak out loud (synthesize + play + subtitle popup).",
      `Read this first and follow it: ${installRoot}\\skills\\voice-core\\SKILL.md`,
      `Default to the CLI: ${cliPath}`,
      "Use the HTTP API only when building software on voice-core-runtime (event stream, concurrency, raw audio).",
      "First utterance cold-starts in 20-60s; allow 120s before calling it a timeout.",
    ].join("\n");

    fill(
      wiring.body,
      copyRow({
        label: "给 AI 的提示词",
        value: prompt,
        glyph: "book-open-text",
        block: true,
        what: "提示词",
      }),
      copyRow({ label: "端点地址", value: RUNTIME_BASE_URL, glyph: "pulse", what: "端点地址" }),
      copyRow({ label: "令牌文件", value: tokenPath, glyph: "key", what: "令牌路径" }),
      copyRow({
        label: "speak 请求（PowerShell）",
        value: snippet,
        glyph: "terminal-window",
        block: true,
        what: "请求片段",
      }),
      copyRow({
        label: "命令行等价写法",
        value: `${cliPath} speak --voice ${voice} --text "${SPEAK_TEXT}" --play auto`,
        glyph: "waveform",
        block: true,
        what: "命令行写法",
      }),
      el(
        "div",
        { class: "wiring__paths" },
        el(
          "div",
          { class: "wiring__path" },
          el("span", { text: "数据目录" }),
          pathText(dataDir, 60),
          openButton(dataDir),
        ),
        el(
          "div",
          { class: "wiring__path" },
          el("span", { text: "安装目录" }),
          pathText(installRoot, 60),
          openButton(installRoot),
        ),
      ),
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
          title: "还没有部署",
          lines: [],
          actions: [
            button({
              label: "开始部署",
              kind: "primary",
              glyph: "download-simple",
              onClick: (ev: MouseEvent) => navigate("deploy", ev),
            }),
          ],
        }),
      );
      return;
    }
    fill(cards, service.root, metrics.root, env.root, wiring.root);
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
    renderWiring(inv);
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
