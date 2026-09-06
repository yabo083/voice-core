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
import hashlib
import json
import re
import shutil
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
#: `checkpoint_0002000` -> 2000. A periodic save, whose name upstream leaves lossless even
#: though a validation ran at that exact step.
PERIODIC_STEP = re.compile(r"^checkpoint_(?P<step>\d+)$")

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
    # Steps per epoch is stated by `cadence_for`, which needs the same number and rounds it UP -
    # a partial batch still has to be trained through. This used to print a floored copy of it:
    # 81 rows at batch 16 read as 5 here and 6 there, in the same block.
    if valid_ratio > 0:
        held = max(1, int(rows * valid_ratio))
        # The interval is deliberately not stated here: `cadence_for` derives it from the corpus and
        # prints the value that will actually be used. Printing `train_cfg['valid_every']` too meant
        # two different numbers in the same block, the config's and the effective one.
        lines.append(f"  validation    valid_ratio {valid_ratio} -> ~{held} held-out row(s)")
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
    # No QoS declination here, deliberately. This step used to declare it, and it did nothing:
    # the trainer runs in a separate process and the policy is not inherited, so the call only
    # ever covered this launcher. Reaching the real trainer was implemented and then MEASURED,
    # interleaved on/off/on/off at 14 steps a run: median 784 ms/step declined against 792 ms
    # under Windows' heuristic, i.e. 1.01x. A training step carries backward and optimizer work,
    # so core placement does not gate it the way it gates the synthesis dispatch loop (3.02x
    # there, `worker/irodori/worker.py`). The mechanism is gone rather than kept dark: an
    # unused knob in a training path is one more thing to rule out next time somebody measures.
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
            if seen["arm"] is not None and seen["stop"] is None:
                # A training step after the validation boundary means every save at that boundary
                # finished. Safe to ask for termination now; `stream_upstream` does it.
                seen["stop"] = seen["arm"]
                print(f"  early stop    {seen['arm']}; stopping at step {bar['done']}")
                _engine.emit(STAGE, "log", f"early stop: {seen['arm']} (step {bar['done']})")
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
            # Every validation, keyed by step. `finalize_checkpoint_names` needs this to stamp the
            # loss onto a periodic checkpoint: `save_every` is a multiple of `valid_every`, so a
            # periodic save always lands on a step that was just validated, and the number exists
            # even though upstream leaves it out of that directory's name.
            seen["history"][int(valid["step"])] = valid["loss"]
            # Patience is counted here and not on the `saved` line: a validation that does not
            # improve produces no checkpoint and therefore no `saved` line, which is exactly the
            # event this has to count.
            try:
                value = float(valid["loss"])
            except ValueError:
                value = None
            note = ""
            if value is not None:
                if seen["floor"] is None or value < seen["floor"]:
                    seen["floor"] = value
                    seen["stale"] = 0
                else:
                    seen["stale"] += 1
                    note = f"   no improvement on {seen['floor']:.6f} ({seen['stale']}/{EARLY_STOP_PATIENCE})"
                    if seen["stale"] >= EARLY_STOP_PATIENCE:
                        # Arm, do not fire. Upstream is mid-boundary here: the leaderboard save and
                        # the periodic save both still have to happen at this step, and terminating
                        # between them would leave a half-written directory. The next training
                        # progress line proves the boundary is finished.
                        seen["arm"] = (
                            f"{EARLY_STOP_PATIENCE} validations without improving on "
                            f"{seen['floor']:.6f}"
                        )
            _engine.emit(
                STAGE,
                "progress",
                f"validation at step {valid['step']}: val loss {valid['loss']}{note}",
                done=int(valid["step"]),
                total=seen["total"],
            )
            return

        saved = SAVED.match(text)
        if saved is not None:
            step = CKPT_STEP.search(saved["name"])
            loss = saved["loss"] or seen["val"] or "?"
            # `checkpoint` is "the last one upstream wrote", which is what proves a run produced
            # something selectable at all. `best` below is the one worth installing. Dropping
            # this line while adding `best` made every successful run fail its own completion
            # guard - caught by a 12-step run, which is why that run exists.
            seen["checkpoint"] = saved["name"]
            # The name carries the loss, so the summary at the end has a number even when the
            # `valid step=` line that produced it scrolled past before this run was watched.
            if loss != "?":
                seen["val"] = loss
            # Upstream keeps a top-N leaderboard, not the single best: it saves whenever the new
            # loss beats the WORST of the N it is holding, or whenever it holds fewer than N
            # (`train.py:386-393`). So while `checkpoint_best_n` exceeds the number of
            # validations the gate never engages and every validation is saved - a run whose loss
            # goes 0.804 -> 0.843 -> 0.839 -> 0.844 keeps all four under a name that says `best`.
            # The leaderboard is wanted (val loss does not decide; §score does, on similarity), so
            # the fix is honest naming, not fewer candidates: `finalize_checkpoint_names` below
            # leaves `best` only on the minimum.
            try:
                value = float(loss)
            except ValueError:
                value = None
            if value is not None and (seen["best"] is None or value < seen["best"][0]):
                seen["best"] = (value, saved["name"])
            best_now = seen["best"] is not None and seen["best"][1] == saved["name"]
            headline = "lowest val loss so far" if best_now else "checkpoint saved"
            _engine.emit(
                STAGE,
                "progress",
                f"{headline}: {saved['name']} (val loss {loss})",
                done=int(step[1]) if step is not None else seen["done"],
                total=seen["total"],
                checkpoint=saved["name"],
            )
            return

        _engine.emit(STAGE, "log", text)

    return on_line


