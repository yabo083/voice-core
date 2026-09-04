#!/usr/bin/env python3
"""Where the install tree is, and where its writable state lives.

Engine-agnostic on purpose: an install root and a data directory are properties of
voice-core, not of whichever backend makes the sound. `irodori/_engine.py` builds on this;
`install_pack.py` needs only this.
"""
from __future__ import annotations

import os
import sys
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
