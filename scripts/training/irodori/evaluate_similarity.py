#!/usr/bin/env python3
"""Score generated audio against the reference corpus - step 5, and the accept/reject gate.

The method, unchanged from the run that produced the numbers this project quotes:

  * A speaker-verification model with NO relationship to the generator (Resemblyzer's
    GE2E encoder, 256-d d-vectors at 16 kHz). Scoring a TTS model with its own encoder
    measures agreement, not similarity.
  * Cosine similarity of every generated clip against every reference clip. Resemblyzer's
    embeddings are L2-normalised, so the dot product IS the cosine.
  * Leave-one-out over the reference corpus as the ceiling: each reference clip against
    all the others. That is what "the same human twice" scores, and it is the only honest
    upper bound - a generated clip is not supposed to beat it.
  * Judgement by the LOWER bound of the distribution, not the mean. A voice that averages
    0.80 and dips to 0.65 is a voice that audibly drifts every few utterances; the mean
    hides exactly the failure a listener notices.

The reference run's ceiling, for calibration: LOO mean 0.771, p10 0.703 over an 80-clip
corpus. A batch whose minimum was 0.651 had already fallen through that p10, and it was
audible. Your corpus has its own ceiling - this script computes it - so compare against
yours, not against those two numbers.

    evaluate_similarity.py --label lora-sweep --ref-dir corpus/wav \\
                           --tests "eval/gen/my-voice/*.wav" --out-dir eval/results

Clips are grouped by the `<label>_t<N>.wav` names generate_samples.py writes, so a sweep
over checkpoints prints one row per checkpoint, sorted by the number selection should use.

Needs an extra install; it is deliberately NOT an engine dependency:
    uv pip install --python <install root>\\runtime\\python\\Scripts\\python.exe resemblyzer webrtcvad-wheels
    (a venv built by `python -m venv` instead of uv has pip: python -m pip install resemblyzer webrtcvad-wheels)
"""
from __future__ import annotations

import argparse
import glob
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

AUDIO_SUFFIXES = {".wav", ".ogg", ".mp3", ".flac", ".opus", ".aiff", ".aif"}
# The encoder is a 16 kHz model. This is its rate, not the engine's 48 kHz.
ENCODER_SR = 16000
GROUP_SUFFIX = re.compile(r"_t\d+$")

# This step's name in the progress protocol (`scripts/training/_layout.py`).
STAGE = "score"

#: `lora_checkpoint_best_val_loss_0002000_0.843894` -> step 2000, val loss 0.843894. The trainer
#: puts both in the directory name, which is the only place a scorer can read them from: this
#: stage never sees the training log.
VALIDATED = re.compile(r"best_val_loss_(?P<step>\d+)_(?P<loss>[\d.]+)")
#: `lora_checkpoint_0002000` - a PERIODIC checkpoint. No validation stands behind it.
PERIODIC = re.compile(r"checkpoint_(?P<step>\d+)$")


def selection_key(entry: dict) -> tuple[float, float, float, float]:
    """Sort key for `groups`, used with `reverse=True`, so bigger is better in every component.

    1. `lower_bound` - the worst clip in the group, which is what a listener actually hears.
    2. whether a validation selected this checkpoint at all. Periodic checkpoints and
       `checkpoint_final` are written unconditionally; a tie between them and a validated one
       must not go to the unvalidated candidate.
    3. the validation loss carried in the directory name, lower being better.
    4. the step, earlier being better - the safer side of an overfitting curve, and the same
       direction the trainer's own tie-break takes.
    """
    name = str(entry.get("group", ""))
    validated = VALIDATED.search(name)
    if validated is not None:
        return (
            float(entry.get("lower_bound", 0.0)),
            1.0,
            -float(validated["loss"]),
            -float(validated["step"]),
        )
    periodic = PERIODIC.search(name)
    step = float(periodic["step"]) if periodic is not None else float("inf")
    # No val loss to rank on, so this candidate loses every tie it takes part in.
    return (float(entry.get("lower_bound", 0.0)), 0.0, float("-inf"), -step)


def collect_refs(ref_dir: Path) -> list[str]:
    if not ref_dir.is_dir():
        raise SystemExit(f"--ref-dir is not a directory: {ref_dir}")
    paths = sorted(
        str(p) for p in ref_dir.iterdir() if p.is_file() and p.suffix.lower() in AUDIO_SUFFIXES
    )
    if len(paths) < 2:
        raise SystemExit(
            f"need at least 2 reference clips in {ref_dir} (found {len(paths)}).\n"
            "  The leave-one-out ceiling is what makes a score interpretable, and it needs\n"
            "  more than one clip to leave one out of."
        )
    return paths


