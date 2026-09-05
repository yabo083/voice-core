// The only module that names a Tauri command or event. CONTRACT 2 is reproduced
// here as types; if a name or a shape changes, it changes once, here.
//
// Casing is not uniform and that is deliberate, not sloppiness:
//   - detect/provision own their shapes, so they are snake_case Rust structs
//     serialized as written;
//   - Status.body is the runtime's own /api/status body forwarded verbatim, so it
//     is camelCase - re-declaring service::Status in the host crate purely to
//     change its casing would be a second source of truth for the same JSON;
//   - every Pack field is a single word, so both conventions are byte-identical.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The host spawns `voice-core-runtime.exe --data-dir <root>\data` and passes no
 *  --bind, so the runtime's own default stands. Printed on the Status screen. */
export const RUNTIME_BASE_URL = "http://127.0.0.1:8760";

export type PackKind = "lora-adapter" | "speaker-embedding" | "reference-audio";

export interface Pack {
  id: string;
  name: string;
  kind: PackKind;
  /** Absolute, or relative to the data dir in a portable install. */
  path: string;
  engine: string;
  languages: string[];
  character?: string | null;
  avatar?: string | null;
}

export interface ModelState {
  repo: string;
  present: boolean;
  gib: number;
}

export interface Inventory {
  engine_root: string | null;
  engine_python: string | null;
  python_ok: boolean;
  cuda: string | null;
  hf_cache: string | null;
  models: ModelState[];
  packs: Pack[];
  /** Absolute path of <data dir>\runtime.json, present or not - which makes it
   *  the app's only handle on the data dir and, one level up, the install root. */
  runtime_json: string;
  disk_free_gib: number;
  needs_gib: number;
}

export interface ProvisionOpts {
  engine_root?: string | null;
  hf_home?: string | null;
  voice_packs?: string | null;
  /** One stage id to run alone, for per-stage retry. Omitted runs all seven. */
  only?: string | null;
  check_only: boolean;
}

/** Verbatim `GET /api/status` body. */
export interface StatusBody {
  name: string;
  runtimeVersion: string;
  apiVersion: number;
  uptimeMs: number;
  voicePacks: number;
  presenters: number;
  inFlight: number;
  idleStopMs: number;
  worker: {
    managed: boolean;
    running: boolean;
    ready: boolean;
    modelLoaded: boolean;
    port: number | null;
    uptimeMs: number;
    idleMs: number;
    missing: string[];
  };
  spool: { entries: number; bytes: number; maxBytes: number };
}

/** Never rejects and never throws on a stopped runtime: down is the normal state
 *  on first run, so it is data, not an error. */
export interface Status {
  reachable: boolean;
  error: string | null;
  body: StatusBody | null;
}

/** Locally measured, not reported by the runtime: the GPU numbers come from
 *  nvidia-smi and the working sets from the OS, so every field is null or zero on a
 *  machine that has neither an NVIDIA card nor a running stack. */
export interface Usage {
  gpuName: string | null;
  gpuUsedMib: number | null;
  gpuTotalMib: number | null;
  /** VRAM held by this stack's own processes, not by the whole card. */
  engineGpuMib: number | null;
  rssRuntimeMib: number;
  rssEngineMib: number;
  rssPresenterMib: number;
  rssManagerMib: number;
}

export const STAGES = [
  "preflight",
  "engine",
  "codec",
  "venv",
  "models",
  "layout",
  "smoke",
] as const;
export type Stage = (typeof STAGES)[number];

export type BootstrapEventKind = "start" | "progress" | "log" | "ok" | "skip" | "fail";

export interface BootstrapEvent {
  ts: number;
  stage: Stage;
  event: BootstrapEventKind;
  message: string;
  /** Bytes in the `models` stage, item counts elsewhere. `total` may be null when
   *  the size is not known yet; `done` never is. */
  done: number | null;
  total: number | null;
  /** Non-null on every `fail`, and on `log` lines that carry one failing check
   *  inside a multi-check stage. Failure is `event === "fail"`, never remedy. */
  remedy: string | null;
}

export interface StackState {
  runtime: boolean;
  presenter: boolean;
  model_loaded: boolean;
}

