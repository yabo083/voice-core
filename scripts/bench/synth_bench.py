#!/usr/bin/env python3
"""What one utterance costs, measured against the running runtime.

Nothing here reimplements the product's timing. The runtime already reports `queueMs`,
`synthMs` and `totalMs` per speak, and the worker already prints one `[worker] stage=...`
line per cold-path event with the engine's own stage timings on it
(`worker/irodori/worker.py`). This drives `POST /api/speak` over a fixed text set at a
fixed seed, reads both sources back, and reduces them to numbers that can be compared
across a change:

    synth_bench.py --label baseline --runs 5

    cold start (this window)                      ms
      boot.interpreter                          108.3
      ...
      model.load / ckpt_read                   8123.4
    case          n    p50      p95     sample_rf  decode  ...
    t1            5   1712.0   1804.1      1301.2   118.4
    ...

`--runs` is the number of samples per text, not per run: five samples of three texts is
fifteen speaks. p50 is the number to quote and p95 is the tail; both come from the same
nearest-rank percentile the similarity harness uses, so "p95" means one thing in this
repository.

Every row also lands in `bench.jsonl` (one JSON object per speak, every field present),
and every utterance's WAV is kept. `--compare <dir>` then diffs this run's audio against
another run's, bitwise first: a scheduling or allocator change that alters one sample is
not a scheduling change. Only when the bytes differ does it fall back to arguing from
mel RMSE and speaker-embedding cosine.

The GPU is single-tenant (`Semaphore(1)` in src/service.rs, single-flight in the worker),
so this must be the only thing asking for it. `--sleep-first` calls `POST /api/sleep` to
drop the model before measuring, which is how a load is measured without killing the
process and re-paying the torch import.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(_SCRIPTS / "training"))
sys.path.insert(0, str(_SCRIPTS / "training" / "irodori"))

import _layout  # noqa: E402
from evaluate_similarity import percentile  # noqa: E402
from generate_samples import DEFAULT_TEXTS  # noqa: E402

# The cold path, in the order the worker prints it. `model.load.step` is not here because
# it repeats: one line per sub-stage, each named by its own `step=`.
BOOT_STAGES = (
    "boot.interpreter",
    "boot.config",
    "boot.imports",
    "boot.device",
    "boot.listening",
)

# Engine stage timings, in the order they happen inside one synthesis. Anything the engine
# starts reporting later still shows up - it is appended, not dropped.
ENGINE_STAGES = (
    "prepare_lora_ms",
    "tokenize_text_ms",
    "prepare_reference_ms",
    "predict_duration_ms",
    "sample_rf_ms",
    "unpatchify_latent_ms",
    "decode_latent_ms",
    "silentcipher_watermark_ms",
    "wav_ms",
)

_STAGE_LINE = re.compile(r"^\[worker\] stage=(\S+)\s*(.*)$")
_FIELD = re.compile(r'(\w+)=("[^"]*"|\S+)')


def _coerce(raw: str) -> object:
    """The worker's own rendering, read back: quoted strings, true/false, ints, floats."""
    if raw.startswith('"') and raw.endswith('"'):
        return raw[1:-1]
    if raw == "true":
        return True
    if raw == "false":
        return False
    for cast in (int, float):
        try:
            return cast(raw)
        except ValueError:
            continue
    return raw


