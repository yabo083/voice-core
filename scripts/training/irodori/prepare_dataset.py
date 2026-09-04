#!/usr/bin/env python3
"""Turn a folder of clips plus their transcripts into a dataset file, with QA - step 1.

This is the only step that looks at your audio as audio, and it exists because upstream's
encoder starts one level higher up: `prepare_manifest.py` reads through HuggingFace
`datasets`, so it wants rows, not a folder. This writes those rows - and, more importantly,
it writes a QA report naming every clip it refused and why. The failure mode of voice
training is not a crash; it is two hours of GPU time spent on a dataset that had eleven
clipped clips and a transcript in the wrong language.

Output is one JSON object per line, UTF-8, no BOM, in the shape `datasets` loads:

    {"audio": "C:/corpus/line_01.wav", "text": "...", "speaker": "my-voice",
     "duration_s": 3.36, "sample_rate": 44100, "channels": 1, "subtype": "PCM_16"}

`audio`, `text` and `speaker` are the three columns step 2 names. `audio` is absolute so
the file works from any working directory. `speaker` is always present, empty when you did
not give one: a row with no speaker keeps its place and simply carries no `speaker_id`,
and the trainer handles that by using the clip itself as its own reference and masking the
speaker condition off (`irodori_tts/dataset.py:314-349`). Give a speaker id for a LoRA -
it is what makes the trainer draw a DIFFERENT clip of the same voice as the reference. The
remaining fields are QA provenance; upstream ignores columns it was not told about.

Note that upstream normalises the text before training (`--text-normalize`, on by
default): NFKC folding, `？！` to `?!`, `...` to `…`, and a pair of `「」` wrapping an entire
line is stripped. Do not hand-strip those; do not rely on them surviving either.

Audio requirements, from the codec rather than from folklore
------------------------------------------------------------
There is no format this rejects. The codec resamples ANY rate to its own and mean-downmixes
ANY channel count to mono before encoding (`irodori_tts/codec.py::encode_waveform`), and
upstream's encoder passes your file's native rate straight into it. There is still one
format that loses nothing:

    48000 Hz, mono, 16-bit PCM WAV or better.

48 kHz because that is the codec's native rate: upstream documents Semantic-DACVAE-
Japanese-32dim as a 48 kHz codec, and every WAV the engine writes is 48 kHz mono 16-bit.
Feeding it 44.1 kHz works - the reference dataset was 44.1 kHz Vorbis and trained fine -
but the band above 22 kHz is not there and upsampling cannot invent it.

Duration is where the engine does impose a bound. The trainer truncates each target latent
at `max_latent_steps: 750` frames and the codec runs at 25 latent frames per second, so
anything past 30.0 s loses its tail while its transcript is still fed in full: the model is
taught that this much text takes that much less time. Clips longer than that are skipped by
default for exactly that reason, not out of tidiness.
"""
from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

# libsndfile's own repertoire. Anything else (m4a, aac, wma) has to be converted first:
# this script refuses to guess at a file it cannot measure.
AUDIO_SUFFIXES = (".wav", ".flac", ".ogg", ".opus", ".mp3", ".aiff", ".aif")

# 750 latent frames / 25 fps. Both numbers are the engine's, not ours.
MAX_TARGET_SECONDS = 750 / _engine.LATENT_FRAMES_PER_SECOND

# The trainer's reference floor (`ref_min_seconds: 1.0`). A shorter clip is still a legal
# target, just a poor one, so dropping it is dataset hygiene rather than an engine limit -
# hence a flag rather than a rule.
DEFAULT_MIN_SECONDS = 1.0

# `max_text_len: 256` tokens including BOS, and the tokenizer truncates silently
# (`irodori_tts/tokenizer.py:111-116`). Characters are not tokens, so this only flags the
# lines worth looking at.
LONG_TEXT_CHARS = 160


def collapse(text: str) -> str:
    """One line of JSONL cannot contain a newline, and a transcript pasted out of a
    spreadsheet usually does."""
    return " ".join(str(text).split())


def scan_audio(audio_dir: Path, recursive: bool) -> list[Path]:
    walk = audio_dir.rglob("*") if recursive else audio_dir.glob("*")
    return sorted(path for path in walk if path.is_file() and path.suffix.lower() in AUDIO_SUFFIXES)


