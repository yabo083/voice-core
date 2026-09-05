#!/usr/bin/env python3
"""Encode a dataset into DACVAE latents with the engine's own codec - step 2.

One codec load in this process, then one latent per row of the dataset file step 1 wrote:
read the clip with `soundfile`, hand the waveform to
`irodori_tts.codec.DACVAECodec.encode_waveform`, save the tensor, write the manifest row
the trainer reads.

Why not upstream's `prepare_manifest.py`, which is the maintained path
---------------------------------------------------------------------
Because on this distribution it cannot decode audio at all, and the missing piece is not
shippable. `prepare_manifest.py` loads through HuggingFace `datasets` and casts the audio
column to `datasets.Audio`; `datasets` 4.x decodes audio ONLY through `torchcodec`, with no
soundfile fallback left in it, and `torchcodec` loads `libtorchcodec_core*.dll`, which needs
the FFmpeg shared libraries. This install has no FFmpeg and will not grow one - the whole
product is a single unsigned setup.exe with no new payloads. What that looked like from the
panel was every row skipped as `dataset_iter_error`, then `fail: nothing was encoded`.

Nothing downstream of here ever wanted `datasets`: upstream's `train.py` reads the manifest
and the `.pt` files, and the encode itself is `codec.encode_waveform(wav, sample_rate=sr)`
on a waveform. So the waveform is read with `soundfile` - the same library step 1 already
probes every clip with, so it demonstrably reads this corpus - and everything after that is
upstream's own code, unchanged. There is repo precedent for exactly this: the v1 scripts
(`assets/training/data/encode_shun_latents.py`) called the codec directly too.

What is reproduced from `prepare_manifest.py`, in its order (`_prepare_example`, then
`_handle_item`), so that a latent encoded here is the latent encoded there:

  * text: `irodori_tts.text_normalization.normalize_text`, then `.strip()`. That is
    upstream's `--text-normalize`, whose default is on. An empty result is skipped
    (`empty_text`), which is reachable even though step 1 refuses empty transcripts,
    because normalisation strips characters.
  * speaker id: `<namespace>:<component>`, both through upstream's own sanitiser (below).
    An empty speaker keeps the row and simply carries no `speaker_id`; the trainer handles
    that by using the clip as its own reference.
  * audio: decoded at its NATIVE rate as `(C, T)` float32. `--min-sample-rate` skips below
    a floor (`low_sample_rate`, disabled by default, as upstream); `--max-seconds` trims
    `wav[:, : int(max_seconds * rate)]` BEFORE the encode (`trimmed_empty` if nothing is
    left).
  * resample, downmix and loudness are deliberately NOT done here. `encode_waveform` does
    all three itself - `torchaudio.functional.resample` to `codec.sample_rate` (48000),
    `mean` over the channel axis, and `audiotools`' `normalize(--normalize-db)` +
    `ensure_max_of_audio()` per utterance - so doing any of them outside would be a second,
    subtly different definition of the same three numbers.
  * the latent: `encode_waveform(...)[0].cpu()`, i.e. `(T, 32)` float32, written with
    `torch.save`, and `num_frames` is that tensor's own `shape[0]`.

Measured, not assumed: the `.pt` files under `assets/training/data/latents/` are the latents
the v1 script encoded through this same codec, from clips under `assets/training/audio/` that
are byte-identical to the ones a smoke corpus still uses. Re-encoding those six clips here
gives the same tensors - same shape, same float32 dtype, maximum absolute difference 0.0.

    encode_latents.py --dataset-file corpus/my-voice.dataset.jsonl \\
                      --latent-dir corpus/my-voice/latents \\
                      --out-manifest corpus/my-voice/train_manifest.jsonl

`--check` validates the dataset file and the weights and encodes nothing.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import sys
import time
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

# This step's name in the progress protocol (`scripts/training/_layout.py`).
STAGE = "latents"
# 750 latent frames / 25 fps. Past this the trainer keeps only the first 30 s of a clip's
# audio while still feeding all of its text, so trimming here is the lesser evil.
MAX_SECONDS = 750 / _engine.LATENT_FRAMES_PER_SECOND

# The skip reasons, in the order the summary prints them. Upstream's vocabulary
# (`prepare_manifest.py::_inc_skip`) minus the reasons that were properties of `datasets`
# rather than of a clip (`dataset_iter_error`, `prepare_error`) and of flags this step does
# not have (`max_samples_limit`, `missing_speaker`). Each of these five can happen here.
REASONS = ("empty_text", "audio_decode", "low_sample_rate", "trimmed_empty", "encode_error")

# How many skipped clips reach the event stream. The console gets every one of them as it
# happens; the panel's log pane is not a place to put six hundred lines.
SHOWN = 10

# Flags that only ever described the HuggingFace `datasets` load, and now have nothing to
# reach. Rejected by name rather than deleted, so a saved command line says why.
RETIRED = {
    "dataset": "--dataset",
    "config": "--config",
    "split": "--split",
    "data_files": "--data-files",
}


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
            "  A row with no text cannot be encoded, so this would silently shrink the dataset."
        )
    absent = [row[audio_column] for row in rows if not Path(str(row[audio_column])).is_file()]
    if absent:
        raise SystemExit(
            f"{path}: {len(absent)} audio file(s) named in the dataset do not exist:\n"
            + "".join(f"  MISSING {item}\n" for item in absent[:10])
        )


def sanitize_id(value: object, fallback: str) -> str:
    """Upstream's speaker-id sanitiser, character for character
    (`prepare_manifest.py::_sanitize_id_component`): NFKC, whitespace to `_`, path
    separators to `-`, control characters dropped, every other non-word run to `-`, runs of
    `-` collapsed, `-_.` trimmed off both ends. A value that survives none of that becomes a
    sha1 prefix, and one over 96 characters is cut and given a sha1 suffix. Reproduced here
    rather than imported because importing it means importing `prepare_manifest`, which
    means importing `datasets` - the dependency this step exists to not have.
    """
    raw = "" if value is None else str(value).strip()
    if not raw:
        return fallback
    text = unicodedata.normalize("NFKC", raw)
    text = re.sub(r"\s+", "_", text)
    text = re.sub(r"[:/\\]+", "-", text)
    text = re.sub(r"[\x00-\x1f\x7f]", "", text)
    text = re.sub(r"[^\w.\-]+", "-", text, flags=re.UNICODE)
    text = re.sub(r"-{2,}", "-", text)
    text = text.strip("-_.")
    if not text:
        return hashlib.sha1(raw.encode("utf-8")).hexdigest()[:16]
    if len(text) > 96:
        return f"{text[:80]}-{hashlib.sha1(text.encode('utf-8')).hexdigest()[:10]}"
    return text


def latent_prefix_for(latent_dir: Path, manifest_dir: Path) -> Path:
    """The `latent_path` prefix every manifest row carries.

    Relative, because the loader resolves a relative `latent_path` against the manifest's
    own directory (`irodori_tts/dataset.py:199-203`), which is what lets a corpus be moved
    as one folder. Computed once - only the file name varies - which also puts the one way
    this can fail (two different Windows drives, where no relative path exists) before the
    first clip is encoded instead of after it.
    """
    try:
        return Path(os.path.relpath(latent_dir, start=manifest_dir))
    except ValueError as exc:
        raise SystemExit(
            "--latent-dir and --out-manifest are on different drives, so no relative path "
            f"joins them ({exc})\n"
            "  The trainer resolves latent_path against the manifest's own directory; keep "
            "the latents and the manifest on one drive."
        ) from exc


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Encode a local dataset file into DACVAE latents with the engine's codec.",
    )
    parser.add_argument(
        "--dataset-file",
        type=Path,
        required=True,
        help="Local JSONL from prepare_dataset.py: one object per line, with an audio path.",
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
            "would truncate the audio but not the text). 0 trims nothing."
        ),
    )
    parser.add_argument(
        "--min-sample-rate",
        type=int,
        default=0,
        help=(
            "Skip clips recorded below this rate instead of letting the codec upsample them "
            "(default 0, disabled, which is upstream's default; the codec's own rate is "
            f"{_engine.CODEC_SAMPLE_RATE})."
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
        "--check", action="store_true", help="Validate inputs, resolve the weights, encode nothing."
    )
    retired = parser.add_argument_group(
        "retired",
        "--dataset, --config, --split and --data-files described the HuggingFace `datasets` "
        "load this step no longer does. They are still parsed, and rejected by name: latents "
        "come from the local JSONL --dataset-file points at. For a HuggingFace dataset, run "
        "upstream's prepare_manifest.py directly.",
    )
    for attribute, flag in RETIRED.items():
        retired.add_argument(flag, dest=attribute, default=None, help=argparse.SUPPRESS)
    _engine.add_progress_flags(parser)
    _engine.add_engine_args(parser)
    args = parser.parse_args()
    named = [flag for attribute, flag in RETIRED.items() if getattr(args, attribute) is not None]
    if named:
        # A wrong argv exits non-zero on purpose: that is the one outcome the caller has to
        # surface as a rejected call rather than as a stage that ran and failed
        # (`_layout.py::guard`).
        parser.error(
            f"{', '.join(named)}: gone with the subprocess this step used to run. Latents are "
            "encoded in this process now, from the local JSONL that --dataset-file names. For "
            "a HuggingFace dataset, run upstream's prepare_manifest.py directly."
        )
    _engine.progress_mode(args, STAGE)
    _engine.decline_eco_qos(STAGE)
    if args.log is not None:
        print(f"output -> {args.log}")
    with transcript(args.log):
        _engine.guard(STAGE, lambda: encode(args))


@contextlib.contextmanager
def transcript(path: Path | None):
    """`--log`, in both modes.

    Only human output moves: `emit` writes to the stream `json_mode()` set aside before
    this runs, so `--json --log` keeps its event stream on stdout and puts its transcript -
    including a traceback, which `guard` prints to stderr - in the file.
    """
    if path is None:
        yield
        return
    path = path.expanduser()
    path.parent.mkdir(parents=True, exist_ok=True)
    saved = (sys.stdout, sys.stderr)
    with path.open("a", encoding="utf-8", errors="replace") as handle:
        sys.stdout, sys.stderr = handle, handle
        try:
            yield
        finally:
            sys.stdout, sys.stderr = saved


def encode(args: argparse.Namespace) -> None:
    engine = _engine.resolve_engine(args)
    engine.require_tree()
    # Before any of the engine's own imports, and cheap: this looks for torch, it does not
    # load it, so --check under the wrong interpreter still says which one to use.
    engine.require_own_interpreter()

    dataset_file = args.dataset_file.expanduser().resolve()
    rows = read_dataset_file(dataset_file)
    validate_dataset_file(rows, dataset_file, args.audio_column, args.text_column)
    latent_dir = args.latent_dir.expanduser().resolve()
    out_manifest = args.out_manifest.expanduser().resolve()
    latent_prefix = latent_prefix_for(latent_dir, out_manifest.parent)
    namespace = sanitize_id(
        args.speaker_id_prefix or dataset_file.stem.replace(".dataset", ""), fallback="dataset"
    )
    weights = engine.codec_weights()
    total = len(rows)
    trim = f"trim {args.max_seconds:g}s" if args.max_seconds > 0 else "no trim"
    floor = f"   skip below {args.min_sample_rate} Hz" if args.min_sample_rate > 0 else ""

    print(engine.describe())
    print("  encoder       irodori_tts.codec.DACVAECodec, in this process")
    print(f"  codec         {weights}")
    print(f"  dataset       {dataset_file}")
    print(f"  rows in       {total}")
    print(f"  speaker ids   {namespace}:<{args.speaker_column}>")
    print(f"  encode        {args.device}   {args.normalize_db:g} dB   {trim}{floor}")
    print(f"  latents    -> {latent_dir}")
    print(f"  manifest   -> {out_manifest}")
    print(f"  latent_path   {(latent_prefix / '00000000_00000000.pt').as_posix()}")
    if args.check:
        # Touch the base weights too, so a missing checkpoint is reported now rather than
        # at step 3.
        print(f"  checkpoint    {engine.checkpoint()}")
        print("check     ok")
        _engine.emit(STAGE, "ok", f"check ok: {total} row(s), nothing was encoded")
        return

    _engine.emit(
        STAGE, "start", f"encoding {total} row(s) with the engine's codec on {args.device}"
    )
    latent_dir.mkdir(parents=True, exist_ok=True)
    out_manifest.parent.mkdir(parents=True, exist_ok=True)

    # `activate()` puts webui/dacvae and webui/Irodori-TTS on sys.path and sets HF_HOME and
    # the offline flags, and both have to be true before the first `import torch` /
    # `import irodori_tts`, which is why these four imports are here and not at the top.
    engine.activate()
    import soundfile as sf  # noqa: E402
    import torch  # noqa: E402

    from irodori_tts.codec import DACVAECodec  # noqa: E402
    from irodori_tts.text_normalization import normalize_text  # noqa: E402

    if args.device.startswith("cuda") and not torch.cuda.is_available():
        raise SystemExit(
            f"--device {args.device}, but this torch reports no CUDA device\n"
            "  Pass --device cpu to encode without a GPU; a small corpus then takes minutes "
            "rather than seconds. Otherwise something else is holding the card, or the "
            "driver is older than this build of torch."
        )

    # Worth its own event: the codec is a 3-8 s load before the first clip, and silence
    # reads as a hang. The weights go in as the resolved file rather than as the repo id
    # upstream defaults to, so nothing asks the hub anything (`codec.py::load` takes either).
    _engine.emit(STAGE, "log", f"loading the codec onto {args.device}")
    clock = time.perf_counter()
    codec = DACVAECodec.load(
        str(weights),
        device=args.device,
        deterministic_encode=True,
        deterministic_decode=True,
        normalize_db=args.normalize_db,
    )
    loaded = time.perf_counter() - clock
    precision = str(codec.dtype).removeprefix("torch.")
    print(
        f"codec     {codec.sample_rate} Hz, {codec.latent_dim} dim, {precision} on "
        f"{codec.device}   loaded in {loaded:.1f}s"
    )
    _engine.emit(
        STAGE,
        "log",
        f"codec loaded in {loaded:.1f}s: {codec.sample_rate} Hz, {codec.latent_dim} dim, "
        f"{precision} on {codec.device}",
    )

    written = 0
    frames = 0
    skips: dict[str, int] = {}

    def skip(index: int, clip: Path, reason: str, detail: str) -> None:
        skips[reason] = skips.get(reason, 0) + 1
        print(f"  - {clip.name}   {reason}: {detail}", flush=True)
        if sum(skips.values()) <= SHOWN:
            _engine.emit(STAGE, "log", f"skipped {clip.name}: {reason}: {detail}")
        _engine.emit(
            STAGE, "progress", f"{clip.name} skipped: {reason}", done=index + 1, total=total
        )

    started = time.perf_counter()
    with out_manifest.open("w", encoding="utf-8") as handle:
        for index, row in enumerate(rows):
            clip = Path(str(row[args.audio_column]))
            text = normalize_text(str(row.get(args.text_column, ""))).strip()
            caption = None
            if args.caption_column:
                caption = str(row.get(args.caption_column, "")).strip() or None
            if not text:
                skip(index, clip, "empty_text", "no text left after normalisation")
                continue
            speaker_id = None
            if args.speaker_column:
                component = sanitize_id(row.get(args.speaker_column), fallback="")
                if component:
                    speaker_id = f"{namespace}:{component}"

            try:
                # Native rate, float32 in [-1, 1], and (frames, channels) - libsndfile's
                # own orientation, transposed below to the (C, T) the codec wants. This is
                # what `datasets.Audio` handed upstream, from a decoder that works here.
                data, rate = sf.read(str(clip), dtype="float32", always_2d=True)
                if data.size == 0:
                    raise ValueError("decoded audio is empty")
            except Exception as exc:
                skip(index, clip, "audio_decode", f"{type(exc).__name__}: {exc}")
                continue
            if args.min_sample_rate > 0 and rate < args.min_sample_rate:
                skip(
                    index, clip, "low_sample_rate", f"{rate} Hz, below {args.min_sample_rate} Hz"
                )
                continue

            wav = torch.from_numpy(data.T)  # a view of the read, not a second copy
            if args.max_seconds > 0:
                wav = wav[:, : int(args.max_seconds * rate)]
                if wav.numel() == 0:
                    reason = f"nothing left of it after {args.max_seconds:g}s"
                    skip(index, clip, "trimmed_empty", reason)
                    continue

            try:
                # Resample, downmix and loudness all happen in here, once, upstream's way.
                latent = codec.encode_waveform(wav, sample_rate=rate)[0].cpu()
            except Exception as exc:
                skip(index, clip, "encode_error", f"{type(exc).__name__}: {exc}")
                continue

            # `<written>_<row index>.pt`, upstream's own naming, so a manifest written here
            # and one written there are interchangeable down to the file names.
            name = f"{written:08d}_{index:08d}.pt"
            torch.save(latent, latent_dir / name)
            payload = {
                "text": text,
                # Forward slashes: this is JSON, where a Windows separator has to be
                # escaped and a single backslash silently becomes an escape. The same
                # reason step 1 writes `audio` as a posix path.
                "latent_path": (latent_prefix / name).as_posix(),
                "num_frames": int(latent.shape[0]),
            }
            if caption is not None:
                payload["caption"] = caption
            if speaker_id is not None:
                payload["speaker_id"] = speaker_id
            handle.write(json.dumps(payload, ensure_ascii=False) + "\n")
            # Per row, so a cancelled run leaves a manifest that matches the latents it
            # managed to write rather than an empty file.
            handle.flush()
            written += 1
            frames += int(latent.shape[0])
            seconds = wav.shape[1] / rate
            print(
                f"  {clip.name}   {seconds:.2f}s -> {latent.shape[0]} frames   {name}",
                flush=True,
            )
            _engine.emit(
                STAGE,
                "progress",
                f"{clip.name}: {latent.shape[0]} frames",
                done=index + 1,
                total=total,
            )

    elapsed = time.perf_counter() - started
    minutes = frames / _engine.LATENT_FRAMES_PER_SECOND / 60.0
    breakdown = ", ".join(f"{reason} {skips[reason]}" for reason in REASONS if skips.get(reason))
    print(f"encoded   {written} of {total} row(s) in {elapsed:.1f}s")
    print(f"manifest  {out_manifest}   {written} rows, {frames:,} frames ({minutes:.1f} min)")
    lost = total - written
    if lost > 0:
        print(f"skipped   {lost}   {breakdown}")
        # Each reason is a different mistake, and the count is the only thing that says
        # whether this run was a success or a near-total loss.
        print(
            f"WARNING   {lost} of {total} rows did not survive encoding. Read the skip "
            "counts above: empty_text, audio_decode, low_sample_rate, trimmed_empty and "
            "encode_error each mean something different."
        )
        _engine.emit(
            STAGE,
            "log",
            f"{lost} of {total} rows did not survive encoding: {breakdown}",
            remedy="the skip counts above say which check dropped them: empty_text, "
            "audio_decode, low_sample_rate, trimmed_empty and encode_error each mean "
            "something different",
        )
    if written == 0:
        raise SystemExit(
            "nothing was encoded\n"
            f"  every row was skipped ({breakdown}), so there is no manifest to train on; "
            "each clip and the reason it was dropped are above"
        )
    _engine.emit(
        STAGE,
        "ok",
        f"{written} row(s), {frames:,} frames ({minutes:.1f} min of audio) -> {out_manifest}",
        done=written,
        total=total,
    )


if __name__ == "__main__":
    main()