def flag_value(passthrough: list[str], flag: str) -> str | None:
    """`--max-steps 500` or `--max-steps=500` -> `"500"`, else None."""
    for index, item in enumerate(passthrough):
        if item == flag:
            return passthrough[index + 1] if index + 1 < len(passthrough) else None
        if item.startswith(f"{flag}="):
            return item.split("=", 1)[1]
    return None


def schedule_for(train_cfg: dict, passthrough: list[str]) -> list[str]:
    """Extra argv that keeps the LR schedule proportional when `--max-steps` is overridden.

    The trap this closes: `warmup_steps` and `stable_steps` are absolute step counts, and
    `--max-steps` does not touch them. The template is 100 + 1500 inside 2000, so the
    documented `-- --max-steps 1000` leaves 1600 steps of warmup+stable inside a 1000-step run
    and the WSD scheduler returns 1.0 for every step after warmup: the cosine decay silently
    disappears, and nothing in either program says so.

    So a shorter run gets the template's SHAPE rather than its numbers - the same 5% warmup and
    75% stable the template chose - and the derivation is printed, because a value the caller
    did not type is one they have to be told about. Passing either flag yourself turns this off
    entirely; that is the escape hatch for anyone who wants a different shape.
    """
    steps = flag_value(passthrough, "--max-steps")
    if steps is None:
        return []
    if flag_value(passthrough, "--warmup-steps") or flag_value(passthrough, "--stable-steps"):
        return []
    try:
        target = int(steps)
    except ValueError:
        # Let upstream's own argparse produce the error message for a bad value.
        return []
    base = int(train_cfg.get("max_steps") or 0)
    warmup = int(train_cfg.get("warmup_steps") or 0)
    stable = int(train_cfg.get("stable_steps") or 0)
    if target <= 0 or base <= 0 or warmup + stable <= 0:
        return []
    scaled_warmup = max(1, round(target * warmup / base))
    scaled_stable = max(0, round(target * stable / base))
    # Leave at least one step of decay, which is the phase this whole function exists to save.
    if scaled_warmup + scaled_stable >= target:
        scaled_stable = max(0, target - scaled_warmup - 1)
    print(
        f"  schedule      --max-steps {target} scales the template's {warmup}+{stable}/{base} "
        f"to warmup {scaled_warmup} + stable {scaled_stable}, leaving "
        f"{target - scaled_warmup - scaled_stable} step(s) of decay"
    )
    _engine.emit(
        STAGE,
        "log",
        f"schedule derived for --max-steps {target}: warmup {scaled_warmup}, "
        f"stable {scaled_stable}, decay {target - scaled_warmup - scaled_stable}",
    )
    return [
        "--warmup-steps",
        str(scaled_warmup),
        "--stable-steps",
        str(scaled_stable),
    ]