class WorkerLog:
    """The worker's stage lines as they are appended.

    The runtime tees the engine's stdout into `logs/tts-worker.out.log`
    (src/supervise.rs), which is the only place the cold path is itemised. Reading by byte
    offset means a run only ever sees its own lines, and a partial trailing line is left
    for the next drain rather than parsed half-written.
    """

    def __init__(self, path: Path) -> None:
        self.path = path
        self.offset = path.stat().st_size if path.is_file() else 0
        self.events: list[tuple[str, dict]] = []

    def drain(self) -> list[tuple[str, dict]]:
        if not self.path.is_file():
            return []
        size = self.path.stat().st_size
        if size < self.offset:
            # The runtime rotated or truncated it; start from what is there now.
            self.offset = 0
        with self.path.open("rb") as handle:
            handle.seek(self.offset)
            raw = handle.read()
        cut = raw.rfind(b"\n")
        if cut < 0:
            return []
        self.offset += cut + 1
        fresh: list[tuple[str, dict]] = []
        for line in raw[: cut + 1].decode("utf-8", "replace").splitlines():
            match = _STAGE_LINE.match(line.strip())
            if match is None:
                continue
            fields = {key: _coerce(value) for key, value in _FIELD.findall(match.group(2))}
            fresh.append((match.group(1), fields))
        self.events.extend(fresh)
        return fresh

    def await_stage(self, stage: str, *, timeout: float = 5.0) -> dict | None:
        """The next unconsumed line for `stage`, waiting for the tee to catch up.

        The worker prints before it answers, but the runtime copies the pipe on its own
        task, so the line can trail the HTTP response by a few milliseconds.
        """
        deadline = time.monotonic() + timeout
        while True:
            for index, (name, fields) in enumerate(self.events):
                if name == stage:
                    del self.events[index]
                    return fields
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.05)
            self.drain()


def _request(url: str, token: str, *, payload: object = None, timeout: float) -> object:
    body = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(url, data=body, method="POST" if body else "GET")
    request.add_header("Authorization", f"Bearer {token}")
    if body is not None:
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read() or b"null")
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", "replace")
        raise SystemExit(f"{url} -> HTTP {exc.code}: {detail.strip()}") from exc
    except urllib.error.URLError as exc:
        raise SystemExit(
            f"{url} is not answering: {exc.reason}\n"
            "  Start the runtime first (bin/voice-core-runtime.exe or the VoiceCore tray)."
        ) from exc


def _post(url: str, token: str, *, payload: object = None, timeout: float = 900.0) -> object:
    return _request(url, token, payload=payload if payload is not None else {}, timeout=timeout)


def _get(url: str, token: str, *, timeout: float = 30.0) -> object:
    return _request(url, token, timeout=timeout)


def _audio(base_url: str, token: str, audio_id: str, out: Path) -> int:
    request = urllib.request.Request(f"{base_url}/api/audio/{audio_id}")
    request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=120.0) as response:
        data = response.read()
    out.write_bytes(data)
    return len(data)


def _table(rows: list[tuple[str, ...]], headers: tuple[str, ...]) -> str:
    """One fixed-width table. First column left, the rest right, so numbers line up."""
    widths = [len(head) for head in headers]
    for row in rows:
        for index, cell in enumerate(row):
            widths[index] = max(widths[index], len(cell))
    lines = ["  ".join(head.ljust(widths[0]) if i == 0 else head.rjust(widths[i])
                       for i, head in enumerate(headers))]
    for row in rows:
        lines.append("  ".join(cell.ljust(widths[0]) if i == 0 else cell.rjust(widths[i])
                               for i, cell in enumerate(row)))
    return "\n".join(lines)


def _load_texts(path: Path | None) -> dict[str, str]:
    """The same three shapes the training harness generates samples from, so a latency
    number and a similarity number are about the same utterances."""
    if path is None:
        return dict(DEFAULT_TEXTS)
    kept = [line.strip() for line in path.read_text(encoding="utf-8").splitlines()]
    kept = [line for line in kept if line]
    if not kept:
        raise SystemExit(f"{path}: no non-empty lines")
    return {f"t{index}": text for index, text in enumerate(kept, start=1)}


def _speak(
    base_url: str,
    token: str,
    log: WorkerLog,
    *,
    text: str,
    voice_pack: str | None,
    seed: int,
    steps: int,
    timeout_ms: int,
) -> dict:
    payload: dict = {"text": text, "seed": seed, "numSteps": steps, "timeoutMs": timeout_ms}
    if voice_pack is not None:
        payload["voicePackId"] = voice_pack
    wall = time.perf_counter()
    reply = _post(f"{base_url}/api/speak", token, payload=payload, timeout=timeout_ms / 1000.0 + 60)
    assert isinstance(reply, dict)
    reply["wallMs"] = round((time.perf_counter() - wall) * 1000.0, 1)
    log.drain()
    reply["worker"] = log.await_stage("synthesize.done") or {}
    return reply


