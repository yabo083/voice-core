"""Irodori-TTS synthesis worker for voice-core v2.

Contract with the runtime (loopback HTTP, JSON control only):

    GET  /health      -> {"ready": bool, "modelLoaded": bool}
    POST /load        -> {"modelLoaded": true, "loadMs": int}
                      -> {"error": "model load failed: ..."} on failure
                         Idempotent: an already loaded model answers at once with
                         loadMs 0, and a load that is already in flight is waited for
                         rather than started a second time.
    POST /unload      -> {"modelLoaded": false, "freedMs": int}
                         Drops the model and hands the VRAM back while the process
                         stays alive, so the next utterance repays the model load but
                         not the multi-second torch import. Idempotent.
    POST /synthesize  {"text": str,
                       "outPath": str,                      # runtime-owned spool path
                       "voicePack": {"kind": str, "path": str|[str]} | null,
                       "seed": int|None,
                       "numSteps": int}
                      -> {"sampleRate": int, "durationMs": int, "bytes": int}
                      -> {"error": str} on failure, prefixed with the stage that failed
                         ("model load failed: " / "synthesis failed: ") because the
                         runtime maps the two to different error codes (src/engine.rs).

Audio never travels in JSON. The runtime reserves `outPath` inside its spool and
this worker writes the WAV straight there, so no base64 exists in the system and
neither process ever holds a second copy of the samples.

The model loads lazily on the first synthesize or eagerly on /load, and either way
exactly once. Process lifetime is the runtime's business: it starts this worker on
demand and terminates the tree through a job object.

Cold-path observability: one plain-text line per event on stdout, which the runtime
tees to data/logs/tts-worker.out.log:

    [worker] stage=<name> t_ms=<since interpreter start> [ms=<this stage>] [k=v ...]

Booleans print as true/false, values containing a space are double-quoted, and every
duration is milliseconds. The stage names are parsed by whoever measures the cold
path, so they are API: boot.interpreter, boot.config, boot.imports, boot.device,
boot.listening, model.load.start, model.load.step, model.load.done,
model.load.failed, synthesize.done, model.unload.done. model.load.step itemises the
model load — the single biggest cost in the product, and until now one opaque number:
`step=` names the sub-stage and the line carries the allocator's allocated/reserved
pair at that instant.
"""
from __future__ import annotations

import argparse  # noqa: E402
import functools  # noqa: E402
import gc  # noqa: E402
import inspect  # noqa: E402
import os  # noqa: E402
import sys  # noqa: E402
import threading  # noqa: E402
import time  # noqa: E402
import traceback  # noqa: E402
from contextlib import asynccontextmanager, contextmanager, nullcontext  # noqa: E402
from pathlib import Path  # noqa: E402

_T0 = time.perf_counter()
"""Zero for every t_ms below. The interpreter's own startup happens before any Python
in this file runs, so the runtime hands us its spawn instant to cover that part."""


def _elapsed_ms(since: float) -> float:
    return (time.perf_counter() - since) * 1000.0


def _render(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        return f"{value:.1f}"
    text = str(value)
    return f'"{text}"' if not text or " " in text else text


def _stage(name: str, *, stream=None, **fields: object) -> None:
    """One line per cold-path event. Fields whose value is None are dropped, so a
    caller can pass an optional measurement without branching."""
    parts = [f"[worker] stage={name}", f"t_ms={_elapsed_ms(_T0):.1f}"]
    parts.extend(f"{key}={_render(value)}" for key, value in fields.items() if value is not None)
    print(" ".join(parts), file=stream or sys.stdout, flush=True)


def _reason(exc: BaseException) -> str:
    """Some exceptions carry only their type; the caller still needs a sentence."""
    return str(exc) or type(exc).__name__


def _argv_value(flag: str) -> str | None:
    """Flags the boot lines need before argparse can run: argparse lives in main(),
    on the far side of a multi-second import block this file has to report on."""
    for index, item in enumerate(sys.argv):
        if item == flag and index + 1 < len(sys.argv):
            return sys.argv[index + 1]
    return None


_PORT_ARG = _argv_value("--port")
_ROOT_ARG = _argv_value("--root")
if _ROOT_ARG is None:
    raise SystemExit("--root is required: pass the irodori-tts engine root")
ROOT = Path(_ROOT_ARG)
REPO = ROOT / "webui" / "Irodori-TTS"

_spawn_epoch_ms = _argv_value("--spawn-epoch-ms")
_spawn_ms: float | None = None
if _spawn_epoch_ms is not None:
    try:
        # CreateProcess, the interpreter's own startup (site, .pth files, the venv) and
        # this module's stdlib imports — everything before the first line we can time.
        _spawn_ms = time.time() * 1000.0 - float(_spawn_epoch_ms)
    except ValueError:
        _spawn_ms = None
_stage(
    "boot.interpreter",
    ms=_spawn_ms,
    pid=os.getpid(),
    python=f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}",
)

