#!/usr/bin/env python3
"""Launch upstream `train.py` with the environment and paths it needs - step 3.

Upstream owns the training loop and nothing here reimplements any of it. What this owns is
the four things a stranger gets wrong on the first attempt:

  * `--init-checkpoint`. LoRA and Speaker Inversion both refuse to start without it
    (`train.py:3352`, `:2744`), and it is a snapshot path inside the HuggingFace cache that
    nobody memorises. Resolved here from the same glob the worker uses.
  * `HF_HOME` / `HF_HUB_CACHE` / offline. Pristine upstream asks the hub for the text
    encoder with `local_files_only=False`; the environment is what keeps that read local.
  * The interpreter: the engine's venv, not whatever `python` is on PATH.
  * `PYTHONUNBUFFERED=1`, because tqdm into a pipe otherwise shows nothing for an hour.

It also pre-flights the manifest. A manifest whose latents have moved is the one mistake
that costs you the whole model load before failing.

    run_training.py --config lora --manifest corpus/my-voice/train_manifest.jsonl \\
                    --output-dir corpus/my-voice/lora

`--check` resolves everything, prints the command line it would run, and stops.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

HERE = Path(__file__).resolve().parent
UPSTREAM = "train.py"
# This step's name in the progress protocol (`scripts/training/_layout.py`).
STAGE = "train"

# The three lines of upstream's output a caller can act on. `train.py` has no JSON mode:
# it draws a tqdm bar on stderr, prints one metrics line every `log_every` steps, and
# prints these two as they happen. Matched here, next to the process that writes them,
# because a reader further away would be parsing a bar it can no longer see the shape of.
LOSS = re.compile(r"\bloss=(?P<loss>[-\d.]+(?:[eE][-+]?\d+)?)")
VALID = re.compile(r"^valid step=(?P<step>\d+)\s+loss=(?P<loss>[-\d.]+(?:[eE][-+]?\d+)?)")
SAVED = re.compile(
    r"^saved best val checkpoint:\s+(?P<name>\S+)(?:\s+\(loss=(?P<loss>[-\d.]+)\))?"
)
# `checkpoint_best_val_loss_0000500_0.906366` -> 500.
CKPT_STEP = re.compile(r"_(\d{4,})_")

# Windows spawns a fresh interpreter per DataLoader worker and each one re-imports torch:
# ~700 MB resident, and persistent workers keep it for the whole run. Measured on a 32 GB
# box, `num_workers: 8` across a train and a valid loader came to 16 workers and ~11.4 GB.
WORKER_RSS_MB = 700


def resolve_config(value: str) -> Path:
    """Accept a path, or the bare name of a template beside this script."""
    candidate = Path(value).expanduser()
    if candidate.is_file():
        return candidate.resolve()
    sibling = HERE / f"{value}.yaml"
    if sibling.is_file():
        return sibling
    available = ", ".join(sorted(path.stem for path in HERE.glob("*.yaml")))
    raise SystemExit(f"config not found: {value}\n  templates beside this script: {available}")


def load_train_section(config: Path) -> dict:
    """The `train:` block, for the pre-flight summary. Optional: a launcher must not fail
    because PyYAML is missing from whatever interpreter started it."""
    try:
        import yaml
    except ImportError:
        return {}
    payload = yaml.safe_load(config.read_text(encoding="utf-8")) or {}
    section = payload.get("train")
    return section if isinstance(section, dict) else {}


def preflight_manifest(manifest: Path) -> tuple[int, int]:
    """Row count and total latent frames, and a hard error if the latents are not where the
    manifest says they are. The loader resolves a relative `latent_path` against the
    manifest's own directory (`irodori_tts/dataset.py:199-203`), so moving one without the
    other is the failure this catches."""
    if not manifest.is_file():
        raise SystemExit(f"manifest not found: {manifest}")
    rows = 0
    frames = 0
    missing: list[str] = []
    base = manifest.parent
    for number, line in enumerate(manifest.read_text(encoding="utf-8").splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        if "text" not in row or "latent_path" not in row:
            raise SystemExit(
                f"{manifest}:{number}: needs 'text' and 'latent_path'. This is the manifest "
                "encode_latents.py writes, not the dataset file prepare_dataset.py writes."
            )
        rows += 1
        frames += int(row.get("num_frames") or 0)
        if len(missing) < 5:
            latent = Path(row["latent_path"])
            if not latent.is_absolute():
                latent = base / latent
            if not latent.is_file():
                missing.append(str(latent))
    if rows == 0:
        raise SystemExit(f"{manifest}: no rows")
    if missing:
        raise SystemExit(
            "the manifest points at latents that are not there:\n"
            + "".join(f"  MISSING {path}\n" for path in missing)
            + "  A relative latent_path resolves against the manifest's own directory.\n"
            "  Move the manifest and its latent folder together, or re-run encode_latents.py."
        )
    return rows, frames


def summarise(train_cfg: dict, rows: int, output_dir: Path) -> list[str]:
    if not train_cfg:
        return ["  (install PyYAML in this interpreter to see the config summary)"]
    lora = bool(train_cfg.get("lora_enabled"))
    inversion = bool(train_cfg.get("speaker_inversion_enabled"))
    mode = "LoRA adapter" if lora else "Speaker Inversion embedding" if inversion else "full model"
    batch = int(train_cfg.get("batch_size") or 0)
    steps = int(train_cfg.get("max_steps") or 0)
    workers = int(train_cfg.get("num_workers") or 0)
    persistent = bool(train_cfg.get("dataloader_persistent_workers"))
    valid_ratio = float(train_cfg.get("valid_ratio") or 0.0)
    lines = [
        f"  training      {mode}",
        f"  batch/steps   batch_size {batch}, max_steps {steps}",
    ]
    if batch and rows:
        lines.append(
            f"  epoch         ~{max(1, rows // max(1, batch))} step(s) per epoch over {rows} rows"
        )
    if valid_ratio > 0:
        held = max(1, int(rows * valid_ratio))
        lines.append(
            f"  validation    valid_ratio {valid_ratio} -> ~{held} held-out row(s), every "
            f"{train_cfg.get('valid_every')} steps"
        )
    else:
        lines.append(
            "  validation    valid_ratio 0 - no val loss, so no best-checkpoint selection"
        )
    if workers:
        sets = 2 if valid_ratio > 0 else 1  # a train and a valid loader each spawn their own
        lines.append(
            f"  dataloader    num_workers {workers}"
            f"{' persistent' if persistent else ''} -> up to {workers * sets} worker "
            f"processes, ~{workers * sets * WORKER_RSS_MB / 1024:.1f} GB RAM"
        )
    else:
        lines.append("  dataloader    num_workers 0 - loading happens in the training process")
    if lora:
        lines.append(
            f"  checkpoints   {output_dir}\\checkpoint_best_val_loss_<step>_<loss>\\  (directories)"
        )
    elif inversion:
        lines.append(
            f"  checkpoints   {output_dir}\\checkpoint_<step>.speaker.safetensors  (files)"
        )
    return lines


def main() -> None:
    _engine.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Run upstream Irodori train.py with the right environment.",
        epilog="Anything after -- is passed through to train.py unchanged.",
    )
    parser.add_argument(
        "--config",
        required=True,
        help=(
            "Config YAML, or the bare name of a template beside this script "
            "(lora, speaker-embedding)."
        ),
    )
    parser.add_argument(
        "--manifest", type=Path, required=True, help="Training manifest from encode_latents.py."
    )
    parser.add_argument("--output-dir", type=Path, required=True, help="Where checkpoints go.")
    parser.add_argument(
        "--init-checkpoint",
        type=Path,
        default=None,
        help="Base weights. Default: the v4.1-Small model.safetensors in the model cache.",
    )
    parser.add_argument(
        "--log",
        type=Path,
        default=None,
        help=(
            "Redirect the run's output to this file. Without it the output stays on the "
            "console, where tqdm's bar is live; with it, follow the file (PowerShell: "
            "Get-Content -Wait -Tail 20 <file>)."
        ),
    )
    parser.add_argument(
        "--check", action="store_true", help="Resolve and print everything, launch nothing."
    )
    parser.add_argument("passthrough", nargs="*", help=argparse.SUPPRESS)
    _engine.add_progress_flags(parser)
    _engine.add_engine_args(parser)
    args = parser.parse_args()
    _engine.progress_mode(args, STAGE)
    _engine.decline_eco_qos(STAGE)
    _engine.guard(STAGE, lambda: launch(args))


def relay(seen: dict):
    """Upstream's training output, as protocol events.

    A bar refresh is re-emitted only when the step number or the loss actually moved:
    tqdm redraws on a timer, up to ten times per step at 2 s/step, and an event per redraw
    is an hour of events nobody reads. Both halves of that key matter - tqdm sets its
    postfix once per `log_every` steps, so the first bar of a step often carries the
    PREVIOUS loss, or none at all, and the line that adds it does not advance the step.

    Bar lines stay out of the transcript for the same reason: the metrics line every
    `log_every` steps says the same thing once, and a traceback must not be buried under
    two thousand redraws.
    """

    def on_line(line: str) -> None:
        text = line.strip()
        if not text:
            return

        bar = _engine.parse_bar(text)
        if bar is not None:
            seen["total"] = bar["total"]
            loss = LOSS.search(text)
            loss = None if loss is None else loss["loss"]
            if (bar["done"], loss) == seen["bar"]:
                return
            seen["bar"] = (bar["done"], loss)
            seen["done"] = bar["done"]
            detail = "" if loss is None else f"   loss {loss}"
            _engine.emit(
                STAGE,
                "progress",
                f"step {bar['done']}/{bar['total']}{detail}   {bar['rate']}   ETA {bar['eta']}",
                done=bar["done"],
                total=bar["total"],
            )
            return

        print(text)
        valid = VALID.match(text)
        if valid is not None:
            seen["val"] = valid["loss"]
            _engine.emit(
                STAGE,
                "progress",
                f"validation at step {valid['step']}: val loss {valid['loss']}",
                done=int(valid["step"]),
                total=seen["total"],
            )
            return

        saved = SAVED.match(text)
        if saved is not None:
            seen["checkpoint"] = saved["name"]
            step = CKPT_STEP.search(saved["name"])
            loss = saved["loss"] or seen["val"] or "?"
            # The name carries the loss, so the summary at the end has a number even when the
            # `valid step=` line that produced it scrolled past before this run was watched.
            if loss != "?":
                seen["val"] = loss
            _engine.emit(
                STAGE,
                "progress",
                f"best checkpoint so far: {saved['name']} (val loss {loss})",
                done=int(step[1]) if step is not None else seen["done"],
                total=seen["total"],
                checkpoint=saved["name"],
            )
            return

        _engine.emit(STAGE, "log", text)

    return on_line


def launch(args: argparse.Namespace) -> None:
    engine = _engine.resolve_engine(args)
    engine.require_tree()

    config = resolve_config(args.config)
    manifest = args.manifest.expanduser().resolve()
    rows, frames = preflight_manifest(manifest)
    output_dir = args.output_dir.expanduser().resolve()
    checkpoint = (
        args.init_checkpoint.expanduser().resolve()
        if args.init_checkpoint is not None
        else engine.checkpoint()
    )
    if not checkpoint.is_file():
        raise SystemExit(
            f"--init-checkpoint is not a file: {checkpoint}\n"
            "  LoRA refuses to start without base weights; provision them first "
            "(scripts/bootstrap.ps1 -Only models) or pass --init-checkpoint"
        )

    argv = [
        "--config",
        str(config),
        "--manifest",
        str(manifest),
        "--output-dir",
        str(output_dir),
        "--init-checkpoint",
        str(checkpoint),
        *[item for item in args.passthrough if item != "--"],
    ]

    train_cfg = load_train_section(config)
    print(engine.describe())
    print(f"  trainer       {engine.upstream / UPSTREAM}")
    print(f"  config        {config}")
    print(
        f"  manifest      {manifest}   {rows} rows, {frames:,} frames "
        f"({frames / _engine.LATENT_FRAMES_PER_SECOND / 60.0:.1f} min)"
    )
    print(f"  checkpoint    {checkpoint}")
    print(f"  output        {output_dir}")
    for line in summarise(train_cfg, rows, output_dir):
        print(line)
    print("command:")
    print("  " + engine.command_line(UPSTREAM, argv))

    steps = int(train_cfg.get("max_steps") or 0) or None
    batch = int(train_cfg.get("batch_size") or 0) or None
    if args.check:
        _engine.emit(
            STAGE,
            "ok",
            f"check ok: {rows} row(s), {steps or '?'} step(s) at batch {batch or '?'}, "
            "nothing was launched",
        )
        return

    output_dir.mkdir(parents=True, exist_ok=True)
    if not _engine.json_enabled():
        log = args.log.expanduser() if args.log else None
        if log is not None:
            print(f"output -> {log}")
        raise SystemExit(engine.run_upstream(UPSTREAM, argv, log=log))

    # `steps` and `batch` come from the config FILE, and anything after `--` overrides them
    # inside the trainer - which is the documented way to change them, so the record has to
    # name it. Without this line a 100-step run announces the template's 2000.
    overrides = [item for item in args.passthrough if item != "--"]
    _engine.emit(
        STAGE,
        "start",
        f"{steps or '?'} steps at batch {batch or '?'} over {rows} row(s) -> {output_dir}"
        + (f"   overrides {' '.join(overrides)}" if overrides else ""),
    )
    # `bar` is the dedupe key for tqdm redraws; the rest is what the `ok` event reports.
    seen = {"total": steps, "done": -1, "bar": None, "checkpoint": None, "val": None}
    status = engine.stream_upstream(UPSTREAM, argv, on_line=relay(seen))
    if status != 0:
        raise SystemExit(
            f"{UPSTREAM} exited {status}\n"
            "  its own traceback is in this step's log; a CUDA out-of-memory here means "
            "batch_size is too high for this card or something else is holding the GPU"
        )
    if seen["checkpoint"] is None:
        # No best-val checkpoint means no way to choose one, which is a finished run that
        # produced nothing selectable.
        raise SystemExit(
            "the trainer finished without saving a best-validation checkpoint\n"
            "  valid_ratio must be above 0 for best-checkpoint selection, and the run must "
            "reach at least one valid_every boundary"
        )
    _engine.emit(
        STAGE,
        "ok",
        f"{seen['done']} step(s) done; best checkpoint {seen['checkpoint']} "
        f"(val loss {seen['val'] or '?'})",
        done=seen["done"],
        total=seen["total"],
        checkpoint=seen["checkpoint"],
    )


if __name__ == "__main__":
    main()