def _cold_report(log: WorkerLog) -> list[tuple[str, ...]]:
    """Whatever cold-path lines this window caught, in the order they were printed.

    A run against an already-warm worker catches none of them and prints nothing, which is
    the honest answer: there was no cold start to break down.
    """
    rows: list[tuple[str, ...]] = []
    for name, fields in log.events:
        if name in BOOT_STAGES:
            rows.append((name, f"{float(fields.get('ms', 0.0)):.1f}", "", ""))
        elif name == "model.load.step":
            rows.append(
                (
                    f"model.load / {fields.get('step', '?')}",
                    f"{float(fields.get('ms', 0.0)):.1f}",
                    f"{float(fields.get('vram_alloc_mb', 0.0)):.0f}",
                    f"{float(fields.get('vram_reserved_mb', 0.0)):.0f}",
                )
            )
        elif name in ("model.load.done", "model.unload.done"):
            rows.append(
                (
                    name,
                    f"{float(fields.get('ms', 0.0)):.1f}",
                    f"{float(fields.get('vram_alloc_mb', fields.get('vram_alloc_after_mb', 0.0))):.0f}",
                    f"{float(fields.get('vram_reserved_mb', fields.get('vram_reserved_after_mb', 0.0))):.0f}",
                )
            )
    return rows


def _summary(samples: list[dict], key: str) -> dict:
    values = [float(row[key]) for row in samples if row.get(key) is not None]
    if not values:
        return {"n": 0, "p50": None, "p95": None, "min": None, "max": None}
    return {
        "n": len(values),
        "p50": percentile(values, 50),
        "p95": percentile(values, 95),
        "min": round(min(values), 1),
        "max": round(max(values), 1),
    }


def speaker_encoder():
    """Resemblyzer's GE2E encoder on CPU, or None when it is not installed.

    The same encoder the training harness scores with (`evaluate_similarity.py`), for the
    same reason: it has no relationship to the generator, so it measures similarity rather
    than agreement. It is an opt-in install, so its absence must degrade a report, not end
    one.
    """
    try:
        from resemblyzer import VoiceEncoder
    except ImportError:
        return None
    return VoiceEncoder("cpu")


def audio_delta(new: Path, old: Path, *, encoder=None) -> dict:
    """How far one clip moved from another. Bitwise first, and only then from numbers.

    A pure scheduling or allocator change must not move a single sample: same seed, same
    text, same GPU is bitwise reproducible in this engine (inference_runtime.py:1205,
    rf.py:228-232, codec.py:84-95). Bytes that do differ mean the change perturbed a
    floating-point reduction, and then the question is how much: mel RMSE in dB is what a
    listener would hear, and the speaker cosine is whether it is still the same voice.
    """
    import librosa
    import numpy
    import soundfile

    if new.read_bytes() == old.read_bytes():
        return {"identical": True, "maxSampleDelta": 0.0, "melDbRmse": 0.0, "cosine": None}

    fresh, rate = soundfile.read(str(new), dtype="float32", always_2d=False)
    prior, _ = soundfile.read(str(old), dtype="float32", always_2d=False)
    span = min(len(fresh), len(prior))
    mel_new = librosa.power_to_db(librosa.feature.melspectrogram(y=fresh, sr=rate))
    mel_old = librosa.power_to_db(librosa.feature.melspectrogram(y=prior, sr=rate))
    frames = min(mel_new.shape[1], mel_old.shape[1])
    cosine = None
    if encoder is not None:
        left = encoder.embed_utterance(librosa.resample(fresh, orig_sr=rate, target_sr=16000))
        right = encoder.embed_utterance(librosa.resample(prior, orig_sr=rate, target_sr=16000))
        cosine = round(float(left @ right), 4)
    return {
        "identical": False,
        "maxSampleDelta": float(numpy.abs(fresh[:span] - prior[:span]).max()),
        "melDbRmse": round(
            float(numpy.sqrt(numpy.mean((mel_new[:, :frames] - mel_old[:, :frames]) ** 2))), 4
        ),
        "cosine": cosine,
    }