# The model cache may live outside the engine tree — a packaged install keeps it
# under <root>/models so the engine source stays swappable — so honour whatever
# the runtime handed us and only fall back to the in-tree location.
os.environ.setdefault("HF_HOME", str(ROOT / "model" / "huggingface"))
os.environ.setdefault("HF_HUB_CACHE", str(Path(os.environ["HF_HOME"]) / "hub"))
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
# Reserved VRAM runs ~1.4 GB above allocated (measured: 1755.6 MB allocated against
# 3178.0 MB reserved), which is allocator overhang every other process on the GPU still
# pays for. Expandable segments let the caching allocator grow one mapping instead of
# rounding blocks up into new segments. MEASURED RESULT ON THIS BOX: no change to either
# number for this workload — the overhang is the engine's own transient peak, not
# fragmentation. Kept because it costs nothing, is the right default for a long-lived
# process that loads and unloads repeatedly, and `boot.device` now reports whether it is
# in effect. setdefault, so an operator can override it; torch ignores keys it does not
# know and the variable is inert without CUDA, so it stays portable.
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
HUB = Path(os.environ["HF_HUB_CACHE"])
sys.path.insert(0, str(ROOT / "webui" / "dacvae"))
sys.path.insert(0, str(REPO))

# Printed before the import block, not after: when torch or the engine fails to
# import, this line is the only record of which tree and which cache it tried.
_stage(
    "boot.config",
    port=_PORT_ARG,
    root=str(ROOT),
    hf_home=os.environ["HF_HOME"],
    hf_cache=str(HUB),
    hf_offline=os.environ["HF_HUB_OFFLINE"],
)

_t_imports = time.perf_counter()

# Only what defining the app needs. torch and the engine are 9.2 s of imports on this
# box (`boot.imports` in tts-worker.out.log) and they are NOT needed to answer /health,
# so they move to a background thread: the port binds in ~0.3 s instead of ~9.5 s, the
# runtime's readiness handshake stops covering the import, and the import overlaps with
# whatever the caller does between spawning us and asking for audio. Nothing gets faster
# in total; the wait moves off the path where a human is blocked.
from fastapi import FastAPI  # noqa: E402
from pydantic import BaseModel  # noqa: E402

torch = None
InferenceRuntime = None
RuntimeKey = None
SamplingRequest = None

_IMPORTS = threading.Condition()
_IMPORT_STATE: dict[str, object] = {"done": False, "error": None}


def _import_engine() -> None:
    """Import torch and the engine, then probe the device. Runs once, on its own thread,
    started before uvicorn. A failure here is recorded and re-raised to whoever asks for
    a model — never at startup, because a worker that cannot answer /health at all is
    indistinguishable from one that never started."""
    global torch, InferenceRuntime, RuntimeKey, SamplingRequest
    try:
        import torch as _torch

        from irodori_tts.inference_runtime import (
            InferenceRuntime as _InferenceRuntime,
            RuntimeKey as _RuntimeKey,
            SamplingRequest as _SamplingRequest,
        )

        torch = _torch
        InferenceRuntime = _InferenceRuntime
        RuntimeKey = _RuntimeKey
        SamplingRequest = _SamplingRequest
        _stage("boot.imports", ms=_elapsed_ms(_t_imports), torch=torch.__version__)

        t_probe = time.perf_counter()
        cuda_available = torch.cuda.is_available()
        device_name: str | None = None
        probe_error: str | None = None
        try:
            if cuda_available:
                # This is what actually initializes the CUDA context, which is why the
                # stage carries its own ms: the cost is real, but the load would pay it.
                device_name = torch.cuda.get_device_name(0)
        except Exception as exc:  # a broken driver must fail /load, not startup
            probe_error = _reason(exc)
        _stage(
            "boot.device",
            ms=_elapsed_ms(t_probe),
            cuda_available=cuda_available,
            device=device_name,
            probe_error=probe_error,
            alloc_conf=os.environ.get("PYTORCH_CUDA_ALLOC_CONF"),
        )
    except BaseException as exc:
        _IMPORT_STATE["error"] = _reason(exc)
        _stage("boot.imports.failed", ms=_elapsed_ms(_t_imports), reason=_reason(exc))
    finally:
        with _IMPORTS:
            _IMPORT_STATE["done"] = True
            _IMPORTS.notify_all()


