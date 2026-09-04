// The shell: a left rail, one scrolling content region, three screens, and a command
// bar pinned under the scroll region.
//
// A rail rather than a top nav for three reasons. The window is wide enough that
// 216px costs nothing, while vertical space is exactly what the stage list and the
// log pane are short of. The rail keeps the runtime's state on screen during an
// hour-long download, which a horizontal strip cannot do without competing with the
// native title bar. And Windows 11's own Settings uses a left rail, so the muscle
// memory is already there.
//
// All three screens are built once at boot and retained. Deploy must be alive from
// the first frame or a bootstrap://event arriving before its first visit would be
// dropped, and keeping the others alive means navigating away from a running
// provision and back does not lose the log or the stage timings.
//
// Deploy is not a permanent destination. Once the engine is installed, the rail item
// disappears: a tab whose job is finished is a tab that trains people to ignore the
// rail. It stays reachable from 状态 -> 环境 -> 检查环境 as a transient page with a
// back arrow, and in that mode the rail keeps 状态 marked as current, because that is
// where the back arrow goes.

import { el, fill } from "./dom";
import { brandMark, icon, type IconName } from "./icons";
import { ipcMessage, onStackState, type Inventory } from "./ipc";
import { inventory, refreshInventory, stack, startStatusPolling, status } from "./state";
import { toast } from "./toast";
import { createDeployScreen, type DeployScreen } from "./screens/deploy";
import { createStatusScreen } from "./screens/status";
import { createVoicesScreen } from "./screens/voices";

type ScreenId = "deploy" | "voices" | "status";

/** Screens may own a command bar; the shell, not the screen, decides where it sits. */
type ScreenElement = HTMLElement & { commandBar?: HTMLElement };

interface NavSpec {
  id: ScreenId;
  label: string;
  glyph: IconName;
}

const NAV: NavSpec[] = [
  { id: "deploy", label: "部署", glyph: "download-simple" },
  { id: "voices", label: "音色", glyph: "microphone-stage" },
  { id: "status", label: "状态", glyph: "pulse" },
];