def _compare(out_dir: Path, other: Path) -> None:
    """This run's audio against an earlier run's, clip by matching name."""
    mine = sorted(path for path in out_dir.glob("*.wav"))
    if not mine:
        raise SystemExit(f"no WAVs in {out_dir}")
    pairs = [(path, other / path.name) for path in mine]
    missing = [path.name for path, prior in pairs if not prior.is_file()]
    pairs = [(path, prior) for path, prior in pairs if prior.is_file()]
    if not pairs:
        raise SystemExit(f"none of {len(mine)} clip names exist in {other}")

    identical = all(path.read_bytes() == prior.read_bytes() for path, prior in pairs)
    encoder = None if identical else speaker_encoder()
    if not identical and encoder is None:
        print("note       resemblyzer is absent; reporting mel RMSE without a cosine")

    rows: list[tuple[str, ...]] = []
    for path, prior in pairs:
        delta = audio_delta(path, prior, encoder=encoder)
        rows.append(
            (
                path.name,
                "identical" if delta["identical"] else f"differs (dsamp {delta['maxSampleDelta']:.2e})",
                f"{delta['melDbRmse']:.4f}",
                "" if delta["cosine"] is None else f"{delta['cosine']:.4f}",
            )
        )
    for name in missing:
        rows.append((name, "not in the other run", "", ""))

    print()
    print(f"compare    {out_dir}  vs  {other}")
    print(_table(rows, ("clip", "bytes", "mel dB RMSE", "cosine")))