def _await_imports() -> None:
    """Block until the engine is importable. Called by every path that touches torch."""
    with _IMPORTS:
        while not _IMPORT_STATE["done"]:
            _IMPORTS.wait()
    if _IMPORT_STATE["error"] is not None:
        raise RuntimeError(f"engine imports failed: {_IMPORT_STATE['error']}")


threading.Thread(target=_import_engine, name="engine-import", daemon=True).start()

_DEFAULT_PLACEMENT = {
    "model_device": "cuda",
    "codec_device": "cuda",
    "model_precision": "bf16",
    "codec_precision": "bf16",
}
PLACEMENT = dict(_DEFAULT_PLACEMENT)
"""Where the model and the codec run. These four are the shipped behaviour; main()
overwrites them from the CLI so a measured sweep (codec on CPU, fewer steps) needs no
config-file surface and no new policy."""

STATE = {"runtime": None, "loading": False, "error": None, "busy": 0}
_COND = threading.Condition()
"""Guards every STATE mutation and wakes whoever is waiting for a load. /health
deliberately does not take it — a dict read is atomic under the GIL — so a load that
runs for half a minute cannot stall the runtime's readiness poll into killing us."""


@asynccontextmanager
async def _lifespan(_app: FastAPI):
    # uvicorn awaits lifespan startup and only then calls loop.create_server
    # (uvicorn/server.py: startup()), so this is the last point before the port can
    # answer: one bind() separates this line from the runtime's /health poll.
    _stage("boot.listening", ms=_elapsed_ms(_t_imports), port=_PORT_ARG)
    yield


app = FastAPI(lifespan=_lifespan)


class VoicePackBody(BaseModel):
    kind: str
    path: str | list[str]


class SynthesizeBody(BaseModel):
    text: str
    outPath: str
    voicePack: VoicePackBody | None = None
    seed: int | None = 1234
    numSteps: int = 32


def _checkpoint() -> Path:
    """Locate the base checkpoint inside whatever hub cache we were pointed at."""
    pattern = "models--Aratako--Irodori-TTS-v4.1-Small/snapshots/*/model.safetensors"
    found = next(HUB.glob(pattern), None)
    if found is None:
        raise FileNotFoundError(f"no Irodori checkpoint under {HUB} (looked for {pattern})")
    return found


def _vram_mb() -> tuple[float, float] | None:
    """(allocated, reserved) MiB, or None without CUDA. `reserved` is the number that
    matters for reclaim: torch keeps freed blocks in its caching allocator, so only
    empty_cache() hands them back to the driver."""
    if torch is None or not torch.cuda.is_available():
        return None
    return torch.cuda.memory_allocated() / 1048576.0, torch.cuda.memory_reserved() / 1048576.0


def _vram_peak_mb() -> tuple[float, float] | None:
    """(max allocated, max reserved) MiB since the last reset, or None without CUDA. The
    transient peak is what decides whether a second GPU tenant fits, and it is invisible
    in the steady-state numbers `_vram_mb` reports."""
    if torch is None or not torch.cuda.is_available():
        return None
    return (
        torch.cuda.max_memory_allocated() / 1048576.0,
        torch.cuda.max_memory_reserved() / 1048576.0,
    )


