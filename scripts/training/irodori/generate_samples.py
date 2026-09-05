#!/usr/bin/env python3
"""Generate the comparison samples that checkpoint selection is decided on - step 4.

One runtime load, then the same texts under every condition you ask for, with one fixed
seed. That is the whole point: if the text and the seed are held still, the difference
between two WAVs is the difference between two checkpoints, and nothing else.

    # sweep every checkpoint a LoRA run produced
    generate_samples.py --lora outputs/my-voice-lora --out-dir eval/gen/my-voice

    # a speaker embedding, plus the cross-speaker floor to calibrate against
    generate_samples.py --speaker-embedding out/checkpoint_final.speaker.safetensors \\
                        --no-ref --out-dir eval/gen/my-voice-se

Conditions, and what each one exercises:

  --lora DIR              the adapter carries the voice, generated with NO reference
                          audio (`no_ref`), which is exactly how the runtime uses a
                          lora-adapter pack. Point it at one adapter directory, or at the
                          run's output directory to sweep every checkpoint under it.
  --speaker-embedding F   learned speaker tokens (`ref_embed`). Mutually exclusive with
                          reference audio and with no-ref, per the engine.
  --reference-audio A B   zero-training cloning from clips (`ref_wavs`), concatenated in
                          the order given. Useful as a baseline even when you are training
                          something: it is the quality a pack costs nothing to reach.
  --no-ref                no speaker conditioning at all. This is the cross-speaker FLOOR
                          for the similarity harness - without it you have no idea whether
                          0.7 is good.

This is the one step that imports the engine instead of shelling out to upstream, and the
reason is the sweep: `infer.py` takes one `--text` and one `--output-wav` per invocation,
so five checkpoints times three texts would be fifteen cold model loads. Loading the
runtime once and holding it is the entire point.

Needs the engine's Python, the weights, and a GPU.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

# This step's name in the progress protocol (`scripts/training/_layout.py`).
STAGE = "samples"

# Neutral Japanese, because this backend's text encoder is Japanese. Three shapes -
# greeting, short declarative, longer polite - so a checkpoint that only sounds right on
# one sentence length is visible. Override with --texts-file.
DEFAULT_TEXTS = {
    "t1": "こんにちは。今日はいい天気ですね。",
    "t2": "了解しました。すぐに準備します。",
    "t3": "少し待っていてくださいね。今、調べてみます。",
}


def load_texts(path: Path | None) -> dict[str, str]:
    if path is None:
        return dict(DEFAULT_TEXTS)
    lines = [line.strip() for line in path.read_text(encoding="utf-8").splitlines()]
    kept = [line for line in lines if line]
    if not kept:
        raise SystemExit(f"{path}: no non-empty lines")
    return {f"t{index}": text for index, text in enumerate(kept, start=1)}


def lora_conditions(root: Path) -> list[tuple[str, dict]]:
    """One condition per adapter directory: `root` itself if it is one, otherwise every
    child that is. An adapter directory is `adapter_config.json` plus an
    `adapter_model.safetensors` (`irodori_tts/lora.py:263-269`)."""

    def is_adapter(path: Path) -> bool:
        return path.is_dir() and (path / "adapter_config.json").is_file()

    if is_adapter(root):
        return [(f"lora_{root.name}", {"lora_adapter": str(root), "no_ref": True})]
    children = sorted(child for child in root.iterdir() if is_adapter(child))
    if not children:
        raise SystemExit(
            f"no LoRA adapter directories under {root}\n"
            "  Expected either an adapter directory (adapter_config.json +\n"
            "  adapter_model.safetensors) or a training output directory containing them."
        )
    return [(f"lora_{child.name}", {"lora_adapter": str(child), "no_ref": True}) for child in children]


def build_conditions(args: argparse.Namespace) -> list[tuple[str, dict]]:
    conditions: list[tuple[str, dict]] = []
    if args.lora is not None:
        conditions.extend(lora_conditions(args.lora.expanduser().resolve()))
    if args.speaker_embedding is not None:
        path = args.speaker_embedding.expanduser().resolve()
        if not path.name.endswith(".speaker.safetensors"):
            raise SystemExit(
                f"{path}\n"
                "  The engine requires this suffix by name and will refuse the file:\n"
                "  \"Speaker Inversion embeddings must use the '.speaker.safetensors' suffix\""
            )
        conditions.append((f"se_{path.name.split('.speaker')[0]}", {"ref_embed": str(path)}))
    if args.reference_audio:
        paths = [str(Path(p).expanduser().resolve()) for p in args.reference_audio]
        conditions.append((f"ref{len(paths)}", {"ref_wavs": paths}))
    if args.no_ref:
        conditions.append(("noref", {"no_ref": True}))
    if not conditions:
        raise SystemExit(
            "nothing to generate. Pass at least one of --lora, --speaker-embedding, "
            "--reference-audio, --no-ref."
        )
    return conditions


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Generate fixed-seed comparison samples from Irodori voice artefacts."
    )
    parser.add_argument("--out-dir", type=Path, required=True, help="Where WAVs are written.")
    parser.add_argument(
        "--lora", type=Path, default=None, help="Adapter directory, or a directory of them."
    )
    parser.add_argument(
        "--speaker-embedding", type=Path, default=None, help="A .speaker.safetensors file."
    )
    parser.add_argument(
        "--reference-audio", nargs="+", default=[], help="Reference clips, concatenated in order."
    )
    parser.add_argument(
        "--no-ref", action="store_true", help="Also generate the unconditioned floor."
    )
    parser.add_argument(
        "--texts-file", type=Path, default=None, help="One text per line. Default: three neutral Japanese lines."
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=1234,
        help="Fixed seed (default 1234). The comparison is only meaningful because this is held still.",
    )
    parser.add_argument("--steps", type=int, default=32, help="Sampling steps (default 32).")
    parser.add_argument("--device", default="cuda", help="Model and codec device (default cuda).")
    parser.add_argument(
        "--precision", default="bf16", choices=["bf16", "fp32"], help="Model and codec precision."
    )
    _engine.add_json_flag(parser)
    _engine.add_engine_args(parser)
    args = parser.parse_args()
    if args.json:
        _engine.json_mode()
    _engine.guard(STAGE, lambda: generate(args))


def generate(args: argparse.Namespace) -> None:
    engine = _engine.resolve_engine(args)
    engine.require_own_interpreter()
    checkpoint = engine.checkpoint()
    conditions = build_conditions(args)
    texts = load_texts(args.texts_file)
    engine.activate()

    from irodori_tts.inference_runtime import (  # noqa: E402
        InferenceRuntime,
        RuntimeKey,
        SamplingRequest,
        save_wav,
    )

    out_dir = args.out_dir.expanduser()
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"checkpoint {checkpoint}")
    print(f"conditions {', '.join(label for label, _ in conditions)}")
    print(f"texts      {len(texts)}   seed {args.seed}   steps {args.steps}")
    total = len(conditions) * len(texts)
    _engine.emit(
        STAGE,
        "start",
        f"{total} clip(s): {len(conditions)} condition(s) x {len(texts)} text(s), "
        f"seed {args.seed}, {args.steps} steps",
    )
    # Worth its own line: loading the model is 14-23 s of a step whose first clip then
    # takes two, and silence for twenty seconds reads as a hang.
    _engine.emit(STAGE, "log", f"loading {checkpoint.name} onto {args.device}")

    started = time.perf_counter()
    runtime = InferenceRuntime.from_key(
        RuntimeKey(
            checkpoint=str(checkpoint),
            model_device=args.device,
            codec_device=args.device,
            model_precision=args.precision,
            codec_precision=args.precision,
        )
    )
    loaded = time.perf_counter() - started
    print(f"runtime    loaded in {loaded:.1f}s")
    _engine.emit(STAGE, "log", f"model loaded in {loaded:.1f}s")

    manifest = {
        "checkpoint": str(checkpoint),
        "seed": args.seed,
        "steps": args.steps,
        "texts": texts,
        "items": [],
    }
    done = 0
    for label, kwargs in conditions:
        for text_id, text in texts.items():
            request = SamplingRequest(
                text=text, num_steps=args.steps, seed=args.seed, **kwargs
            )
            clock = time.perf_counter()
            result = runtime.synthesize(request)
            path = out_dir / f"{label}_{text_id}.wav"
            save_wav(str(path), result.audio, result.sample_rate)
            elapsed = time.perf_counter() - clock
            manifest["items"].append(
                {
                    "label": label,
                    "text_id": text_id,
                    "wav": str(path),
                    "condition": kwargs,
                    "gen_seconds": round(elapsed, 2),
                    "sample_rate": int(result.sample_rate),
                }
            )
            print(f"  {label}/{text_id}: {elapsed:.1f}s -> {path.name}", flush=True)
            done += 1
            _engine.emit(
                STAGE,
                "progress",
                f"{label}/{text_id} in {elapsed:.1f}s",
                done=done,
                total=total,
            )

    out = out_dir / "manifest.json"
    out.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"manifest   {out}")
    print(f"elapsed    {time.perf_counter() - started:.1f}s")
    _engine.emit(
        STAGE,
        "ok",
        f"{done} clip(s) from {len(conditions)} condition(s) in "
        f"{time.perf_counter() - started:.1f}s -> {out_dir}",
        done=done,
        total=total,
    )


if __name__ == "__main__":
    main()
