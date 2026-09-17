// Shared state. Three screens read the same five facts, so they are fetched once
// here rather than once per screen: two of them (status, stack) change on a timer
// or an event, and duplicating that would mean two pollers disagreeing about
// whether the runtime is up.

import {
  detect,
  ipcMessage,
  listVoices,
  onConfigChanged,
  resourceUsage,
  runtimeStatus,
  type Inventory,
  type Pack,
  type StackState,
  type Status,
  type Usage,
} from "./ipc";
import { t } from "./i18n";
import { toast } from "./toast";

class Store<T> {
  #value: T;
  #listeners = new Set<(value: T) => void>();

  constructor(initial: T) {
    this.#value = initial;
  }

  get value(): T {
    return this.#value;
  }

  set(next: T): void {
    this.#value = next;
    for (const listener of this.#listeners) listener(next);
  }

  /** Calls back immediately with the current value, so a screen mounted after the
   *  first poll renders live data instead of an empty frame. */
  subscribe(listener: (value: T) => void): () => void {
    this.#listeners.add(listener);
    listener(this.#value);
    return () => {
      this.#listeners.delete(listener);
    };
  }
}

/** null means "detect() has not answered yet", which is a different screen state
 *  from "detect() found nothing". */
export const inventory = new Store<Inventory | null>(null);

// --- deploy preview fixtures --------------------------------------------------
//
// Outside Tauri (`?vcPreview=deploy` in a plain browser) `detect()` rejects, and
// the deploy screen would sit on skeletons forever — the update panel solved the
// same problem with `?vcPreview=update` and its own fixture. These are the deploy
// twin: two inventories covering the two renders the screen exists for, switched
// by `#inv=` (default `ready`). Numbers are fixed so screenshots compare; paths
// mirror the portable install layout. main.ts feeds the fixture into the store
// when the preview is on, and every screen keeps reading the real code paths.
export const PREVIEW_INVENTORIES: Record<"ready" | "missing", Inventory> = {
  ready: {
    engine_root: "E:\\NewToolBox\\voice-core\\runtime\\engine",
    engine_python: "E:\\NewToolBox\\voice-core\\runtime\\python\\Scripts\\python.exe",
    python_ok: true,
    cuda: "12.8",
    hf_cache: "E:\\NewToolBox\\voice-core\\models\\huggingface",
    models: [
      { repo: "Aratako/Irodori-TTS-v4.1-Small", present: true, gib: 2.86 },
      { repo: "sbintuitions/modernbert-ja-310m", present: true, gib: 1.18 },
      { repo: "Aratako/Semantic-DACVAE-Japanese-32dim", present: true, gib: 0.4 },
    ],
    packs: [
      { id: "preview-miyu", name: "霞沢美游", kind: "lora-adapter", path: "voicepacks/miyu", engine: "irodori", languages: ["ja"], character: "霞沢美游", avatar: null },
      { id: "preview-shun", name: "幼年瞬", kind: "lora-adapter", path: "voicepacks/shun", engine: "irodori", languages: ["ja"], character: "幼年瞬", avatar: null },
    ],
    runtime_json: "E:\\NewToolBox\\voice-core\\data\\runtime.json",
    disk_free_gib: 382.5,
    needs_gib: 0,
  },
  missing: {
    engine_root: null,
    engine_python: null,
    python_ok: false,
    cuda: "12.8",
    hf_cache: null,
    models: [
      { repo: "Aratako/Irodori-TTS-v4.1-Small", present: false, gib: 2.86 },
      { repo: "sbintuitions/modernbert-ja-310m", present: false, gib: 1.18 },
      { repo: "Aratako/Semantic-DACVAE-Japanese-32dim", present: false, gib: 0.4 },
    ],
    packs: [],
    runtime_json: "E:\\NewToolBox\\voice-core\\data\\runtime.json",
    disk_free_gib: 382.5,
    needs_gib: 4.44,
  },
};

/** Which fixture `#inv=` names, defaulting to the provisioned tree — the state a
 *  post-install panel actually opens on. */
export function previewInventory(): Inventory {
  const name = window.location.hash.replace(/^#inv=/, "");
  return name === "missing" ? PREVIEW_INVENTORIES.missing : PREVIEW_INVENTORIES.ready;
}

/** The one definition of "this install is done": a working Python, the engine's
 *  interpreter beside it, and every model weight the manifest expects on disk. The
 *  rail's retire-deploy rule, the landing screen and the deploy screen's final page
 *  all read this one predicate, so they cannot disagree about whether work remains.
 *  An empty model manifest keeps this false - detect() always reports the engine's
 *  weight manifest, so an empty list means the answer did not arrive in full. */
export function envComplete(inv: Inventory | null): boolean {
  return (
    inv !== null &&
    inv.engine_python !== null &&
    inv.python_ok &&
    inv.models.length > 0 &&
    inv.models.every((model) => model.present)
  );
}

export const voices = new Store<Pack[] | null>(null);
export const status = new Store<Status>({ reachable: false, error: null, body: null });
export const stack = new Store<StackState>({ runtime: false, presenter: false, model_loaded: false });
export const usage = new Store<Usage | null>(null);
/** Increments once a second while the window is visible, for anything that has to
 *  redraw on a clock rather than on new data. */
export const tick = new Store<number>(0);

export async function refreshInventory(): Promise<void> {
  try {
    inventory.set(await detect());
  } catch (err: unknown) {
    toast(t.common.detectFailed(ipcMessage(err)), "fail");
  }
}

export async function refreshVoices(): Promise<void> {
  try {
    voices.set(await listVoices());
  } catch (err: unknown) {
    toast(t.common.loadPacksFailed(ipcMessage(err)), "fail");
  }
}

export async function refreshStatus(): Promise<void> {
  try {
    status.set(await runtimeStatus());
  } catch (err: unknown) {
    // runtime_status() resolves even when the runtime is down, so a rejection here
    // means the host itself failed - worth a toast, unlike a stopped runtime.
    status.set({ reachable: false, error: ipcMessage(err), body: null });
  }
}

export async function refreshUsage(): Promise<void> {
  try {
    usage.set(await resourceUsage());
  } catch {
    // Measurement is decoration: a machine without nvidia-smi, or a query that lost a
    // race with a process exiting, must not put an error in front of the user.
    usage.set(null);
  }
}

/** One timer, two cadences.
 *
 *  Status is polled every second, not every five. The earlier version polled slowly
 *  and let the screen extrapolate uptime between answers, which made the readouts
 *  jump - and occasionally count backwards, because an extrapolated clock and the
 *  runtime's own measurement do not agree to the millisecond. A loopback GET against a
 *  process on the same machine costs about a millisecond, so the fix is to stop
 *  guessing and ask.
 *
 *  Memory is measured every 2 s: it spawns nvidia-smi, which is far more expensive
 *  than an HTTP GET and changes far more slowly than a clock.
 *
 *  Everything stops while the window is hidden: closing it only hides it to the tray,
 *  and a window nobody can see has no reason to keep asking - least of all to keep
 *  starting a subprocess. */
export function startStatusPolling(): void {
  const USAGE_EVERY = 2;
  let second = 0;

  void refreshStatus();
  void refreshUsage();

  window.setInterval(() => {
    if (document.visibilityState !== "visible") return;
    second += 1;
    tick.set(second);
    void refreshStatus();
    if (second % USAGE_EVERY === 0) void refreshUsage();
  }, 1000);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    void refreshStatus();
    void refreshUsage();
  });
}

/** External config.json changes reach the stores here, once.
 *
 *  The event carries only a path, so the answer to "what changed" is a re-read of the two
 *  stores that each hold half the picture: `voices` reads config.json itself (and the
 *  runtime when it is up), while `inventory` carries the pack list the deploy screen's
 *  badge and rail count derive from.
 *
 *  Writes arrive in bursts — registering a pack touches config.json more than once, and an
 *  agent installing one writes it repeatedly — so events are debounced into one refresh
 *  per quiet 500 ms rather than one per event. Writes this panel made do not come through
 *  here: their screens re-read after their own command resolves, which is observing the
 *  result rather than eavesdropping on the notification. */
export function startConfigWatcher(): void {
  let timer = 0;
  void onConfigChanged(() => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      timer = 0;
      void refreshVoices();
      void refreshInventory();
    }, 500);
  });
}
