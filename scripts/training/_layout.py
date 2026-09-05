#!/usr/bin/env python3
"""Where the install tree is, where its writable state lives, and the progress protocol.

Engine-agnostic on purpose: an install root, a data directory and a line of JSON are
properties of voice-core, not of whichever backend makes the sound. `irodori/_engine.py`
builds on this; `install_pack.py` needs only this.
"""
from __future__ import annotations

import argparse
import atexit
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

# The pair of files on disk, or None when nobody asked for one. Separate from `_events`
# because being watched and being observable are different things: an agent running these
# six steps from a shell reads the stream in its terminal, and the panel — which is not
# running the steps and may not even be open — reads the pair.
_record = None


def add_progress_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--json",
        action="store_true",
        help=(
            "Emit one JSON progress event per line on stdout and nothing else, for a "
            "caller that renders progress rather than reads text "
            "(manager/src-tauri/src/jsonstream.rs). Human output moves to stderr."
        ),
    )
    parser.add_argument(
        "--status-file",
        type=Path,
        default=None,
        help=(
            "Fold this step's events into this status file and append them to the "
            "transcript beside it. Point it at "
            "<data dir>\\logs\\training-<pack id>.status.json: that name, in that "
            "directory, is what the panel's 训练 screen lists, so writing there is what "
            "makes a run nobody started from the GUI visible in it."
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


def progress_mode(args: argparse.Namespace, stage: str) -> None:
    """Put the progress protocol in play for this step, as its own flags asked.

    One call rather than two, because the two flags are one decision - how this step
    reports itself - and a step that handed stdout to the stream but forgot the file on
    disk is exactly the half-observable run `--status-file` exists to prevent.
    """
    if getattr(args, "json", False):
        json_mode()
    status = getattr(args, "status_file", None)
    if status is None:
        return
    global _record
    try:
        # The first stage of a run starts the record over. A status folded across two runs
        # would answer "which stage" with a stage from the one before; every later stage
        # carries the earlier ones forward, which is what makes the finished file a record
        # of the whole pipeline instead of of its last step.
        _record = _Record(Path(status), fresh=stage == STAGES[0])
    except OSError as exc:
        # Observability is not a precondition for training: a step whose record cannot be
        # opened still runs, and says so where its human output goes.
        print(f"--status-file unavailable, running without it: {exc}")
        return
    atexit.register(_record.close)


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
    """One event, onto the stream, into the record, or nowhere at all.

    Nowhere is the default: with neither --json nor --status-file these scripts are a
    human's tool and what they print is their output.
    """
    if _events is None and _record is None:
        return
    payload = {
        "ts": now_ms(),
        "stage": stage,
        "event": event,
        "message": message,
        "done": done,
        "total": total,
        "remedy": remedy,
        "checkpoint": checkpoint,
    }
    # Serialised once for both destinations, so the file holds the bytes the caller was
    # given rather than a second rendering of the same object.
    line = json.dumps(payload, ensure_ascii=False)
    if _events is not None:
        # Flushed per line. A caller that shows a 50-minute run's progress gets nothing at
        # all if this buffers until exit, which is the same reason PYTHONUNBUFFERED is set
        # for the children.
        _events.write(line + "\n")
        _events.flush()
    if _record is not None:
        _record.absorb(payload, line)


def now_ms() -> int:
    return int(time.time() * 1000)


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


# ------------------------------------------------------------------- the record on disk --
# What `--status-file` writes: the transcript, and the status folded from it. Both are read
# by `manager/src-tauri/src/training.rs` and by any agent following a run
# (`skills/voice-core-voice-training/SKILL.md`), and neither says who ran the step - which is
# the point. A pipeline driven from a shell and a pipeline driven from the panel leave the same
# two files behind, so the 训练 screen shows a run it never started.

STAGE_KEYS = ("state", "message", "done", "total", "started", "ended")


def _run_stem(status: Path) -> str:
    """`…/training-my-voice.status.json` -> `training-my-voice`."""
    suffix = ".status.json"
    name = status.name
    return name[: -len(suffix)] if name.endswith(suffix) else status.stem


def _read_status(path: Path) -> dict | None:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return payload if isinstance(payload, dict) else None


def _blank_stage(name: str) -> dict:
    return {
        "stage": name,
        "state": "pending",
        "message": "",
        "done": None,
        "total": None,
        "started": None,
        "ended": None,
    }


class _Record:
    """One run's two files, written by the step that is running.

    `state` is the event kind that last described a stage - `pending`, `running`, `ok`,
    `skip`, `fail` - plus one word the stream cannot produce: `interrupted`, for a stage
    that was still running when the process ended. That distinction is the whole reason
    this is folded rather than left as a log: a `fail` was explained by the step that
    failed, with a remedy; an `interrupted` was killed, and there is nothing to explain.
    """

    # How often the status file may be rewritten while a stage is only making progress.
    # The fast stages emit one event per clip, and a rename per clip would be a hundred
    # filesystem transactions a second for a number nobody polls that fast. Transitions
    # are never throttled: `start`, `ok`, `skip` and `fail` are the answers a caller is
    # waiting for, so they publish the instant they arrive.
    INTERVAL_MS = 250

    def __init__(self, status_path: Path, *, fresh: bool) -> None:
        self.path = status_path.expanduser().resolve()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        stem = _run_stem(self.path)
        self.transcript = self.path.with_name(f"{stem}.jsonl")
        # Line buffered and held open for the step: `Get-Content -Wait` is the interface, so
        # a line nobody flushed is a line nobody sees, and reopening per line would be work
        # with no output.
        self.lines = self.transcript.open(
            "w" if fresh else "a", encoding="utf-8", buffering=1, newline="\n"
        )
        now = now_ms()
        self.status = {
            "schema": 1,
            "pack_id": stem[len("training-") :] if stem.startswith("training-") else stem,
            # True until this process ends. One that is killed outright cannot write
            # `false` here, which is what `pid` is for: the run is live only while that
            # process is.
            "live": True,
            "pid": os.getpid(),
            "stage": "",
            "state": "pending",
            "message": "",
            "done": None,
            "total": None,
            "failed_stage": None,
            "failure": None,
            "remedy": None,
            "started": now,
            "updated": now,
            "ended": None,
            "stages": [_blank_stage(name) for name in STAGES],
            "log": str(self.transcript),
        }
        if not fresh:
            self._carry(_read_status(self.path))
        self._published = 0
        self._publish()

    def _carry(self, previous: dict | None) -> None:
        """The stages that ran before this one, and when the run began.

        Merged row by row rather than adopted wholesale: the file may have been written by
        an older build, or by hand, and a `stages` array that is not six known rows would
        leave the reader inferring a stage's absence. The failure fields are deliberately
        NOT carried - a step that has just been re-run successfully is not still failing.
        """
        if previous is None:
            return
        if isinstance(previous.get("started"), int):
            self.status["started"] = previous["started"]
        earlier = {
            row.get("stage"): row
            for row in previous.get("stages") or []
            if isinstance(row, dict)
        }
        for row in self.status["stages"]:
            before = earlier.get(row["stage"])
            if before is None:
                continue
            for key in STAGE_KEYS:
                if key in before:
                    row[key] = before[key]

    def absorb(self, payload: dict, line: str) -> None:
        """One event: appended verbatim, then folded."""
        try:
            self.lines.write(line + "\n")
        except OSError:
            # Same rule as `_publish`: a step does not die because its record could not be
            # written. The stream the caller is reading is unaffected.
            pass
        transition = self._fold(payload)
        if transition or self.status["updated"] - self._published >= self.INTERVAL_MS:
            self._publish()

    def _fold(self, event: dict) -> bool:
        """Fold one event in, and say whether it was a transition - which is what publishes
        the file immediately rather than at the next throttle window."""
        status = self.status
        stage = event.get("stage") or ""
        kind = event.get("event") or "log"
        message = event.get("message") or ""
        status["updated"] = event.get("ts") or now_ms()
        if not stage:
            return False
        status["stage"] = stage
        if message:
            status["message"] = message
        if kind == "fail":
            # Kept at the top level, and kept sticky, because "what failed" must not
            # require walking `stages` and because the run stops at the stage that failed.
            status["failed_stage"] = stage
            status["failure"] = message
            status["remedy"] = event.get("remedy")

        transition = kind in ("start", "ok", "skip", "fail")
        row = next((row for row in status["stages"] if row["stage"] == stage), None)
        if row is None:
            return transition
        if message:
            row["message"] = message
        if kind == "progress":
            # Only `progress` moves the position. Every other kind carries `done: null` by
            # protocol, and letting a log line erase how far a fifty-minute run has got
            # would make "how far" flicker to unknown once a minute.
            row["done"] = event.get("done")
            row["total"] = event.get("total")
        elif kind == "start":
            row["state"] = "running"
            row["started"] = status["updated"]
            row["done"] = None
            row["total"] = None
        elif transition:
            row["state"] = kind
            row["ended"] = status["updated"]
            if row["started"] is None:
                row["started"] = status["updated"]
        status["state"] = row["state"]
        status["done"] = row["done"]
        status["total"] = row["total"]
        return transition

    def close(self) -> None:
        """This process is done, whatever the last event said.

        A stage still marked `running` was killed or died without a terminal event, and
        leaving it running in a file whose `live` is false would be a contradiction the
        reader has to resolve. Registered with `atexit`, so it also runs after a
        SystemExit, an unhandled traceback and a Ctrl-C; a process killed outright writes
        nothing at all, which is what the reader's `pid` check answers for.
        """
        if self.lines.closed:
            return
        now = now_ms()
        self.status["live"] = False
        self.status["ended"] = now
        self.status["updated"] = now
        for row in self.status["stages"]:
            if row["state"] == "running":
                row["state"] = "interrupted"
                row["ended"] = now
        if self.status["state"] == "running":
            self.status["state"] = "interrupted"
        self._publish()
        self.lines.close()

    def _publish(self) -> None:
        """Rewritten whole, through a temporary file and a rename, because a reader polling
        this must never catch half an object. `os.replace` is atomic on Windows."""
        self._published = self.status["updated"]
        temp = self.path.with_name(self.path.name + ".part")
        try:
            temp.write_text(
                json.dumps(self.status, ensure_ascii=False, indent=2), encoding="utf-8"
            )
            os.replace(temp, self.path)
        except OSError:
            # A run whose status cannot be published is still a run. The alternative is
            # killing an hour of GPU time over a locked file.
            pass


# ------------------------------------------------------------------ windows scheduling --
# Windows' own heuristic calls a windowless child of a console process "background" and
# EcoQoS-throttles it: reduced clock, and placement on an E-core. Measured on this machine,
# declining that throttle made engine synthesis 3.02x faster (4794 -> 1589 ms p50) and cut its
# spread from 33% to 3.3%, because that sampler is a single-threaded dispatch loop and clock
# and core choice are the whole cost.
#
# `generate_samples.py` runs that same loop in this process, so it pays the same 3x and this is
# the fix. `encode_latents.py` and `run_training.py` call it too: the first is GPU-compute bound
# and will gain far less, and the second does its work in the upstream trainer it SPAWNS - a
# separate process, which makes its own scheduling bed - so there the call only covers this
# launcher's own relay loop. One syscall each, and every step then reports the regime it ran
# under instead of leaving it to be inferred.
#
# The engine worker declines the same throttle for itself in `worker/irodori/worker.py`. That
# copy and this one are deliberate duplicates: `worker/` and `scripts/training/` are separate
# process families with separate sys.paths, and sharing a module across them would be a worse
# coupling than a repeated syscall. Change one, look at the other.

#: Set to `1` to keep Windows' throttle. Same switch name as the worker's, so there is one
#: across the product.
ECOQOS_ENV = "VC_ENGINE_ECOQOS"

# `ProcessPowerThrottling`, its current version, and the one bit of it that matters.
_POWER_THROTTLING_CLASS = 4
_POWER_THROTTLING_VERSION = 1
_EXECUTION_SPEED = 0x1


def decline_eco_qos(stage: str) -> None:
    """Ask Windows not to throttle this process, and report what it actually did.

    Reported rather than assumed. The first version of this call in the worker failed silently
    with ERROR_INVALID_HANDLE - ctypes had truncated the `GetCurrentProcess()` pseudo-handle to
    32 bits - and the read-back is the only reason anyone noticed. So this sets the state, reads
    it back out of the OS, and says both: what was asked for, and what is in force.
    """
    asked, got = _apply_eco_qos()
    emit(stage, "log", f"scheduling: qos_asked={asked} qos={got}")
    print(f"scheduling    qos_asked={asked}   qos={got}")


def _apply_eco_qos() -> tuple[str, str]:
    """`(what was asked for, what the OS reports now)`.

    Never raises: a training step does not fail because a scheduling hint did not take. Every
    way this can go wrong ends up in the second half of the pair, where the log will show it.
    """
    if sys.platform != "win32":
        return "skipped", "not windows"
    try:
        import ctypes
        from ctypes import wintypes

        class State(ctypes.Structure):
            _fields_ = [
                ("Version", wintypes.ULONG),
                ("ControlMask", wintypes.ULONG),
                ("StateMask", wintypes.ULONG),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        # Declared, not defaulted. ctypes assumes `int` for an undeclared restype, which
        # truncates the (HANDLE)-1 pseudo-handle to 0xFFFFFFFF, and every call then fails with
        # ERROR_INVALID_HANDLE - the exact bug the read-back exists to catch.
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        signature = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        kernel32.SetProcessInformation.argtypes = signature
        kernel32.SetProcessInformation.restype = wintypes.BOOL
        kernel32.GetProcessInformation.argtypes = signature
        kernel32.GetProcessInformation.restype = wintypes.BOOL
        process = kernel32.GetCurrentProcess()

        def in_force() -> str:
            state = State(Version=_POWER_THROTTLING_VERSION)
            ok = kernel32.GetProcessInformation(
                process, _POWER_THROTTLING_CLASS, ctypes.byref(state), ctypes.sizeof(state)
            )
            if not ok:
                return f"unknown (GetProcessInformation error {ctypes.get_last_error()})"
            if not state.ControlMask & _EXECUTION_SPEED:
                # No stated policy, so Windows' heuristic decides - which is what throttles a
                # windowless child in the first place.
                return "unset"
            return "throttle-on" if state.StateMask & _EXECUTION_SPEED else "throttle-off"

        if os.environ.get(ECOQOS_ENV) == "1":
            return f"unchanged ({ECOQOS_ENV}=1)", in_force()

        # ControlMask says which policy this process states; StateMask 0 says "never throttle
        # me" rather than "throttle me".
        asked = State(
            Version=_POWER_THROTTLING_VERSION, ControlMask=_EXECUTION_SPEED, StateMask=0
        )
        ok = kernel32.SetProcessInformation(
            process, _POWER_THROTTLING_CLASS, ctypes.byref(asked), ctypes.sizeof(asked)
        )
        if not ok:
            return "throttle-off", f"unchanged (SetProcessInformation error {ctypes.get_last_error()})"
        return "throttle-off", in_force()
    except (OSError, AttributeError) as exc:
        # A Windows old enough to lack SetProcessInformation (pre-1709) has no EcoQoS to
        # decline either.
        return "throttle-off", f"unavailable ({exc})"