export const detect = (): Promise<Inventory> => invoke("detect");

/** Spawns `scripts/bootstrap.ps1 -Json`. Resolves when the process EXITS, which
 *  is the app's one signal for "run finished": a -Only run may never emit a
 *  terminal event for the last stage. Rejects on a usage error (non-zero exit)
 *  and when a run is already in flight. */
export const provision = (opts: ProvisionOpts): Promise<void> => invoke("provision", { opts });

/** No-op when nothing is running. */
export const cancelProvision = (): Promise<void> => invoke("cancel_provision");

export const pickFolder = (title: string): Promise<string | null> =>
  invoke("pick_folder", { title });

export const pickFile = (title: string, extensions: string[]): Promise<string | null> =>
  invoke("pick_file", { title, extensions });

export const runtimeStatus = (): Promise<Status> => invoke("runtime_status");

export const resourceUsage = (): Promise<Usage> => invoke("resource_usage");

/** Runtime up: `GET /api/voices`. Runtime down: read straight out of
 *  data/config.json, so registered packs are visible before it ever starts. */
export const listVoices = (): Promise<Pack[]> => invoke("list_voices");

export const registerPack = (pack: Pack): Promise<void> => invoke("register_pack", { pack });

/** Copies the picked image into the pack and resolves with the file name to store in
 *  its manifest - relative to the pack, so the portrait travels with the voice. */
export const importAvatar = (path: string, packPath: string): Promise<string> =>
  invoke("import_avatar", { path, packPath });

/** The pack's own `voicepack.json`, verbatim, or null when it has none. */
export const packManifest = (id: string): Promise<unknown | null> =>
  invoke("pack_manifest", { id });

export const removePack = (id: string): Promise<void> => invoke("remove_pack", { id });

export const startStack = (): Promise<void> => invoke("start_stack");

export const stopStack = (): Promise<void> => invoke("stop_stack");

/** Shell-opens a folder or file. Rejects for anything outside the install root
 *  and the data dir, by design on the host side. */
export const openPath = (path: string): Promise<void> => invoke("open_path", { path });

export const onBootstrapEvent = (fn: (e: BootstrapEvent) => void): Promise<UnlistenFn> =>
  listen<BootstrapEvent>("bootstrap://event", (e) => fn(e.payload));

export const onStackState = (fn: (s: StackState) => void): Promise<UnlistenFn> =>
  listen<StackState>("stack://state", (e) => fn(e.payload));

// --- 配置 screen: the two config files, read-only ---------------------------------------

/** One configuration file as the host read it.
 *
 *  `exists: false` is an answer rather than a failure: runtime.json is not there until a
 *  deployment writes it, and a pack's manifest is optional by design. A file that exists
 *  with `bytes > 0` and empty `text` is one something else has open this instant. */
export interface ConfigFile {
  label: string;
  path: string;
  text: string;
  exists: boolean;
  bytes: number;
}

/** The runtime's merged view of one pack, forwarded verbatim from `GET /api/voices`.
 *
 *  Only `sources` is named, because that is the only field this screen reads by name: the
 *  table is built from its keys, so a field added to the runtime's `VoicePack` appears
 *  here without a change. `sources` is optional on the wire — a runtime older than this
 *  panel answers without it, and the panel talks to whatever is listening on the port. */
export interface EffectivePack {
  sources?: Record<string, string>;
  [field: string]: unknown;
}

/** `data/config.json` and `data/runtime.json`, in that order. */
export const configFiles = (): Promise<ConfigFile[]> => invoke("config_files");

/** The pack's own `voicepack.json` as the file on disk. null when no pack is registered
 *  under `id`; `exists: false` when the pack simply never wrote one. */
export const packManifestFile = (id: string): Promise<ConfigFile | null> =>
  invoke("pack_manifest_file", { id });

/** The runtime's merged view of one pack. null when the runtime is not answering, because
 *  the merge lives there: with it stopped, nothing on this machine can say which file won
 *  a field. */
export const packEffective = (id: string): Promise<EffectivePack | null> =>
  invoke("pack_effective", { id });