_RANDOM_INIT = (
    "uniform_",
    "normal_",
    "trunc_normal_",
    "kaiming_uniform_",
    "kaiming_normal_",
    "xavier_uniform_",
    "xavier_normal_",
    "orthogonal_",
    "sparse_",
)
"""The `torch.nn.init` entry points that fill a tensor with random numbers. Everything
else there (`zeros_`, `ones_`, `constant_`, `eye_`) is cheap and deliberate, so it stays."""


def _leave_uninitialised(tensor, *_args, **_kwargs):
    return tensor


@contextmanager
def _no_wasted_init():
    """Build the DiT without initialising weights the checkpoint replaces a moment later.

    MEASURED on this box, one construction per process: `TextToLatentRFDiT.__init__` costs
    13.29 s as shipped and 9.37 s with these no-ops — 3.9 s of RNG for 766M parameters, all
    of them overwritten by `load_state_dict`. (The remaining 9 s is not initialisation at
    all: it is transformers' lazy-import machinery resolving the ModernBERT backbone class,
    which cannot be moved off a cold utterance's critical path from here — the first
    request waits for the model either way.)

    Safe as well as faster: the engine loads with torch's default `strict=True`, which
    raises unless every parameter and every persistent buffer appears in the checkpoint, so
    a checkpoint that would leave a tensor holding uninitialised memory fails the load
    instead of producing sound. Verified against this checkpoint: "<All keys matched
    successfully>", 714 tensors, no non-finite parameter afterwards, and the audio is
    bitwise identical to the audio from before this change.

    Non-persistent buffers are untouched by this: the two `_freqs_cis_cache` entries and
    ModernBERT's `inv_freq` tables are computed, not sampled.

    Scoped to that one constructor and no wider. The codec goes through
    `audiotools.ml.BaseModel.load`, which loads with `strict=False`, so a key it failed to
    match would silently keep whatever the constructor left behind — there, initialisation
    is load-bearing.
    """
    init = torch.nn.init
    saved = {name: getattr(init, name) for name in _RANDOM_INIT if hasattr(init, name)}
    for name in saved:
        setattr(init, name, _leave_uninitialised)
    try:
        yield
    finally:
        for name, original in saved.items():
            setattr(init, name, original)


_LOAD_PROBES = (
    # (attribute of inference_runtime owning the call, call, stage name, context to run in)
    (None, "_load_checkpoint_for_inference", "ckpt_read", None),
    ("TextToLatentRFDiT", "__init__", "model_build", _no_wasted_init),
    ("TextToLatentRFDiT", "load_state_dict", "state_dict_load", None),
    ("TextToLatentRFDiT", "to", "model_to_device", None),
    (None, "_move_inference_module", "model_cast", None),
    ("PretrainedTextTokenizer", "from_pretrained", "tokenizer_load", None),
    ("DACVAECodec", "load", "codec_load", None),
)
"""What the model load is made of, and the one place it is worth changing.

`InferenceRuntime.from_key` is a single call from here and the engine tree is the product,
not ours to edit, so wrapping the attributes it reaches for is how we both see inside it
and skip the one step in it that is pure waste. Every wrapper is installed for the duration
of one load and removed afterwards.

Listed in the order `from_key` calls them. A nested call reports first, which is why
`model_to_device` reports twice: once for `model.to(cuda)`, once for the redundant no-op
move inside `_move_inference_module`, whose own line follows and encloses it."""