#: Validate and save once every 5-8 epochs, whichever end of that window the run can afford. The
#: window is the caller's: a cadence measured in epochs travels across corpus sizes, where a fixed
#: step count does not - 100 steps is 9 epochs on 163 rows and 1.6 on 1000.
EPOCHS_PER_VALIDATION = (8, 7, 6, 5)
#: Consecutive validations without a new minimum before the run is stopped.
EARLY_STOP_PATIENCE = 5


def cadence_for(train_cfg: dict, passthrough: list[str], rows: int) -> list[str]:
    """Derive `--valid-every` and `--save-every` from the corpus, in epochs rather than steps.

    A step count cannot be right for two corpus sizes at once: at batch 16, 100 steps is 9 epochs
    of 163 rows and 1.6 epochs of 1000. Both knobs that depend on this cadence are counted in
    validations, so the cadence has to be expressed in something that scales with the data.

    Picks the LONGEST interval in `EPOCHS_PER_VALIDATION` that still leaves enough validations for
    the two mechanisms downstream of it to work:

    * early stopping needs more than `EARLY_STOP_PATIENCE` of them, or it can never fire;
    * the leaderboard's eviction gate needs more than `checkpoint_best_n`, or every validation is
      written as a "best" - the failure this pipeline already shipped once.

    Longest rather than shortest because a validation on a 5% split is a handful of clips and
    therefore noisy; fewer, wider-spaced points are more comparable. If even the shortest interval
    cannot reach the floor, the cadence is clamped to hit it and that is said out loud, because a
    cadence which silently disables both mechanisms is worse than a coarse one.

    Passing either flag yourself turns all of this off - that is the escape hatch.
    """
    override = flag_value(passthrough, "--valid-every") or flag_value(passthrough, "--save-every")
    if override:
        # Still say what will happen. Nothing else in this block states the interval, and silence
        # here reads as "the config value applies" when the caller's flag is what applies.
        print(f"  cadence       caller set it to {override}; no derivation from corpus size")
        return []
    steps = flag_value(passthrough, "--max-steps") or train_cfg.get("max_steps")
    batch = flag_value(passthrough, "--batch-size") or train_cfg.get("batch_size")
    accum = flag_value(passthrough, "--gradient-accumulation-steps") or train_cfg.get(
        "gradient_accumulation_steps"
    )
    try:
        total = int(steps or 0)
        effective = int(batch or 0) * max(1, int(accum or 1))
    except ValueError:
        return []
    if rows <= 0 or total <= 0 or effective <= 0:
        return []
    best_n = int(train_cfg.get("checkpoint_best_n") or 0)
    floor = max(EARLY_STOP_PATIENCE, best_n) + 1
    # Round up: a partial epoch still has to be trained through before the epoch is over.
    per_epoch = -(-rows // effective)
    clamped = False
    for epochs in EPOCHS_PER_VALIDATION:
        cadence = per_epoch * epochs
        if total // cadence >= floor:
            break
    else:
        epochs = EPOCHS_PER_VALIDATION[-1]
        cadence = max(1, total // floor)
        clamped = True
    detail = (
        f"clamped to {floor} validations (even {epochs} epochs was too wide)"
        if clamped
        else f"{epochs} epoch(s)"
    )
    print(
        f"  cadence       {rows} row(s) / batch {effective} = {per_epoch} step(s) per epoch -> "
        f"validate and save every {cadence} step(s) ({detail}), "
        f"{total // cadence} validation(s) in {total} steps"
    )
    _engine.emit(
        STAGE,
        "log",
        f"cadence derived from {rows} rows: every {cadence} steps ({detail}), "
        f"{total // cadence} validations, early-stop patience {EARLY_STOP_PATIENCE}",
    )
    return ["--valid-every", str(cadence), "--save-every", str(cadence)]


def corpus_warnings(manifest: Path) -> list[str]:
    """What step 1 found wrong with the audio, said out loud before an hour is spent on it.

    `prepare_dataset.py` writes a QA report beside its dataset (`<dataset>.qa.json`) and, until
    this function existed, NOTHING read it. A real run trained on 163 clips of which 96 were
    flagged `clipping (peak 1.0)` and the only place that number appeared was one line of step
    1's console output, an hour earlier. Flagging is advisory by design - the clips still train -
    which is exactly why the flags have to reach the moment the cost is about to be paid.

    Read from the manifest's own directory rather than passed in: the caller gives us
    `train_manifest.jsonl`, and the stages agree on a scratch directory per pack, so the report
    is a sibling. Absent report, no output - an older scratch tree is not an error.
    """
    reports = sorted(manifest.parent.glob("*.qa.json"))
    lines: list[str] = []
    for report_path in reports:
        try:
            report = json.loads(report_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        problems = report.get("problems") or []
        if not problems:
            continue
        count = int(report.get("count") or 0)
        kinds: dict[str, int] = {}
        for entry in problems:
            issue = str(entry.get("issue", "?")) if isinstance(entry, dict) else str(entry)
            # `clipping (peak 1.0)` -> `clipping`: the parenthesis carries a per-clip number,
            # and what a reader needs first is how many clips share the kind.
            kind = issue.split("(")[0].strip()
            kinds[kind] = kinds.get(kind, 0) + 1
        summary = ", ".join(f"{kind} x{n}" for kind, n in sorted(kinds.items(), key=lambda kv: -kv[1]))
        scope = f" of {count} clip(s)" if count else ""
        lines.append(f"  corpus        {len(problems)} finding(s){scope}: {summary}")
        lines.append(f"                {report_path.name} - flagged clips still train")
        _engine.emit(
            STAGE,
            "log",
            f"corpus quality: {len(problems)} finding(s){scope}: {summary} ({report_path.name})",
        )
    return lines


#: Upstream's leaderboard prefix. Every member is named `best`, including the ones that are not.
LEADERBOARD_PREFIX = "checkpoint_best_val_loss_"
#: What a leaderboard member that did not win is renamed to. Same stem, so `evaluate_similarity`'s
#: `val_loss_<step>_<loss>` parse still reads step and loss out of either name.
CANDIDATE_PREFIX = "checkpoint_val_loss_"


def _tree_fingerprint(path: Path) -> str:
    """Content hash of a checkpoint tree, for proving two of them are the same weights."""
    digest = hashlib.sha256()
    for item in sorted(p for p in path.rglob("*") if p.is_file()):
        digest.update(item.relative_to(path).as_posix().encode())
        digest.update(item.stat().st_size.to_bytes(8, "little"))
        with item.open("rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
    return digest.hexdigest()


def finalize_checkpoint_names(
    output_dir: Path, winner: str, history: dict[int, str]
) -> tuple[list[str], list[str]]:
    """Make every checkpoint name carry what is known about it, and drop exact duplicates.

    Three things upstream leaves on the floor, all of them information it already had:

    1. It keeps the N lowest validation losses and names all of them `best`, which is true of the
       set and false of each member. Keeping the set is right - validation loss does not decide
       which checkpoint ships, the similarity score does - so the members that did not win are
       renamed rather than deleted, and one directory says `best`.
    2. A periodic save lands on a step that was just validated (`save_every` is a multiple of
       `valid_every`), but its name carries no loss, so it reads as "no validation behind this"
       when there is one. `history` supplies the number.
    3. The periodic save at `max_steps`, `checkpoint_final`, and a leaderboard member at the same
       step are the SAME WEIGHTS under three names. Scoring all three spends the sample-generation
       and similarity stages three times to produce one number - measured once at mean 0.8013,
       p10 0.7864, identical to six decimals. Duplicates are collapsed, keeping the most
       informative name, and only after `_tree_fingerprint` proves the contents match.

    Safe only after the trainer exits: upstream globs the leaderboard prefix to prune during the
    run and never afterwards, and this wrapper has no resume path. Returns (renamed, dropped).
    """
    renamed: list[str] = []
    for path in sorted(output_dir.glob(f"{LEADERBOARD_PREFIX}*")):
        if path.name == winner:
            continue
        target = path.with_name(CANDIDATE_PREFIX + path.name[len(LEADERBOARD_PREFIX) :])
        if target.exists():
            continue
        path.rename(target)
        renamed.append(target.name)

    # Stamp the validated loss onto periodic saves. `checkpoint_final` is deliberately left alone:
    # its name states what it is, and if it duplicates a validated tree the pass below removes it.
    for path in sorted(output_dir.glob("checkpoint_*")):
        step_match = PERIODIC_STEP.match(path.name)
        if step_match is None:
            continue
        loss = history.get(int(step_match["step"]))
        if loss is None:
            continue
        target = path.with_name(f"{CANDIDATE_PREFIX}{step_match['step']}_{loss}")
        if target.exists():
            continue
        path.rename(target)
        renamed.append(target.name)

    # Collapse byte-identical trees. Named-by-loss beats `checkpoint_final` beats a bare step, and
    # `best` beats everything, so sorting by that rank keeps the name a reader learns most from.
    def rank(path: Path) -> tuple[int, str]:
        name = path.name
        if name.startswith(LEADERBOARD_PREFIX):
            return (0, name)
        if name.startswith(CANDIDATE_PREFIX):
            return (1, name)
        return (2, name) if name == "checkpoint_final" else (3, name)

    dropped: list[str] = []
    keepers: dict[str, Path] = {}
    for path in sorted((p for p in output_dir.glob("checkpoint_*") if p.is_dir()), key=rank):
        fingerprint = _tree_fingerprint(path)
        kept = keepers.get(fingerprint)
        if kept is None:
            keepers[fingerprint] = path
            continue
        shutil.rmtree(path)
        dropped.append(f"{path.name} (same weights as {kept.name})")
    return renamed, dropped


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
    for line in corpus_warnings(manifest):
        print(line)
    for line in summarise(train_cfg, rows, output_dir):
        print(line)
    # Both derive argv the caller did not type, and both print what they derived, so they belong
    # inside this block rather than above it.
    argv += schedule_for(train_cfg, args.passthrough)
    argv += cadence_for(train_cfg, args.passthrough, rows)
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
    # `bar` is the dedupe key for tqdm redraws; `best` is the (val loss, name) MINIMUM across
    # every validation, which is what the `ok` event reports - not `checkpoint`, which is only
    # the last one written. See the `saved` branch in `relay` for why those differ.
    seen = {
        "total": steps, "done": -1, "bar": None, "checkpoint": None,
        "val": None, "best": None, "history": {},
        # Early stopping: `floor` is the lowest val loss seen, `stale` the consecutive validations
        # since it moved, `arm` the reason once patience is spent, `stop` the same reason once a
        # later training step proves the save boundary closed.
        "floor": None, "stale": 0, "arm": None, "stop": None,
    }
    status = engine.stream_upstream(
        UPSTREAM, argv, on_line=relay(seen), should_stop=lambda: seen["stop"]
    )
    if status != 0 and seen["stop"] is None:
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
    best = seen["best"]
    if best is None:
        # Every checkpoint upstream wrote carried an unparseable loss, so there is a tree on
        # disk but nothing this side can rank. Name the last one and say the ranking failed,
        # rather than presenting it as a selection.
        _engine.emit(
            STAGE,
            "ok",
            f"{seen['done']} step(s) done; {seen['checkpoint']} saved last, val loss unreadable "
            "- pick a checkpoint from the scores or by ear",
            done=seen["done"],
            total=seen["total"],
            checkpoint=seen["checkpoint"],
        )
        return
    loss, name = best
    renamed, dropped = finalize_checkpoint_names(output_dir, name, seen["history"])
    head = f"{seen['done']} step(s) done"
    if seen["stop"] is not None:
        # A stopped run reached its answer earlier, which is a result and not a failure. Say why,
        # so nobody reads a 600-step run against a 2000-step budget as a crash.
        head = f"stopped early at step {seen['done']} of {seen['total']} - {seen['stop']}"
    tail = f"; {len(renamed)} other checkpoint(s) named by val loss" if renamed else ""
    if dropped:
        tail += f"; {len(dropped)} duplicate(s) dropped ({', '.join(dropped)})"
    _engine.emit(
        STAGE,
        "ok",
        f"{head}; lowest val loss {loss:.6f} at {name}{tail}",
        done=seen["done"],
        total=seen["total"],
        checkpoint=name,
    )


if __name__ == "__main__":
    main()
