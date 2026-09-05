#!/usr/bin/env python3
"""Where the install tree is, where its writable state lives, and the progress protocol.

Engine-agnostic on purpose: an install root, a data directory and a line of JSON are
properties of voice-core, not of whichever backend makes the sound. `irodori/_engine.py`
builds on this; `install_pack.py` needs only this.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
import traceback
from collections.abc import Callable
from pathlib import Path


def utf8_stdout() -> None:
    """Windows PowerShell 5.1 hands Python the console codepage (often 936 or 437), and
    printing a Japanese transcript through cp437 raises UnicodeEncodeError in the middle of
    a run. These scripts all print user-supplied text, so none of them may depend on the
    console's codepage."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


def install_root(start: Path | None = None) -> Path:
    """`<root>/scripts/training/[irodori/]x.py` -> `<root>`, in a dev checkout and in an
    installed tree alike.

    Walks up looking for something only a root has instead of counting `parents[]`, so
    moving a script one directory does not silently resolve against the wrong tree.
    """
    here = (start or Path(__file__)).resolve()
    for candidate in here.parents:
        if (candidate / "Cargo.toml").is_file():
            return candidate
        if (candidate / "bin" / "voice-core-runtime.exe").is_file():
            return candidate
        if (candidate / "runtime" / "engine").is_dir():
            return candidate
    # No marker: the tree above `scripts/` is still the best answer available.
    for candidate in here.parents:
        if candidate.name == "scripts":
            return candidate.parent
    return here.parent


def resolve_data_dir(explicit: Path | None = None) -> Path:
    """`--data-dir`, then `$VC_DATA_DIR`, then what the runtime itself resolves: `<root>/data`
    when it exists, else `%APPDATA%\\voice-core`
    (`src/bin/voice-core-runtime.rs::resolve_data_dir`).

    The runtime decides by writability because it has to create the directory; this only
    has to find the one already in use, so it decides by existence and never invents a
    second data directory beside a working one.
    """
    if explicit is not None:
        return explicit.expanduser().resolve()
    if os.environ.get("VC_DATA_DIR"):
        return Path(os.environ["VC_DATA_DIR"]).expanduser().resolve()
    preferred = install_root() / "data"
    if preferred.is_dir():
        return preferred.resolve()
    appdata = os.environ.get("APPDATA")
    if appdata:
        candidate = Path(appdata) / "voice-core"
        if candidate.is_dir():
            return candidate.resolve()
    return preferred.resolve()


# ---------------------------------------------------------------------- progress events --
# `scripts/bootstrap.ps1 -Json` defined this and `manager/src-tauri/src/jsonstream.rs`
# reads it: one JSON object per line on stdout, nothing else on stdout, every key always
# present so the reader never tests for one. `checkpoint` is this pipeline's single
# addition - the train and score stages have an artefact to name and the others do not.
#
# The emitter lives here, once, for the same reason the layout rules do: six scripts feed
# one reader, and a shape that is written six times is a shape that drifts.

STAGES = ("dataset", "latents", "train", "samples", "score", "install")
EVENTS = ("start", "progress", "log", "ok", "skip", "fail")

# The real stdout, taken away from human output by `json_mode`. None means human mode,
# which is what makes every `emit` call site free of an `if`.
_events = None


def add_json_flag(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--json",
        action="store_true",
        help=(
            "Emit one JSON progress event per line on stdout and nothing else, for a "
            "caller that renders progress rather than reads text "
            "(manager/src-tauri/src/jsonstream.rs). Human output moves to stderr."
        ),
    )


def json_mode() -> None:
    """Hand stdout to the protocol.

    Human output is not suppressed, it is moved: every `print` in these scripts keeps
    working and lands on stderr, which the caller redirects to a per-run log file. That
    is where a full transcript belongs, and it means one stray print cannot appear in the
    middle of the event stream - which would end the run, because a reader that expects a
    JSON object per line has to treat the rest as noise.

    Child processes are a separate problem: they inherit the OS handle, not this
    assignment, so anything that shells out under --json has to capture its child's
    output itself (`_engine.Engine.stream_upstream`).
    """
    global _events
    if _events is None:
        _events = sys.stdout
        sys.stdout = sys.stderr


def json_enabled() -> bool:
    return _events is not None


def emit(
    stage: str,
    event: str,
    message: str,
    *,
    done: int | None = None,
    total: int | None = None,
    remedy: str | None = None,
    checkpoint: str | None = None,
) -> None:
    """One event, or nothing at all outside --json."""
    if _events is None:
        return
    payload = {
        "ts": int(time.time() * 1000),
        "stage": stage,
        "event": event,
        "message": message,
        "done": done,
        "total": total,
        "remedy": remedy,
        "checkpoint": checkpoint,
    }
    # Flushed per line. A caller that shows a 50-minute run's progress gets nothing at
    # all if this buffers until exit, which is the same reason PYTHONUNBUFFERED is set
    # for the children.
    _events.write(json.dumps(payload, ensure_ascii=False) + "\n")
    _events.flush()


def guard(stage: str, body: Callable[[], None]) -> None:
    """Run a stage, and under --json turn its own refusal into a `fail` event.

    These scripts already explain every refusal in several lines - the first says what
    happened, the rest says what to do - so `message` and `remedy` are that split, not a
    second sentence written for the panel.

    The process then exits 0. A failed STAGE reports itself through the stream; a
    non-zero exit is reserved for "this argv was wrong", which is the one outcome the
    caller has to surface as a rejected call rather than as a stage that never ran. That
    is bootstrap's rule and `jsonstream.rs` depends on it, so argument parsing stays
    outside this: `parse_args` exiting 2 is exactly that case.
    """
    if _events is None:
        body()
        return
    try:
        body()
    except SystemExit as exc:
        if exc.code is None or exc.code == 0:
            raise
        message, remedy = split_reason(exc.code)
        emit(stage, "fail", message, remedy=remedy)
        raise SystemExit(0) from None
    except KeyboardInterrupt:
        # Cancellation, not a failure to explain. The caller killed the process tree and is
        # not waiting to be told why.
        raise
    except BaseException as exc:
        # The traceback is the only useful thing about an unexpected failure, and stderr
        # is where the caller keeps it. The event names the file so the panel can too.
        traceback.print_exc()
        message, _ = split_reason(f"{type(exc).__name__}: {exc}")
        emit(stage, "fail", message, remedy="the traceback is in this step's stderr log")
        raise SystemExit(0) from None


def split_reason(reason: object) -> tuple[str, str | None]:
    """First line is what happened, the rest is what to do about it."""
    if isinstance(reason, int):
        return f"exited with code {reason}", None
    lines = [line.strip() for line in str(reason).splitlines()]
    lines = [line for line in lines if line]
    if not lines:
        return "failed with no message", None
    return lines[0], " ".join(lines[1:]) or None