def collect_tests(patterns: list[str]) -> list[str]:
    found: list[str] = []
    for pattern in patterns:
        matches = sorted(glob.glob(pattern))
        if not matches:
            direct = Path(pattern)
            if direct.is_file():
                found.append(str(direct))
                continue
            raise SystemExit(f"--tests matched nothing: {pattern}")
        found.extend(matches)
    if not found:
        raise SystemExit("--tests matched nothing")
    return found


def group_of(path: str) -> str:
    """`lora_checkpoint_0001000_t2.wav` -> `lora_checkpoint_0001000`."""
    stem = Path(path).stem
    return GROUP_SUFFIX.sub("", stem)


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(q / 100.0 * (len(ordered) - 1)))))
    return round(ordered[index], 4)


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Speaker-similarity scoring against a reference corpus, with a LOO ceiling."
    )
    parser.add_argument("--label", required=True, help="Name for the results file.")
    parser.add_argument(
        "--ref-dir", type=Path, required=True, help="Reference corpus: the real recordings."
    )
    parser.add_argument(
        "--tests", nargs="+", required=True, help="Generated clips: paths or glob patterns."
    )
    parser.add_argument(
        "--out-dir", type=Path, default=Path("eval/results"), help="Where the JSON report goes."
    )
    parser.add_argument(
        "--device",
        default="cpu",
        help=(
            "Encoder device (default cpu). The d-vector encoder is small; leaving the GPU "
            "to the trainer costs seconds here."
        ),
    )
    parser.add_argument(
        "--no-ceiling",
        action="store_true",
        help="Skip the leave-one-out pass. You lose the only reference point for the numbers.",
    )
    _engine.add_progress_flags(parser)
    args = parser.parse_args()
    _engine.progress_mode(args, STAGE)
    _engine.guard(STAGE, lambda: score(args))


