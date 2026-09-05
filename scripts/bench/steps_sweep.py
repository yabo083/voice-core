#!/usr/bin/env python3
"""Is 32 sampling steps necessary? Latency and quality for the same utterances.

`sample_rf` is ~90% of a warm synthesis and its cost is linear in `num_steps`, so fewer
steps is the one lever that shortens an utterance without touching the model. The question
is not whether it is faster - it is - but whether the voice survives, and that has to be
answered by the same measure the training harness accepts a voice with, not by ear alone.

One model load, then every (steps x seed x text) combination, written as
`steps<N>_s<SEED>_t<K>.wav`. Two independent readings follow:

  * `evaluate_similarity.py` (Resemblyzer GE2E, leave-one-out ceiling over the reference
    corpus) scores every clip. It groups by the `_t<K>` suffix, so each (steps, seed) pair
    becomes one group and the spread across seeds at a fixed step count IS the noise band
    that a step count has to beat to count as a real change.
  * Mel-dB RMSE and speaker cosine against the 32-step audio of the same (seed, text),
    which is the listenable difference rather than the corpus similarity: it says how far
    the output moved, not whether it still resembles the speaker.

    steps_sweep.py --lora <adapter dir> --ref-dir assets/training/audio --score

The GPU is single-tenant. This loads the model itself, so the runtime must be stopped or
asleep first.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parents[1]
_TRAINING = _SCRIPTS / "training" / "irodori"
sys.path.insert(0, str(_SCRIPTS / "training"))
sys.path.insert(0, str(_TRAINING))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402
import _layout  # noqa: E402
from evaluate_similarity import percentile  # noqa: E402
from generate_samples import load_texts  # noqa: E402
from synth_bench import audio_delta, speaker_encoder  # noqa: E402


def _clip_name(steps: int, seed: int, text_id: str) -> str:
    """`evaluate_similarity.py` strips a trailing `_t<N>`, so the group it reports is
    `steps<N>_s<SEED>` - one group per (steps, seed) pair, which is what makes the
    seed-to-seed spread readable as noise."""
    return f"steps{steps}_s{seed}_{text_id}.wav"


def _sweep(runtime, out_dir: Path, *, texts, steps_list, seeds, kwargs) -> list[dict]:
    from irodori_tts.inference_runtime import SamplingRequest, save_wav

    rows: list[dict] = []
    for steps in steps_list:
        for seed in seeds:
            for text_id, text in texts.items():
                request = SamplingRequest(text=text, num_steps=steps, seed=seed, **kwargs)
                clock = time.perf_counter()
                result = runtime.synthesize(request)
                elapsed = (time.perf_counter() - clock) * 1000.0
                path = out_dir / _clip_name(steps, seed, text_id)
                save_wav(str(path), result.audio, result.sample_rate)
                timings = dict(result.stage_timings)
                rows.append(
                    {
                        "steps": steps,
                        "seed": seed,
                        "textId": text_id,
                        "wav": str(path),
                        "synthMs": round(elapsed, 1),
                        "sampleRfMs": round(timings.get("sample_rf", 0.0) * 1000.0, 1),
                        "decodeLatentMs": round(timings.get("decode_latent", 0.0) * 1000.0, 1),
                        "audioMs": round(
                            result.audio.shape[-1] * 1000.0 / result.sample_rate, 1
                        ),
                    }
                )
                print(
                    f"  steps {steps:>2}  seed {seed:<10} {text_id}: {elapsed:7.0f} ms   "
                    f"sample_rf {rows[-1]['sampleRfMs']:7.0f} ms   "
                    f"audio {rows[-1]['audioMs']:6.0f} ms",
                    flush=True,
                )
    return rows


def _latency_table(rows: list[dict], steps_list: list[int]) -> None:
    print()
    print(f"{'steps':>5} {'n':>3} {'p50 synth':>10} {'p50 sample_rf':>14} {'vs 32 steps':>12}")
    base = None
    for steps in steps_list:
        group = [row for row in rows if row["steps"] == steps]
        if not group:
            continue
        synth = percentile([row["synthMs"] for row in group], 50)
        sampler = percentile([row["sampleRfMs"] for row in group], 50)
        if base is None:
            base = synth
        print(f"{steps:>5} {len(group):>3} {synth:>10.0f} {sampler:>14.0f} {synth / base:>11.2f}x")


def _score(out_dir: Path, ref_dir: Path, steps_list: list[int], seeds: list[int]) -> None:
    """Delegate to the harness that defines the measure, then aggregate its report.

    Scoring lives in exactly one place in this repository. This runs it once over every
    clip - one shared leave-one-out ceiling for all of them - and then reads the report
    back rather than recomputing anything.
    """
    label = "steps-sweep"
    command = [
        sys.executable,
        str(_TRAINING / "evaluate_similarity.py"),
        "--label",
        label,
        "--ref-dir",
        str(ref_dir),
        "--tests",
        str(out_dir / "*.wav"),
        "--out-dir",
        str(out_dir),
    ]
    print()
    print("scoring    " + " ".join(f'"{part}"' if " " in part else part for part in command))
    if subprocess.run(command).returncode != 0:
        raise SystemExit("evaluate_similarity.py failed; the quality half of this sweep is absent")

    report = json.loads((out_dir / f"{label}.json").read_text(encoding="utf-8"))
    ceiling = report.get("ceiling_loo", {})
    groups = {entry["group"]: entry for entry in report["groups"]}
    print()
    print(
        f"ceiling    LOO mean {ceiling.get('mean')}   p10 {ceiling.get('p10')}   "
        f"p90 {ceiling.get('p90')}   over {report['num_refs']} reference clips"
    )
    print()
    print(
        f"{'steps':>5} {'seeds':>5} {'lower bound: min':>17} {'median':>8} {'max':>8} "
        f"{'mean sim':>9}  verdict"
    )
    reference = None
    for steps in steps_list:
        bounds, means = [], []
        for seed in seeds:
            entry = groups.get(f"steps{steps}_s{seed}")
            if entry is None:
                continue
            bounds.append(entry["lower_bound"])
            means.append(entry["mean"])
        if not bounds:
            continue
        band = (min(bounds), percentile(bounds, 50), max(bounds))
        if reference is None:
            reference = band
            verdict = "reference"
        elif band[1] >= reference[0]:
            # Inside the seed-to-seed spread of the 32-step run: this step count did not
            # move the lower bound further than changing the seed already does.
            verdict = "within 32-step seed noise"
        else:
            verdict = f"BELOW 32-step noise floor ({reference[0]:.4f})"
        print(
            f"{steps:>5} {len(bounds):>5} {band[0]:>17.4f} {band[1]:>8.4f} {band[2]:>8.4f} "
            f"{sum(means) / len(means):>9.4f}  {verdict}"
        )


def _drift(rows: list[dict], out_dir: Path, steps_list: list[int]) -> None:
    """How far each step count moved the audio from the 32-step audio of the same request."""
    if 32 not in steps_list:
        return
    encoder = speaker_encoder()
    if encoder is None:
        print("note       resemblyzer is absent; drift reported without a speaker cosine")
    print()
    print(f"{'steps':>5} {'clips':>5} {'mel dB RMSE p50':>16} {'cosine vs 32 p50':>17}")
    for steps in steps_list:
        if steps == 32:
            continue
        rmses, cosines = [], []
        for row in (row for row in rows if row["steps"] == steps):
            prior = out_dir / _clip_name(32, row["seed"], row["textId"])
            if not prior.is_file():
                continue
            delta = audio_delta(Path(row["wav"]), prior, encoder=encoder)
            rmses.append(delta["melDbRmse"])
            if delta["cosine"] is not None:
                cosines.append(delta["cosine"])
        if not rmses:
            continue
        cosine = f"{percentile(cosines, 50):.4f}" if cosines else "-"
        print(f"{steps:>5} {len(rmses):>5} {percentile(rmses, 50):>16.4f} {cosine:>17}")


def main() -> None:
    _layout.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Sweep num_steps for latency and speaker similarity over a fixed text set."
    )
    parser.add_argument("--lora", type=Path, default=None, help="Adapter directory to voice with.")
    parser.add_argument("--no-ref", action="store_true", help="Base voice, no adapter.")
    parser.add_argument(
        "--steps", type=int, nargs="+", default=[32, 24, 20, 16], help="Step counts to compare."
    )
    parser.add_argument(
        "--seeds",
        type=int,
        nargs="+",
        default=[1234, 20260905, 777],
        help="One group per seed per step count; their spread is the noise band.",
    )
    parser.add_argument("--texts-file", type=Path, default=None, help="One text per line.")
    parser.add_argument("--out-dir", type=Path, default=None, help="Default: <data>/bench/steps.")
    parser.add_argument("--data-dir", type=Path, default=None)
    parser.add_argument("--device", default="cuda")
    parser.add_argument("--precision", default="bf16", choices=["bf16", "fp32"])
    parser.add_argument(
        "--score", action="store_true", help="Also run evaluate_similarity.py over the results."
    )
    parser.add_argument(
        "--ref-dir", type=Path, default=None, help="Reference corpus, required by --score."
    )
    _engine.add_engine_args(parser)
    args = parser.parse_args()

    if (args.lora is None) == (not args.no_ref):
        raise SystemExit("pass exactly one of --lora <adapter dir> or --no-ref")
    if args.score and args.ref_dir is None:
        raise SystemExit("--score needs --ref-dir: a similarity number without a corpus is noise")

    engine = _engine.resolve_engine(args)
    engine.require_own_interpreter()
    checkpoint = engine.checkpoint()
    texts = load_texts(args.texts_file)
    out_dir = args.out_dir or (_layout.resolve_data_dir(args.data_dir) / "bench" / "steps")
    out_dir.mkdir(parents=True, exist_ok=True)
    kwargs: dict = {"no_ref": True}
    if args.lora is not None:
        kwargs["lora_adapter"] = str(args.lora.expanduser().resolve())

    engine.activate()
    from irodori_tts.inference_runtime import InferenceRuntime, RuntimeKey

    print(f"checkpoint {checkpoint}")
    print(f"out        {out_dir}")
    print(f"steps      {args.steps}   seeds {args.seeds}   texts {len(texts)}")
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
    print(f"runtime    loaded in {time.perf_counter() - started:.1f}s")

    rows = _sweep(
        runtime, out_dir, texts=texts, steps_list=args.steps, seeds=args.seeds, kwargs=kwargs
    )
    (out_dir / "sweep.jsonl").write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8"
    )
    _latency_table(rows, args.steps)
    _drift(rows, out_dir, args.steps)
    if args.score:
        _score(out_dir, args.ref_dir.expanduser(), args.steps, args.seeds)
    print()
    print(f"saved      {out_dir / 'sweep.jsonl'}")
    print(f"elapsed    {time.perf_counter() - started:.1f}s")


if __name__ == "__main__":
    main()
