// Shared state. Three screens read the same four facts, so they are fetched once
// here rather than once per screen: two of them (status, stack) change on a timer
// or an event, and duplicating that would mean two pollers disagreeing about
// whether the runtime is up.

import {
  detect,
  ipcMessage,
  listVoices,
  resourceUsage,
  runtimeStatus,
  type Inventory,
  type Pack,
  type StackState,
  type Status,
  type Usage,
} from "./ipc";
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
    toast(`检测本机环境失败：${ipcMessage(err)}`, "fail");
  }
}

export async function refreshVoices(): Promise<void> {
  try {
    voices.set(await listVoices());
  } catch (err: unknown) {
    toast(`读取音色包失败：${ipcMessage(err)}`, "fail");
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