def score(args: argparse.Namespace) -> None:
    # Resolve inputs before paying for the imports: a mistyped glob should fail now, not
    # after ten seconds of loading torch and a speaker encoder.
    ref_paths = collect_refs(args.ref_dir.expanduser())
    test_paths = collect_tests(args.tests)
    print(f"refs       {len(ref_paths)} from {args.ref_dir}")
    print(f"tests      {len(test_paths)}")
    # Every clip is embedded once, refs included, so the bar counts both.
    total = len(ref_paths) + len(test_paths)
    _engine.emit(
        STAGE,
        "start",
        f"{len(test_paths)} generated clip(s) against {len(ref_paths)} reference clip(s) "
        f"on {args.device}",
    )

    try:
        import librosa
        import numpy as np
        from resemblyzer import VoiceEncoder
    except ImportError as exc:
        raise SystemExit(
            f"{exc.name} is missing. The similarity harness is an opt-in install, not an\n"
            "engine dependency:\n"
            "  uv pip install --python <runtime\\python\\Scripts\\python.exe> resemblyzer webrtcvad-wheels\n"
            "  (or, in a venv that has pip: python -m pip install resemblyzer webrtcvad-wheels)"
        ) from exc

    encoder = VoiceEncoder(args.device)
    embedded = 0

    def embed(path: str) -> "np.ndarray":
        nonlocal embedded
        wav, _ = librosa.load(path, sr=ENCODER_SR, mono=True)
        vector = encoder.embed_utterance(wav.astype(np.float32))
        embedded += 1
        _engine.emit(STAGE, "progress", Path(path).name, done=embedded, total=total)
        return vector

    ref_embs = np.stack([embed(path) for path in ref_paths])
    test_embs = np.stack([embed(path) for path in test_paths])
    sim = test_embs @ ref_embs.T

    report: dict = {
        "label": args.label,
        "encoder": "resemblyzer-ge2e-256",
        "encoder_sample_rate": ENCODER_SR,
        "ref_dir": str(args.ref_dir),
        "num_refs": len(ref_paths),
        "tests": [],
        "groups": [],
    }
    for index, path in enumerate(test_paths):
        row = sim[index]
        report["tests"].append(
            {
                "path": path,
                "group": group_of(path),
                "sim_mean": round(float(row.mean()), 4),
                "sim_max": round(float(row.max()), 4),
                "sim_min": round(float(row.min()), 4),
            }
        )

    ceiling_p10: float | None = None
    if not args.no_ceiling:
        gram = ref_embs @ ref_embs.T
        count = len(ref_paths)
        loo = [
            sum(float(gram[j][k]) for k in range(count) if k != j) / (count - 1)
            for j in range(count)
        ]
        report["ceiling_loo"] = {
            "mean": round(float(np.mean(loo)), 4),
            "p10": percentile(loo, 10),
            "p90": percentile(loo, 90),
            "min": round(float(np.min(loo)), 4),
        }
        ceiling_p10 = report["ceiling_loo"]["p10"]

    # Per-group aggregation, because selection is a decision about a checkpoint and not
    # about one utterance. `lower_bound` is the number to select on: the worst clip in the
    # group, which is what a listener will eventually hear.
    for name in sorted({item["group"] for item in report["tests"]}):
        members = [item for item in report["tests"] if item["group"] == name]
        means = [item["sim_mean"] for item in members]
        entry = {
            "group": name,
            "clips": len(members),
            "mean": round(sum(means) / len(means), 4),
            "p10": percentile(means, 10),
            "lower_bound": round(min(item["sim_min"] for item in members), 4),
        }
        if ceiling_p10 is not None:
            entry["below_natural_p10"] = bool(entry["lower_bound"] < ceiling_p10)
        report["groups"].append(entry)
    # Ties are the normal case, not the exception, so the tie-break has to mean something.
    #
    # A real run scored `lora_checkpoint_0002000`, `lora_checkpoint_best_val_loss_0002000_...`
    # and `lora_checkpoint_final` at byte-identical numbers (mean 0.8013, p10 0.7864,
    # lower_bound 0.5745) because all three ARE the same weights, and a stable sort on
    # `lower_bound` alone then left them in alphabetical order - so the pipeline recommended
    # the PERIODIC checkpoint, purely because `0` sorts before `b` and `f`. `install_pack.py`
    # would have taken it without a word.
    #
    # So after `lower_bound`, prefer a checkpoint that a validation actually selected, then the
    # lower validation loss carried in its own directory name, then the earlier step - earlier
    # being the safer side of an overfitting curve. Nothing here invents quality: it only stops
    # a coin toss between identical scores from landing on the one candidate with no validation
    # behind it.
    report["groups"].sort(key=selection_key, reverse=True)

    out_dir = args.out_dir.expanduser()
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"{args.label}.json"
    out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    if "ceiling_loo" in report:
        ceiling = report["ceiling_loo"]
        print(
            f"ceiling    LOO mean {ceiling['mean']}   p10 {ceiling['p10']}   "
            f"p90 {ceiling['p90']}   (same speaker, natural variance)"
        )
        _engine.emit(
            STAGE,
            "log",
            f"natural ceiling: LOO mean {ceiling['mean']}, p10 {ceiling['p10']} - a "
            "generated clip is not supposed to beat this",
        )
    print(f"{'group':<52} {'clips':>5} {'mean':>7} {'lower':>7}")
    for entry in report["groups"]:
        flag = ""
        if entry.get("below_natural_p10"):
            flag = "  <- lower bound is under the natural p10"
        print(
            f"{entry['group'][:52]:<52} {entry['clips']:>5} {entry['mean']:>7.4f} "
            f"{entry['lower_bound']:>7.4f}{flag}"
        )
        _engine.emit(
            STAGE,
            "log",
            f"{entry['group']}: mean {entry['mean']}, lower bound {entry['lower_bound']}"
            + (" (under the natural p10)" if entry.get("below_natural_p10") else ""),
        )
    print(f"saved      {out}")
    print(
        "select     the checkpoint with the highest lower bound, not the highest mean; "
        "the mean hides the utterances that drift."
    )

    best = report["groups"][0] if report["groups"] else None
    _engine.emit(
        STAGE,
        "ok",
        f"best by lower bound: {best['group']} at {best['lower_bound']} "
        f"(mean {best['mean']}) -> {out}"
        if best is not None
        else f"nothing to rank -> {out}",
        done=embedded,
        total=total,
        # The group name is `lora_<checkpoint directory>`, which is what makes this the
        # selection the results table pre-selects.
        checkpoint=best["group"] if best is not None else None,
    )


if __name__ == "__main__":
    main()
