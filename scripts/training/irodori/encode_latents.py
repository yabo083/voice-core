#!/usr/bin/env python3
"""Encode a dataset into DACVAE latents by running upstream's own tooling - step 2.

The encoder is upstream's `prepare_manifest.py`, not a reimplementation of it. That script
is the maintained dataset -> latents -> manifest path: it loads through HuggingFace
`datasets`, casts the audio column, encodes with the same codec settings the runtime uses,
shards across GPUs, and writes the training manifest. Everything this wrapper adds is the
part upstream leaves to you:

  * the environment (HF_HOME, HF_HUB_CACHE, offline, PYTHONUNBUFFERED),
  * the interpreter (the engine's venv, not whatever `python` is on PATH),
  * `--dataset-file`, which turns the local JSONL prepare_dataset.py writes into the
    `--dataset json --data-files ...` invocation `datasets` needs,
  * a before/after row count, because upstream skips unusable rows by design and a run
    that quietly encoded 3 of your 60 clips otherwise looks like a success.

What upstream writes, per row: `text`, `latent_path` (relative to the manifest, which is
how the loader resolves it), `num_frames` (the latent's own length), plus `speaker_id` and
`caption` when you name those columns. Nothing else - the QA fields from step 1 stay in
step 1's file.

Input audio: no format requirement. `prepare_manifest.py` decodes through `datasets`, hands
the waveform to `codec.encode_waveform(wav, sample_rate=sr)`, and that resamples ANY rate
to the codec's 48 kHz and mean-downmixes ANY channel count to mono
(`irodori_tts/codec.py`). 48 kHz mono is what loses nothing; 44.1 kHz stereo works and is
what the reference dataset was.

    encode_latents.py --dataset-file corpus/my-voice.dataset.jsonl \\
                      --latent-dir corpus/my-voice/latents \\
                      --out-manifest corpus/my-voice/train_manifest.jsonl

`--check` validates and prints the exact upstream command without running it.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

UPSTREAM = "prepare_manifest.py"
# 750 latent frames / 25 fps. Past this the trainer keeps only the first 30 s of a clip's
# audio while still feeding all of its text, so trimming here is the lesser evil.
MAX_SECONDS = 750 / _engine.LATENT_FRAMES_PER_SECOND


def read_dataset_file(path: Path) -> list[dict]:
    if not path.is_file():
        raise SystemExit(f"--dataset-file not found: {path}")
    rows: list[dict] = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"{path}:{number}: not valid JSON ({exc})") from exc
        rows.append(row)
    if not rows:
        raise SystemExit(f"{path}: no rows")
    return rows


def validate_dataset_file(rows: list[dict], path: Path, audio_column: str, text_column: str) -> None:
    missing_field = [
        index
        for index, row in enumerate(rows, start=1)
        if audio_column not in row or not str(row.get(text_column, "")).strip()
    ]
    if missing_field:
        raise SystemExit(
            f"{path}: {len(missing_field)} row(s) lack {audio_column!r} or a non-empty "
            f"{text_column!r} (first: line {missing_field[0]}).\n"
            "  Upstream skips rows with empty text, so this would silently shrink the dataset."
        )
    absent = [row[audio_column] for row in rows if not Path(str(row[audio_column])).is_file()]
    if absent:
        raise SystemExit(
            f"{path}: {len(absent)} audio file(s) named in the dataset do not exist:\n"
            + "".join(f"  MISSING {item}\n" for item in absent[:10])
        )


def count_manifest(path: Path) -> tuple[int, int]:
    if not path.is_file():
        return 0, 0
    rows = 0
    frames = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        rows += 1
        frames += int(row.get("num_frames") or 0)
    return rows, frames


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Run upstream prepare_manifest.py over a local or HuggingFace dataset.",
        epilog="Anything after -- is passed to prepare_manifest.py unchanged.",
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument(
        "--dataset-file",
        type=Path,
        default=None,
        help=(
            "Local JSONL from prepare_dataset.py. Loaded as `--dataset json --data-files "
            "<file>`, which is how `datasets` reads local rows."
        ),
    )
    source.add_argument(
        "--dataset",
        default=None,
        help="A HuggingFace dataset id, or a `datasets` builder name, passed through as-is.",
    )
    parser.add_argument("--config", default=None, help="Dataset config/subset (HF datasets).")
    parser.add_argument("--split", default="train", help="Dataset split (default train).")
    parser.add_argument(
        "--data-files", nargs="+", default=None, help="data_files for --dataset, if it needs any."
    )
    parser.add_argument("--audio-column", default="audio", help="Audio column (default audio).")
    parser.add_argument("--text-column", default="text", help="Text column (default text).")
    parser.add_argument(
        "--speaker-column",
        default="speaker",
        help=(
            "Speaker column (default speaker). Rows whose value is empty keep their place "
            "and simply carry no speaker_id, which the trainer handles."
        ),
    )
    parser.add_argument(
        "--speaker-id-prefix",
        default=None,
        help=(
            "Namespace prefixed to each speaker id. Default: the dataset file's stem. The "
            "value is cosmetic - what matters is that clips of one voice share it."
        ),
    )
    parser.add_argument("--caption-column", default=None, help="Optional style-caption column.")
    parser.add_argument("--latent-dir", type=Path, required=True, help="Where .pt latents go.")
    parser.add_argument(
        "--out-manifest", type=Path, required=True, help="Training manifest to write."
    )
    parser.add_argument("--device", default="cuda", help="Encoding device (default cuda).")
    parser.add_argument(
        "--max-seconds",
        type=float,
        default=MAX_SECONDS,
        help=(
            f"Trim each clip before encoding (default {MAX_SECONDS:.0f}s, where the trainer "
            "would truncate the audio but not the text). 0 passes no limit through."
        ),
    )
    parser.add_argument(
        "--normalize-db",
        type=float,
        default=-16.0,
        help=(
            "Per-clip loudness target before encoding (default -16.0, upstream's default and "
            "the value the runtime applies to reference audio at synthesis time)."
        ),
    )
    parser.add_argument("--log", type=Path, default=None, help="Redirect output to this file.")
    parser.add_argument(
        "--check", action="store_true", help="Validate inputs, print the command, run nothing."
    )
    parser.add_argument("passthrough", nargs="*", help=argparse.SUPPRESS)
    _engine.add_engine_args(parser)
    args = parser.parse_args()

    engine = _engine.resolve_engine(args)
    engine.require_tree()
    latent_dir = args.latent_dir.expanduser().resolve()
    out_manifest = args.out_manifest.expanduser().resolve()

    expected_rows = 0
    if args.dataset_file is not None:
        dataset_file = args.dataset_file.expanduser().resolve()
        rows = read_dataset_file(dataset_file)
        validate_dataset_file(rows, dataset_file, args.audio_column, args.text_column)
        expected_rows = len(rows)
        dataset = "json"
        data_files = [str(dataset_file)]
        prefix = args.speaker_id_prefix or dataset_file.stem.replace(".dataset", "")
    else:
        dataset = args.dataset
        data_files = [str(item) for item in (args.data_files or [])]
        prefix = args.speaker_id_prefix or dataset.replace("/", "-")

    argv = [
        "--dataset",
        dataset,
        "--split",
        args.split,
        "--audio-column",
        args.audio_column,
        "--text-column",
        args.text_column,
        "--speaker-column",
        args.speaker_column,
        "--speaker-id-prefix",
        prefix,
        "--output-manifest",
        str(out_manifest),
        "--latent-dir",
        str(latent_dir),
        "--device",
        args.device,
        "--normalize-db",
        str(args.normalize_db),
    ]
    if args.config:
        argv += ["--config", args.config]
    if data_files:
        argv += ["--data-files", *data_files]
    if args.caption_column:
        argv += ["--caption-column", args.caption_column]
    if args.max_seconds > 0:
        argv += ["--max-seconds", str(args.max_seconds)]
    argv += [item for item in args.passthrough if item != "--"]

    print(engine.describe())
    print(f"  encoder       {engine.upstream / UPSTREAM}")
    print(f"  dataset       {dataset}" + (f"   {data_files[0]}" if data_files else ""))
    if expected_rows:
        print(f"  rows in       {expected_rows}")
    print(f"  latents    -> {latent_dir}")
    print(f"  manifest   -> {out_manifest}")
    print("command:")
    print("  " + engine.command_line(UPSTREAM, argv))
    if args.check:
        # Touch the caches so a missing checkpoint is reported now rather than at step 3.
        print(f"  checkpoint    {engine.checkpoint()}")
        print(f"  codec         {engine.codec_weights()}")
        print("check     ok")
        return

    latent_dir.mkdir(parents=True, exist_ok=True)
    out_manifest.parent.mkdir(parents=True, exist_ok=True)
    status = engine.run_upstream(UPSTREAM, argv, log=args.log.expanduser() if args.log else None)
    if status != 0:
        raise SystemExit(f"{UPSTREAM} exited {status}")

    written, frames = count_manifest(out_manifest)
    minutes = frames / _engine.LATENT_FRAMES_PER_SECOND / 60.0
    print(f"manifest  {out_manifest}   {written} rows, {frames:,} frames ({minutes:.1f} min)")
    if expected_rows and written < expected_rows:
        # Upstream counts its own skips, but only the comparison says whether the run was
        # a success or a near-total loss.
        print(
            f"WARNING   {expected_rows - written} of {expected_rows} rows did not survive "
            "encoding. Read the skip counts above: empty_text, audio_decode, "
            "low_sample_rate, trimmed_empty and encode_error each mean something different."
        )
    if written == 0:
        raise SystemExit("nothing was encoded")


if __name__ == "__main__":
    main()