def _install_load_probes(report) -> list[tuple[object, str, object, bool]]:
    """Wrap every `_LOAD_PROBES` entry so it reports its own ms through `report`. Returns
    what `_remove_load_probes` needs; the caller MUST restore, or a probe outlives the load
    it was measuring and taxes every later call.

    A renamed internal costs a measurement, not a load: an attribute that is not there is
    skipped, because the engine source is swappable by design.
    """
    from irodori_tts import inference_runtime as engine

    undo: list[tuple[object, str, object, bool]] = []
    for owner_name, attr, stage, inside in _LOAD_PROBES:
        owner = engine if owner_name is None else getattr(engine, owner_name, None)
        if owner is None:
            continue
        raw = inspect.getattr_static(owner, attr, None)
        target = raw.__func__ if isinstance(raw, (classmethod, staticmethod)) else raw
        if not callable(target):
            continue
        # Before the swap, or the answer is always yes: `to` lives on nn.Module, not on
        # the engine's subclass, and restoring must not leave a copy behind.
        owned = attr in vars(owner)

        @functools.wraps(target)
        def wrapper(*args, _target=target, _stage=stage, _inside=inside, **kwargs):
            started = time.perf_counter()
            try:
                with _inside() if _inside is not None else nullcontext():
                    return _target(*args, **kwargs)
            finally:
                report(_stage, _elapsed_ms(started))

        # Re-wrapping matters: a bare function in a classmethod's place would bind `cls`
        # to the first real argument and the load would fail in a way that looks like the
        # engine's fault.
        if isinstance(raw, classmethod):
            setattr(owner, attr, classmethod(wrapper))
        elif isinstance(raw, staticmethod):
            setattr(owner, attr, staticmethod(wrapper))
        else:
            setattr(owner, attr, wrapper)
        undo.append((owner, attr, raw, owned))
    return undo


def _remove_load_probes(undo: list[tuple[object, str, object, bool]]) -> None:
    for owner, attr, raw, owned in reversed(undo):
        if owned:
            setattr(owner, attr, raw)
        else:
            # `to` is inherited from nn.Module. Assigning it back would leave a permanent
            # copy of the base implementation shadowing the MRO on the engine's class.
            delattr(owner, attr)


def _load_runtime() -> float:
    """Load the model exactly once. Returns the ms this call spent on it — 0.0 when the
    model was already there — and raises whatever the load raised.

    Single-flight: a second caller waits for the first one's outcome instead of starting
    its own load, which would double peak VRAM and throw one result away. /load and
    /synthesize share this, so a warm racing a speak still loads once.
    """
    # The engine is imported on a background thread (see `_import_engine`), so this is
    # where the caller finally pays for it if it has not finished. Outside `_COND`: a
    # thread waiting on the import must not hold the lock that /load's single-flight and
    # /health's state both depend on.
    _await_imports()
    with _COND:
        if STATE["runtime"] is not None:
            return 0.0
        if STATE["loading"]:
            waited = time.perf_counter()
            while STATE["loading"]:
                _COND.wait()
            if STATE["runtime"] is None:
                raise RuntimeError(STATE["error"] or "a concurrent model load failed")
            return _elapsed_ms(waited)
        STATE["loading"] = True
        STATE["error"] = None

    started = time.perf_counter()
    _stage("model.load.start", **PLACEMENT)

    def _load_step(step: str, ms: float) -> None:
        vram = _vram_mb()
        _stage(
            "model.load.step",
            step=step,
            ms=ms,
            vram_alloc_mb=None if vram is None else vram[0],
            vram_reserved_mb=None if vram is None else vram[1],
        )

    probes = _install_load_probes(_load_step)
    try:
        checkpoint = str(_checkpoint())
        runtime = InferenceRuntime.from_key(RuntimeKey(checkpoint=checkpoint, **PLACEMENT))
    except BaseException as exc:
        # Every exit from here must clear `loading` and wake the waiters, or a failed
        # load leaves every later request blocked on a load that will never finish.
        with _COND:
            STATE["loading"] = False
            STATE["error"] = _reason(exc)
            _COND.notify_all()
        _stage("model.load.failed", ms=_elapsed_ms(started), reason=_reason(exc))
        raise
    finally:
        _remove_load_probes(probes)
    with _COND:
        STATE["runtime"] = runtime
        STATE["loading"] = False
        _COND.notify_all()

    # The load leaves the caching allocator holding ~1.4 GiB that nothing owns: the
    # checkpoint lands on the device as fp32 (2929 MiB measured), `_move_inference_module`
    # casts it to bf16 (1495 MiB), and the freed fp32 blocks stay reserved for a process
    # that will never ask for a block that shape again. Coalescing them is what
    # expandable_segments is for, and PyTorch on Windows warns it is unsupported — so hand
    # them back explicitly instead, once, at the single moment the transient is provably
    # dead. Not per utterance: the blocks a synthesis leaves behind are the ones the next
    # synthesis reuses.
    if torch.cuda.is_available():
        trim = time.perf_counter()
        torch.cuda.empty_cache()
        _load_step("empty_cache", _elapsed_ms(trim))

    vram = _vram_mb()
    _stage(
        "model.load.done",
        ms=_elapsed_ms(started),
        vram_alloc_mb=None if vram is None else vram[0],
        vram_reserved_mb=None if vram is None else vram[1],
        checkpoint=checkpoint,
    )
    return _elapsed_ms(started)