def _row_key(row: object) -> str | None:
    if not isinstance(row, dict):
        return None
    for field in ("audio", "audio_path", "file", "filename", "path", "name", "id", "clip"):
        value = row.get(field)
        if isinstance(value, str) and value.strip():
            return value
    return None


def load_transcripts(source: Path) -> dict[str, str]:
    """Read a transcript mapping. Keys are indexed twice - by file name and by stem, both
    lowercased - so a mapping may name `line_01`, `line_01.wav` or `line_01.ogg` and match.
    """
    table: dict[str, str] = {}

    def put(key: str, text: str) -> None:
        key = str(key).strip()
        text = collapse(text)
        if not key or not text:
            return
        name = Path(key).name.lower()
        table.setdefault(name, text)
        table.setdefault(Path(name).stem, text)

    suffix = source.suffix.lower()
    if source.is_dir():
        for sidecar in sorted(source.glob("*.txt")):
            put(sidecar.stem, sidecar.read_text(encoding="utf-8"))
    elif suffix == ".jsonl":
        for line in source.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            key = _row_key(row)
            if key is not None:
                put(key, row.get("text", ""))
    elif suffix == ".json":
        payload = json.loads(source.read_text(encoding="utf-8"))
        if isinstance(payload, dict):
            for key, value in payload.items():
                put(key, value if isinstance(value, str) else value.get("text", ""))
        elif isinstance(payload, list):
            for row in payload:
                key = _row_key(row)
                if key is not None:
                    put(key, row.get("text", ""))
        else:
            raise SystemExit(f"{source}: expected an object or a list of objects")
    elif suffix in {".csv", ".tsv"}:
        delimiter = "\t" if suffix == ".tsv" else ","
        with source.open("r", encoding="utf-8-sig", newline="") as handle:
            rows = list(csv.reader(handle, delimiter=delimiter))
        if not rows:
            raise SystemExit(f"{source}: empty")
        start = 0
        head = [cell.strip().lower() for cell in rows[0][:2]]
        if len(head) >= 2 and (head[1] == "text" or head[0] in {"audio", "file", "clip", "name"}):
            start = 1  # a header row, not data
        for row in rows[start:]:
            if len(row) >= 2:
                put(row[0], row[1])
    else:
        raise SystemExit(
            f"{source}: unsupported transcript source.\n"
            "  Use a directory of per-clip .txt sidecars, or a .jsonl / .json / .csv / .tsv\n"
            "  mapping. A bare .txt file is rejected on purpose: pairing lines with clips by\n"
            "  position silently mislabels the whole dataset when one clip is missing."
        )
    if not table:
        raise SystemExit(f"{source}: no usable transcript rows found")
    return table


def probe(path: Path) -> dict:
    """Duration, rate, channels, encoding and peak level for one clip.

    Peak needs the samples, so this decodes - in blocks, so a 20-minute file does not
    become 200 MB of RAM.
    """
    import numpy as np
    import soundfile as sf

    info = sf.info(str(path))
    peak = 0.0
    for block in sf.blocks(str(path), blocksize=1 << 18, dtype="float32", always_2d=True):
        if block.size:
            peak = max(peak, float(np.abs(block).max()))
    return {
        "duration_s": round(float(info.frames) / float(info.samplerate), 2),
        "sample_rate": int(info.samplerate),
        "channels": int(info.channels),
        "subtype": str(info.subtype),
        "peak": round(peak, 4),
    }


def percentile(values: list[float], q: float) -> float:
    """Nearest-rank percentile. Small datasets do not need interpolation, and a dependency
    for one line is not worth it."""
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(q / 100.0 * (len(ordered) - 1)))))
    return round(ordered[index], 2)