// --- 训练 screen: the training pipeline as one job ------------------------------------
//
// The event shape is bootstrap's, key for key, with one addition (`checkpoint`): the panel
// renders one stream renderer, not two.

export const TRAIN_STAGES = [
  "dataset",
  "latents",
  "train",
  "samples",
  "score",
  "install",
] as const;
export type TrainStage = (typeof TRAIN_STAGES)[number];

export type TrainEventKind = "start" | "progress" | "log" | "ok" | "skip" | "fail";

/** One line of a step's stdout, forwarded verbatim. The seven keys are bootstrap's, key for
 *  key; `checkpoint` is this pipeline's addition, and it is optional only because the
 *  runner's synthetic `log` line for a non-JSON stdout line carries the seven and no more. */
export interface TrainEvent {
  ts: number;
  stage: TrainStage;
  event: TrainEventKind;
  message: string;
  done: number | null;
  total: number | null;
  remedy: string | null;
  checkpoint?: string | null;
}

export interface TrainingPreflight {
  python: string | null;
  missing: string[];
  cuda: string | null;
  gpu_name: string | null;
  vram_free_mib: number | null;
  vram_total_mib: number | null;
  runtime_reachable: boolean;
  model_loaded: boolean;
  running: boolean;
  pack_id: string | null;
  blockers: string[];
}

export interface TrainRequest {
  audio_dir: string;
  transcripts: string | null;
  speaker_id: string;
  pack_id: string;
  display_name: string;
  character: string | null;
  avatar: string | null;
  batch_size: number;
  max_steps: number;
  learning_rate: number;
  save_every: number;
  /** Permission to delete the previous run of this voice. False unless the user ticked the
   *  confirm: `start_training` refuses and names what is at risk. */
  overwrite: boolean;
}

export interface InstallRequest {
  checkpoint: string;
  pack_id: string;
  display_name: string;
  character: string | null;
  avatar: string | null;
}

/** `prepare_dataset.py`'s QA report, verbatim — which is why these are its snake_case field
 *  names and not this app's. */
export interface QaReport {
  count: number;
  total_minutes: number;
  duration_mean_s: number;
  duration_p05_s: number;
  duration_p95_s: number;
  duration_max_s: number;
  sample_rates: number[];
  channels: number[];
  subtypes: string[];
  problems: { clip: string; issue: string }[];
  skipped: { clip: string; reason: string }[];
}

export interface TrainingCheckpoint {
  name: string;
  path: string;
  step: number | null;
  val_loss: number | null;
  lower_bound: number | null;
  mean: number | null;
  best: boolean;
}

export interface TrainingResult {
  dir: string;
  exists: boolean;
  qa: QaReport | null;
  request: TrainRequest | null;
  checkpoints: TrainingCheckpoint[];
  /** How many of those checkpoints no pack has been installed from. Non-zero is what makes
   *  starting again refuse until it is allowed explicitly. */
  at_risk: number;
}

/** Answers without starting anything, and without touching the GPU while a run is live. */
export const trainingPreflight = (): Promise<TrainingPreflight> => invoke("training_preflight");

/** Resolves when the last step's process EXITS. A failed step resolves too: it reported
 *  itself on the stream, with its remedy, while it was happening. */
export const startTraining = (req: TrainRequest): Promise<void> => invoke("start_training", { req });

/** No-op when nothing is running. Kills the trainer and its DataLoader workers with it. */
export const cancelTraining = (): Promise<void> => invoke("cancel_training");

export const installTrainedPack = (req: InstallRequest): Promise<void> =>
  invoke("install_trained_pack", { req });

/** What a run left on disk, for a pack that may never have been trained. */
export const trainingResult = (packId: string): Promise<TrainingResult> =>
  invoke("training_result", { packId });

export const onTrainEvent = (fn: (e: TrainEvent) => void): Promise<UnlistenFn> =>
  listen<TrainEvent>("train://event", (e) => fn(e.payload));

/** Tauri rejects with a plain string; a dev-server tab outside Tauri rejects with
 *  a TypeError. Both must read as one sentence in a toast. */
export function ipcMessage(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