def main() -> None:
    _layout.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Per-utterance latency, engine stage timings and VRAM, from the running runtime."
    )
    parser.add_argument("--label", required=True, help="Names the output directory and the report.")
    parser.add_argument("--base-url", default="http://127.0.0.1:8760")
    parser.add_argument("--data-dir", type=Path, default=None, help="Where token.txt and logs are.")
    parser.add_argument("--out-dir", type=Path, default=None, help="Default: <data>/bench/<label>.")
    parser.add_argument("--texts-file", type=Path, default=None, help="One text per line.")
    parser.add_argument("--voice-pack", default=None, help="Default: the first installed pack.")
    parser.add_argument("--runs", type=int, default=5, help="Samples per text (default 5).")
    parser.add_argument("--warmup", type=int, default=1, help="Speaks excluded from the stats.")
    parser.add_argument("--seed", type=int, default=1234)
    parser.add_argument("--steps", type=int, default=32)
    parser.add_argument("--timeout-ms", type=int, default=600_000)
    parser.add_argument(
        "--sleep-first",
        action="store_true",
        help="POST /api/sleep first, so the next speak pays a model load and it is measured.",
    )
    parser.add_argument(
        "--compare",
        type=Path,
        default=None,
        help="An earlier run's directory. Diffs this run's audio against it, bitwise first.",
    )
    args = parser.parse_args()

    data_dir = _layout.resolve_data_dir(args.data_dir)
    token_file = data_dir / "token.txt"
    if not token_file.is_file():
        raise SystemExit(f"no token at {token_file}; start the runtime once so it writes one")
    token = token_file.read_text(encoding="utf-8").strip()
    base_url = args.base_url.rstrip("/")
    out_dir = args.out_dir or (data_dir / "bench" / args.label)
    out_dir.mkdir(parents=True, exist_ok=True)
    texts = _load_texts(args.texts_file)

    status = _get(f"{base_url}/api/status", token)
    assert isinstance(status, dict)
    voice_pack = args.voice_pack
    if voice_pack is None:
        voices = _get(f"{base_url}/api/voices", token)
        assert isinstance(voices, list)
        voice_pack = voices[0]["id"] if voices else None

    print(f"runtime    {base_url}   {status.get('runtimeVersion')}   packs {status.get('voicePacks')}")
    print(f"data       {data_dir}")
    print(f"pack       {voice_pack}   seed {args.seed}   steps {args.steps}")
    print(f"texts      {len(texts)}   runs {args.runs}   warmup {args.warmup}")
    if args.runs < 5:
        print("note       fewer than 5 samples per text: p95 is a single observation, not a tail")

    log = WorkerLog(data_dir / "logs" / "tts-worker.out.log")
    if args.sleep_first:
        reply = _post(f"{base_url}/api/sleep", token)
        print(f"sleep      {json.dumps(reply, ensure_ascii=False)}")

    rows: list[dict] = []
    plan = [("warmup", index) for index in range(args.warmup)]
    plan += [("measure", index) for index in range(args.runs)]
    for phase, run in plan:
        for text_id, text in texts.items():
            reply = _speak(
                base_url,
                token,
                log,
                text=text,
                voice_pack=voice_pack,
                seed=args.seed,
                steps=args.steps,
                timeout_ms=args.timeout_ms,
            )
            worker = reply.pop("worker", {})
            wav = out_dir / f"{text_id}_r{run}.wav" if phase == "measure" else None
            if wav is not None:
                _audio(base_url, token, reply["audioId"], wav)
            row = {
                "label": args.label,
                "phase": phase,
                "run": run,
                "textId": text_id,
                "chars": len(text),
                "steps": args.steps,
                "seed": args.seed,
                "voicePackId": reply.get("voicePackId"),
                "coldStart": reply.get("coldStart"),
                "queueMs": reply.get("queueMs"),
                "synthMs": reply.get("synthMs"),
                "totalMs": reply.get("totalMs"),
                "wallMs": reply.get("wallMs"),
                "durationMs": reply.get("durationMs"),
                "wav": None if wav is None else str(wav),
            }
            for stage in ENGINE_STAGES:
                row[stage] = worker.get(stage)
            for stage in sorted(key for key in worker if key.endswith("_ms")):
                row.setdefault(stage, worker[stage])
            row["vramPeakAllocMb"] = worker.get("vram_peak_alloc_mb")
            row["vramPeakReservedMb"] = worker.get("vram_peak_reserved_mb")
            row["engineMs"] = worker.get("engine_ms")
            rows.append(row)
            print(
                f"  {phase[:1]}{run} {text_id}: total {row['totalMs']} ms   "
                f"synth {row['synthMs']} ms   sample_rf {row.get('sample_rf_ms')} ms   "
                f"audio {row['durationMs']} ms",
                flush=True,
            )

    log.drain()
    measured = [row for row in rows if row["phase"] == "measure"]
    jsonl = out_dir / "bench.jsonl"
    with jsonl.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")

    cold = _cold_report(log)
    if cold:
        print()
        print("cold path (only what this window caught)")
        print(_table(cold, ("stage", "ms", "alloc MiB", "reserved MiB")))

    print()
    stage_keys = [key for key in ENGINE_STAGES if any(row.get(key) is not None for row in measured)]
    headers = ("case", "n", "p50 total", "p95 total", "p50 synth", "p50 queue", "audio ms")
    headers += tuple(key[:-3] for key in stage_keys)
    table: list[tuple[str, ...]] = []
    for text_id in list(texts) + ["ALL"]:
        samples = measured if text_id == "ALL" else [r for r in measured if r["textId"] == text_id]
        if not samples:
            continue
        total = _summary(samples, "totalMs")
        cells = [
            text_id,
            str(total["n"]),
            f"{total['p50']:.0f}",
            f"{total['p95']:.0f}",
            f"{_summary(samples, 'synthMs')['p50']:.0f}",
            f"{_summary(samples, 'queueMs')['p50']:.0f}",
            f"{_summary(samples, 'durationMs')['p50']:.0f}",
        ]
        cells += [f"{_summary(samples, key)['p50']:.0f}" for key in stage_keys]
        table.append(tuple(cells))
    print(_table(table, headers))

    peaks = [row["vramPeakReservedMb"] for row in measured if row["vramPeakReservedMb"] is not None]
    allocs = [row["vramPeakAllocMb"] for row in measured if row["vramPeakAllocMb"] is not None]
    if peaks:
        print()
        print(
            f"vram peak  allocated p50 {percentile(allocs, 50):.0f} MiB   "
            f"max {max(allocs):.0f} MiB   |   reserved p50 {percentile(peaks, 50):.0f} MiB   "
            f"max {max(peaks):.0f} MiB"
        )

    summary = {
        "label": args.label,
        "cases": {
            text_id: {
                metric: _summary([r for r in measured if r["textId"] == text_id], metric)
                for metric in ("totalMs", "synthMs", "queueMs", *stage_keys)
            }
            for text_id in texts
        },
    }
    (out_dir / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print()
    print(f"saved      {jsonl}")
    print(f"saved      {out_dir / 'summary.json'}")

    if args.compare is not None:
        _compare(out_dir, args.compare.expanduser())


if __name__ == "__main__":
    main()