function mount(app: HTMLElement): void {
  const deploy: DeployScreen = createDeployScreen();
  const screens: Record<ScreenId, ScreenElement> = {
    deploy,
    voices: createVoicesScreen(),
    status: createStatusScreen(),
  };

  const buttons = {} as Record<ScreenId, HTMLButtonElement>;
  const badges = {} as Record<ScreenId, HTMLElement>;
  const items = {} as Record<ScreenId, HTMLElement>;
  const main = el("main", { class: "main", id: "main" });
  const bar = el("div", { class: "cmdslot" });

  let active: ScreenId | null = null;

  /** True once the engine is installed, which is what retires the Deploy tab. */
  function provisioned(): boolean {
    const inv = inventory.value;
    return inv !== null && inv.engine_python !== null && inv.python_ok;
  }

  /** `focus` is false only for the screen the window opens on: nothing has been
   *  navigated yet, and Chromium treats a programmatic focus with no preceding
   *  interaction as keyboard focus, which would ring the title on every launch. */
  function show(id: ScreenId, focus = true): void {
    // Deploy after provisioning is a sub-page of 状态, so the rail highlight and the
    // back arrow both point there.
    const transient = id === "deploy" && provisioned();
    if (id === "deploy") deploy.setTransient(transient);
    const current: ScreenId = transient ? "status" : id;

    if (active !== id) {
      active = id;
      for (const spec of NAV) screens[spec.id].hidden = spec.id !== id;
      main.scrollTop = 0;
    }
    for (const spec of NAV) {
      const nav = buttons[spec.id];
      const isCurrent = spec.id === current;
      nav.classList.toggle("is-active", isCurrent);
      if (isCurrent) nav.setAttribute("aria-current", "page");
      else nav.removeAttribute("aria-current");
    }

    // One command bar on screen at a time, owned by whichever screen is showing.
    const own = screens[id].commandBar;
    fill(bar, own ?? null);
    bar.hidden = own === undefined;

    // Keyboard users must land inside the screen they just chose, not stay parked
    // in the rail with the content silently swapped behind them.
    if (focus) screens[id].querySelector<HTMLElement>(".screen__title")?.focus();
  }

  const navList = el(
    "ul",
    { class: "rail__list" },
    NAV.map((spec) => {
      const badge = el("span", { class: "navitem__badge" });
      const nav = el(
        "button",
        {
          class: "navitem",
          type: "button",
          // `detail === 0` means the button was activated from the keyboard. A mouse
          // user does not need focus yanked onto the heading; a keyboard user does.
          onclick: (ev: MouseEvent) => show(spec.id, ev.detail === 0),
        },
        icon(spec.glyph, "navitem__icon"),
        el("span", { class: "navitem__label", text: spec.label }),
        badge,
      );
      const item = el("li", {}, nav);
      buttons[spec.id] = nav;
      badges[spec.id] = badge;
      items[spec.id] = item;
      return item;
    }),
  );

  const railState = el("div", { class: "rail__state" });

  function renderRail(): void {
    const current = status.value;
    const processes = stack.value;
    const inv = inventory.value;

    fill(
      railState,
      el(
        "div",
        { class: `livestate livestate--${current.reachable ? "up" : "down"}` },
        el("span", { class: "livestate__dot", "aria-hidden": "true" }),
        el("span", {
          class: "livestate__text",
          text: current.reachable ? "服务运行中" : processes.runtime ? "服务启动中" : "服务已停止",
        }),
      ),
    );

    // Badges only exist when they carry information: a rail that always shows three
    // counters trains people to stop reading it.
    const packCount = inv?.packs.length ?? 0;
    badges.voices.textContent = packCount > 0 ? String(packCount) : "";
    badges.status.textContent = current.reachable ? "运行中" : "";

    // The Deploy tab exists only while there is something to deploy. Hiding the row
    // rather than the button keeps the list from leaving a gap behind. It stays gone
    // even while its own transient page is open, because that page highlights 状态 and
    // carries its own back arrow - a visible row that is not the current one would be
    // a second, contradictory answer to "where am I".
    const provisionedNow = provisioned();
    items.deploy.hidden = provisionedNow;
    badges.deploy.textContent = provisionedNow || inv === null ? "" : "待部署";
  }

  fill(
    app,
    el(
      "div",
      { class: "app" },
      el(
        "nav",
        { class: "rail", "aria-label": "主导航" },
        el(
          "div",
          { class: "brand" },
          brandMark(),
          el(
            "div",
            { class: "brand__text" },
            el("p", { class: "brand__name", text: "voice-core" }),
            el("p", { class: "brand__role", text: "语音合成控制台" }),
          ),
        ),
        navList,
        el("div", { class: "rail__foot" }, railState),
      ),
      el("div", { class: "content" }, main, bar),
    ),
  );

  for (const spec of NAV) {
    screens[spec.id].hidden = true;
    main.appendChild(screens[spec.id]);
  }

  status.subscribe(renderRail);
  stack.subscribe(renderRail);
  inventory.subscribe(renderRail);

  document.addEventListener("app:navigate", (ev: Event) => {
    const { to, focus } = (ev as CustomEvent<{ to: ScreenId; focus: boolean }>).detail;
    if (to === "deploy" || to === "voices" || to === "status") show(to, focus);
  });

  // Where the window opens is a statement about what is left to do: an unprovisioned
  // tree opens on Deploy, a provisioned one without voices opens on Voices, and a
  // finished install opens on Status.
  function landing(inv: Inventory | null): ScreenId {
    if (inv === null || inv.engine_python === null || !inv.python_ok) return "deploy";
    return inv.packs.length === 0 ? "voices" : "status";
  }

  // boot() gives detect() a bounded wait so a slow host cannot leave a blank window,
  // which means the first routing decision may have been made with no inventory at
  // all. When the real answer lands, re-decide once - otherwise a provisioned tree
  // sits on a Deploy page whose rail row has already retired itself, with nothing in
  // the rail marked current.
  let provisional = inventory.value === null;
  inventory.subscribe((inv) => {
    if (!provisional || inv === null) return;
    provisional = false;
    if (active === "deploy" && !deploy.isBusy()) show(landing(inv), false);
  });

  show(landing(inventory.value), false);
}

async function boot(): Promise<void> {
  const app = document.querySelector<HTMLElement>("#app");
  if (app === null) return;

  window.addEventListener("unhandledrejection", (ev: PromiseRejectionEvent) => {
    toast(ipcMessage(ev.reason), "fail");
  });

  void onStackState((next) => stack.set(next));

  // detect() decides which screen opens, so the first paint waits for it - but never
  // for long: a host that cannot answer must not leave a blank window behind.
  await Promise.race([
    refreshInventory(),
    new Promise<void>((resolve) => window.setTimeout(resolve, 2500)),
  ]);

  mount(app);
  startStatusPolling();
}

void boot();