def _sampling_request(body: SynthesizeBody) -> SamplingRequest:
    kwargs: dict = {}
    if body.voicePack is not None:
        pack = body.voicePack
        if pack.kind == "lora-adapter":
            kwargs = {"lora_adapter": pack.path, "no_ref": True}
        elif pack.kind == "speaker-embedding":
            kwargs = {"ref_embed": pack.path}
        elif pack.kind == "reference-audio":
            paths = pack.path if isinstance(pack.path, list) else [pack.path]
            kwargs = {"ref_wavs": paths}
        else:
            raise ValueError(f"unsupported voicePack kind: {pack.kind}")
    return SamplingRequest(text=body.text, seed=body.seed, num_steps=body.numSteps, **kwargs)


@app.get("/health")
def health() -> dict:
    return {"ready": True, "modelLoaded": STATE["runtime"] is not None}


@app.post("/load")
def load() -> dict:
    # Warm used to spawn the process and stop there, so the first utterance still paid
    # the model load; the runtime now calls this and waits (Worker::load_model).
    try:
        load_ms = _load_runtime()
    except Exception as exc:
        print(f"[worker] model load failed: {exc!r}", file=sys.stderr, flush=True)
        traceback.print_exc(file=sys.stderr)
        return {"error": f"model load failed: {_reason(exc)}"}
    return {"modelLoaded": True, "loadMs": int(round(load_ms))}


@app.post("/unload")
def unload() -> dict:
    """Hand the VRAM back without dying. Idle reclaim used to kill the process, which
    also threw away the torch import — the expensive half of a cold start."""
    started = time.perf_counter()
    with _COND:
        runtime = STATE["runtime"]
        STATE["runtime"] = None
        busy = STATE["busy"]
    if runtime is None:
        return {"modelLoaded": False, "freedMs": 0}

    before = _vram_mb()
    try:
        if busy == 0:
            # The engine's own release path (inference_runtime.py unload()): it drops
            # model/tokenizer/codec and empties the cache per device. It does that with
            # `del self.model`, so it must not run while a /synthesize is still holding
            # this object — the reference drop above already hid it from new callers,
            # and the last reference dies with that request.
            runtime.unload()
    except Exception as exc:
        print(f"[worker] unload failed: {exc!r}", file=sys.stderr, flush=True)
        traceback.print_exc(file=sys.stderr)
    del runtime
    # unload() does not touch the watermarker (a torch model on the codec device) or
    # the caption tokenizer, so the reference drop above is what frees those; gc runs
    # the reference cycles torch modules are full of, and only empty_cache() returns
    # the allocator's freed blocks to the driver.
    gc.collect()
    if torch.cuda.is_available():
        torch.cuda.empty_cache()
    after = _vram_mb()

    # stdout, like every other stage: the cold path has to be readable end to end in
    # tts-worker.out.log, and an unload is a normal lifecycle event, not an error.
    _stage(
        "model.unload.done",
        ms=_elapsed_ms(started),
        busy=busy,
        vram_alloc_before_mb=None if before is None else before[0],
        vram_alloc_after_mb=None if after is None else after[0],
        vram_reserved_before_mb=None if before is None else before[1],
        vram_reserved_after_mb=None if after is None else after[1],
    )
    return {"modelLoaded": False, "freedMs": int(round(_elapsed_ms(started)))}