def build(args: argparse.Namespace) -> tuple[list[dict], dict]:
    audio_dir = args.audio_dir.expanduser().resolve()
    if not audio_dir.is_dir():
        raise SystemExit(f"--audio-dir is not a directory: {audio_dir}")
    clips = scan_audio(audio_dir, args.recursive)
    if not clips:
        raise SystemExit(
            f"no audio under {audio_dir}\n"
            f"  looked for: {', '.join(AUDIO_SUFFIXES)}"
            + ("" if args.recursive else "\n  (add --recursive to descend into subfolders)")
        )

    table: dict[str, str] = {}
    if args.transcripts is not None:
        table = load_transcripts(args.transcripts.expanduser().resolve())
    elif args.placeholder_text is None:
        # Sidecars beside the audio: the shape a user who typed their transcripts by hand
        # ends up with, and the only one worth guessing at.
        sidecars = {path.stem.lower(): path for path in audio_dir.rglob("*.txt")}
        if not sidecars:
            raise SystemExit(
                "no transcripts. Pick one:\n"
                "  --transcripts <dir|.jsonl|.json|.csv|.tsv>   a transcript per clip\n"
                "  a <clip>.txt beside each clip                same thing, no flag\n"
                "  --placeholder-text '<one sentence>'          speaker-embedding runs only,\n"
                "                                               which learn identity and not\n"
                "                                               a text mapping"
            )
        table = {
            key: collapse(path.read_text(encoding="utf-8")) for key, path in sidecars.items()
        }
        table = {key: value for key, value in table.items() if value}

    rows: list[dict] = []
    skipped: list[dict] = []
    problems: list[dict] = []
    for clip in clips:
        if args.placeholder_text is not None:
            text = collapse(args.placeholder_text)
        else:
            text = table.get(clip.name.lower()) or table.get(clip.stem.lower(), "")
        if not text:
            skipped.append({"clip": clip.name, "reason": "no transcript"})
            continue

        try:
            measured = probe(clip)
        except Exception as exc:  # one unreadable clip must not kill the pass
            skipped.append(
                {"clip": clip.name, "reason": f"unreadable ({type(exc).__name__}: {exc})"}
            )
            continue

        duration = measured["duration_s"]
        if args.min_seconds > 0 and duration < args.min_seconds:
            skipped.append(
                {"clip": clip.name, "reason": f"shorter than {args.min_seconds}s ({duration}s)"}
            )
            continue
        if args.max_seconds > 0 and duration > args.max_seconds:
            skipped.append(
                {
                    "clip": clip.name,
                    "reason": (
                        f"longer than {args.max_seconds}s ({duration}s): the trainer would keep "
                        f"only the first {MAX_TARGET_SECONDS:.0f}s of audio and all of the text"
                    ),
                }
            )
            continue

        if measured["peak"] >= 0.999:
            problems.append({"clip": clip.name, "issue": f"clipping (peak {measured['peak']})"})
        if measured["sample_rate"] < _engine.CODEC_SAMPLE_RATE:
            problems.append(
                {
                    "clip": clip.name,
                    "issue": (
                        f"{measured['sample_rate']} Hz is below the codec's "
                        f"{_engine.CODEC_SAMPLE_RATE} Hz; it will be upsampled"
                    ),
                }
            )
        if len(text) > LONG_TEXT_CHARS:
            problems.append(
                {
                    "clip": clip.name,
                    "issue": f"{len(text)} characters; text is truncated at 256 tokens",
                }
            )

        rows.append(
            {
                # Absolute, and forward-slashed: `datasets` reads this string as a path and
                # the file has to work from any working directory.
                "audio": clip.as_posix(),
                "text": text,
                # Always present, so every row has the same schema. Empty means "no
                # speaker id", which upstream turns into a row without speaker_id.
                "speaker": args.speaker_id,
                "duration_s": duration,
                "sample_rate": measured["sample_rate"],
                "channels": measured["channels"],
                "subtype": measured["subtype"],
            }
        )

    durations = [row["duration_s"] for row in rows]
    report = {
        "audio_dir": str(audio_dir),
        "speaker": args.speaker_id or None,
        "count": len(rows),
        "total_minutes": round(sum(durations) / 60.0, 1),
        "duration_mean_s": round(sum(durations) / len(durations), 2) if durations else 0.0,
        "duration_p05_s": percentile(durations, 5),
        "duration_p95_s": percentile(durations, 95),
        "duration_min_s": round(min(durations), 2) if durations else 0.0,
        "duration_max_s": round(max(durations), 2) if durations else 0.0,
        "sample_rates": sorted({row["sample_rate"] for row in rows}),
        "channels": sorted({row["channels"] for row in rows}),
        "subtypes": sorted({row["subtype"] for row in rows}),
        # Counted from the rows that survived, not from the clips that were offered.
        "placeholder_rows": len(rows) if args.placeholder_text is not None else 0,
        "problems": problems,
        "skipped": skipped,
    }
    return rows, report


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Build a dataset file and QA report from a folder of clips.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "The audio format that loses nothing: 48000 Hz, mono, 16-bit PCM WAV.\n"
            "Anything else is resampled and downmixed by the codec; nothing is rejected for\n"
            "its format. Clips over 30 s are skipped because the trainer would keep only\n"
            "their first 30 s of audio against all of their text."
        ),
    )
    parser.add_argument("--audio-dir", type=Path, required=True, help="Folder of clips.")
    parser.add_argument(
        "--recursive", action="store_true", help="Descend into subfolders of --audio-dir."
    )
    parser.add_argument(
        "--transcripts",
        type=Path,
        default=None,
        help=(
            "Transcript source: a directory of <clip>.txt sidecars, or a .jsonl / .json / "
            ".csv / .tsv mapping clip name to text. Omit to use <clip>.txt files sitting "
            "beside the audio."
        ),
    )
    parser.add_argument(
        "--placeholder-text",
        default=None,
        help=(
            "Use this one sentence as the text of every clip. ONLY for speaker-embedding "
            "runs, which learn a speaker identity and not a text mapping. A LoRA trained on "
            "placeholder text learns nothing about how this voice says words."
        ),
    )
    parser.add_argument(
        "--speaker-id",
        default="",
        help=(
            "Speaker label written into every row. Give it for a LoRA: it is what makes the "
            "trainer use other clips of this voice as the reference."
        ),
    )
    parser.add_argument(
        "--out-dataset", type=Path, required=True, help="Output JSONL path (step 2 reads this)."
    )
    parser.add_argument(
        "--qa-report",
        type=Path,
        default=None,
        help="Output JSON path for the QA report. Default: <out-dataset>.qa.json",
    )
    parser.add_argument(
        "--min-seconds",
        type=float,
        default=DEFAULT_MIN_SECONDS,
        help=(
            f"Skip clips shorter than this (default {DEFAULT_MIN_SECONDS}, the trainer's "
            "reference floor). 0 disables the check."
        ),
    )
    parser.add_argument(
        "--max-seconds",
        type=float,
        default=MAX_TARGET_SECONDS,
        help=(
            f"Skip clips longer than this (default {MAX_TARGET_SECONDS:.0f}, where the "
            "trainer starts truncating the audio but not the text). 0 disables the check."
        ),
    )
    args = parser.parse_args()

    if args.placeholder_text is not None and args.transcripts is not None:
        raise SystemExit("--placeholder-text and --transcripts are mutually exclusive")

    rows, report = build(args)
    if not rows:
        print(json.dumps(report, ensure_ascii=False, indent=2))
        raise SystemExit("no usable clips; nothing written")

    out = args.out_dataset.expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8"
    )
    qa_path = args.qa_report or out.with_suffix(out.suffix + ".qa.json")
    qa_path.parent.mkdir(parents=True, exist_ok=True)
    qa_path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"dataset   {out}   {report['count']} clips, {report['total_minutes']} min")
    print(
        f"duration  mean {report['duration_mean_s']}s   p05 {report['duration_p05_s']}s   "
        f"p95 {report['duration_p95_s']}s   max {report['duration_max_s']}s"
    )
    print(
        f"audio     {report['sample_rates']} Hz   {report['channels']} ch   {report['subtypes']}"
    )
    if report["placeholder_rows"]:
        print(
            f"text      {report['placeholder_rows']} rows carry placeholder text - "
            "speaker-embedding only, never a LoRA"
        )
    if not args.speaker_id:
        print(
            "speaker   no --speaker-id: rows carry no speaker, so the trainer will use each "
            "clip as its own reference. Fine for speaker-embedding, wrong for a LoRA."
        )
    print(f"skipped   {len(report['skipped'])}   problems {len(report['problems'])}   -> {qa_path}")
    for item in report["problems"][:10]:
        print(f"  ! {item['clip']}: {item['issue']}")
    for item in report["skipped"][:10]:
        print(f"  - {item['clip']}: {item['reason']}")


if __name__ == "__main__":
    main()
