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
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _engine  # noqa: E402

HERE = Path(__file__).resolve().parent
UPSTREAM = "train.py"

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
    _engine.add_engine_args(parser)
    args = parser.parse_args()

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
        raise SystemExit(f"--init-checkpoint is not a file: {checkpoint}")

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

    print(engine.describe())
    print(f"  trainer       {engine.upstream / UPSTREAM}")
    print(f"  config        {config}")
    print(
        f"  manifest      {manifest}   {rows} rows, {frames:,} frames "
        f"({frames / _engine.LATENT_FRAMES_PER_SECOND / 60.0:.1f} min)"
    )
    print(f"  checkpoint    {checkpoint}")
    print(f"  output        {output_dir}")
    for line in summarise(load_train_section(config), rows, output_dir):
        print(line)
    print("command:")
    print("  " + engine.command_line(UPSTREAM, argv))
    if args.check:
        return

    output_dir.mkdir(parents=True, exist_ok=True)
    log = args.log.expanduser() if args.log else None
    if log is not None:
        print(f"output -> {log}")
    raise SystemExit(engine.run_upstream(UPSTREAM, argv, log=log))


if __name__ == "__main__":
    main()