@app.post("/synthesize")
def synthesize(body: SynthesizeBody) -> dict:
    # Without a handler here Starlette answers 500 with the literal body "Internal
    # Server Error" and the reason — a missing checkpoint, a pack with no reference
    # audio — never leaves this process. The reply's `error` field is the only channel
    # that reaches the caller; the traceback goes to stderr, which the runtime tees to
    # tts-worker.err.log.
    started = time.perf_counter()
    try:
        _load_runtime()
    except Exception as exc:
        print(f"[worker] model load failed: {exc!r}", file=sys.stderr, flush=True)
        traceback.print_exc(file=sys.stderr)
        return {"error": f"model load failed: {_reason(exc)}"}

    # Claiming the runtime and marking it busy in one critical section is what lets
    # /unload know it must leave the engine's own unload() alone.
    with _COND:
        runtime = STATE["runtime"]
        if runtime is not None:
            STATE["busy"] += 1
        alone = STATE["busy"] == 1
    if runtime is None:
        return {"error": "synthesis failed: the model was unloaded while this request started"}

    # The allocator's high-water marks are per process, so they only attribute to one
    # utterance when nothing else is in flight. The runtime serialises the GPU behind a
    # Semaphore(1) (src/service.rs), so in the product that is every request; a caller
    # that reaches this worker directly and overlaps two just gets no peak reported.
    if alone and torch.cuda.is_available():
        torch.cuda.reset_peak_memory_stats()

    try:
        result = runtime.synthesize(_sampling_request(body))
        out_path = Path(body.outPath)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        wav_started = time.perf_counter()
        frames = _write_wav(out_path, result.audio, result.sample_rate)
        duration_ms = int(round(frames * 1000 / result.sample_rate))

        peak = _vram_peak_mb() if alone else None
        fields: dict = {
            "ms": _elapsed_ms(started),
            "engine_ms": result.total_to_decode * 1000.0,
            "wav_ms": _elapsed_ms(wav_started),
            "audio_ms": duration_ms,
            "steps": body.numSteps,
            "chars": len(body.text),
            "seed": result.used_seed,
            "vram_peak_alloc_mb": None if peak is None else peak[0],
            "vram_peak_reserved_mb": None if peak is None else peak[1],
        }
        # The engine times its own stages (sample_rf is the sampler, decode_latent the
        # codec); setdefault so a stage it adds later cannot collide with a key above.
        for name, seconds in result.stage_timings:
            fields.setdefault(f"{name}_ms", seconds * 1000.0)
        _stage("synthesize.done", **fields)

        return {
            "sampleRate": int(result.sample_rate),
            "durationMs": duration_ms,
            "bytes": out_path.stat().st_size,
        }
    except Exception as exc:
        print(f"[worker] synthesis failed: {exc!r}", file=sys.stderr, flush=True)
        traceback.print_exc(file=sys.stderr)
        return {"error": f"synthesis failed: {_reason(exc)}"}
    finally:
        with _COND:
            STATE["busy"] -= 1
            _COND.notify_all()


def _write_wav(path: Path, audio: "torch.Tensor", sample_rate: int) -> int:
    """Write PCM_16 WAV directly to the runtime's spool path. Returns frames."""
    import soundfile as sf

    cpu = audio.detach().to("cpu", dtype=torch.float32)
    data = cpu.squeeze(0).numpy() if cpu.shape[0] == 1 else cpu.T.numpy()
    sf.write(str(path), data, sample_rate, format="WAV", subtype="PCM_16")
    return int(data.shape[0])


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--root", required=True, help="irodori-tts engine root")
    parser.add_argument(
        "--spawn-epoch-ms",
        help="the runtime's spawn instant in unix ms; only feeds boot.interpreter",
    )
    # Defaults are the shipped behaviour, byte for byte. The flags exist so the next
    # measurement wave can move the codec to CPU without inventing a config surface.
    parser.add_argument("--model-device", default=_DEFAULT_PLACEMENT["model_device"])
    parser.add_argument("--codec-device", default=_DEFAULT_PLACEMENT["codec_device"])
    parser.add_argument("--model-precision", default=_DEFAULT_PLACEMENT["model_precision"])
    parser.add_argument("--codec-precision", default=_DEFAULT_PLACEMENT["codec_precision"])
    args = parser.parse_args()  # --root/--port/--spawn-epoch-ms already read above

    PLACEMENT.update(
        model_device=args.model_device,
        codec_device=args.codec_device,
        model_precision=args.model_precision,
        codec_precision=args.codec_precision,
    )

    import uvicorn

    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")


if __name__ == "__main__":
    main()
