#!/usr/bin/env python3
"""Where the Irodori backend lives, and how to run it.

Every script beside this one needs the same handful of answers - engine source tree, model
cache, base checkpoint, codec weights, which interpreter - and the runtime already
committed to most of them (`docs/deployment.md`, `worker/irodori/worker.py`). Answering
them a second time, differently, is how a training tree drifts from the tree that will
actually speak, so the resolution order lives here once and the entry points consume it.

Backend-scoped on purpose: this directory is `training/irodori/` for the same reason the
worker is `worker/irodori/`. Every path, glob and default below describes ONE backend,
whose text encoder is Japanese. A backend for another language brings its own sibling
directory; nothing here is meant to grow a switch.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import _layout  # noqa: E402

# Layout questions that are not the backend's business are answered one level up.
install_root = _layout.install_root
utf8_stdout = _layout.utf8_stdout

# The two model repositories the Irodori backend cannot train without, in HuggingFace
# cache layout. The checkpoint glob is character-for-character the worker's
# (`worker/irodori/worker.py::_checkpoint`): "training found the checkpoint but the
# runtime did not" is not a failure mode worth having.
CHECKPOINT_GLOB = "models--Aratako--Irodori-TTS-v4.1-Small/snapshots/*/model.safetensors"
CODEC_GLOB = "models--Aratako--Semantic-DACVAE-Japanese-32dim/snapshots/*/weights.pth"

ENGINE_ROOT_ENV = "VOICE_CORE_ENGINE_ROOT"
ENGINE_PYTHON_ENV = "VOICE_CORE_ENGINE_PYTHON"

# Latent frame rate of the codec these scripts encode with, measured rather than assumed:
# 48000 Hz / hop 1920 = 25.0 frames per second, confirmed against a 69-clip manifest where
# num_frames == ceil(duration * 25) for every row. Used only to turn the trainer's frame
# budgets back into seconds for humans.
CODEC_SAMPLE_RATE = 48000
LATENT_FRAMES_PER_SECOND = 25.0


@dataclass(frozen=True)
class Engine:
    """One resolved Irodori install. Paths only - nothing here has imported torch."""

    root: Path
    hf_home: Path
    offline: bool
    python_override: Path | None = None

    @property
    def upstream(self) -> Path:
        """The upstream repository: `train.py`, `prepare_manifest.py`, `irodori_tts/`."""
        return self.root / "webui" / "Irodori-TTS"

    @property
    def dacvae(self) -> Path:
        """Vendored codec source. Imported as a sibling package, not pip-installed."""
        return self.root / "webui" / "dacvae"

    @property
    def hub(self) -> Path:
        return self.hf_home / "hub"

    def python_candidates(self) -> list[Path]:
        """Interpreters that could have the engine's dependencies, best first.

        Two provisioning styles both exist in the wild and both are legitimate: upstream's
        own `uv sync --extra cu128`, which creates `.venv` inside the cloned repo, and the
        separate venv a packaged install ships as `runtime/python` for the worker. Try the
        in-repo one first because it is the one upstream's instructions produce.
        """
        candidates: list[Path] = []
        if self.python_override is not None:
            candidates.append(self.python_override)
        if os.environ.get(ENGINE_PYTHON_ENV):
            candidates.append(Path(os.environ[ENGINE_PYTHON_ENV]))
        for base in (self.upstream / ".venv", install_root() / "runtime" / "python"):
            candidates.append(base / "Scripts" / "python.exe")  # Windows venv
            candidates.append(base / "bin" / "python")  # POSIX venv
        return candidates

    def python(self) -> Path:
        for candidate in self.python_candidates():
            if candidate.is_file():
                return candidate
        raise SystemExit(
            "no engine interpreter found. Looked for, in order:\n"
            + "".join(f"  {candidate}\n" for candidate in self.python_candidates())
            + "  Provision one first (scripts/bootstrap.ps1), or pass --python."
        )

    def checkpoint(self) -> Path:
        """The base weights `train.py --init-checkpoint` must be pointed at."""
        return self._one(CHECKPOINT_GLOB, "Irodori-TTS-v4.1-Small base checkpoint")

    def codec_weights(self) -> Path:
        """DACVAE weights, for reporting. Upstream's tooling loads the codec by repo id
        and finds these itself through the same cache."""
        return self._one(CODEC_GLOB, "Semantic-DACVAE-Japanese-32dim codec weights")

    def _one(self, glob: str, what: str) -> Path:
        found = sorted(self.hub.glob(glob))
        if not found:
            raise SystemExit(
                f"cannot find the {what}.\n"
                f"  looked for: {self.hub / glob}\n"
                "  Provision the weights first (scripts/bootstrap.ps1), or point at another\n"
                "  cache with --hf-home."
            )
        # Several matches means the cache holds several revisions; the newest snapshot
        # directory is the one a fresh download produced.
        return max(found, key=lambda path: path.parent.stat().st_mtime)

    def require_tree(self) -> None:
        missing = [path for path in (self.upstream, self.dacvae) if not path.is_dir()]
        if missing:
            raise SystemExit(
                "the engine source tree is incomplete:\n"
                + "".join(f"  MISSING {path}\n" for path in missing)
                + f"  engine root: {self.root}\n"
                "  Provision it first (scripts/bootstrap.ps1), or pass --engine-root."
            )

    def activate(self) -> None:
        """Apply that environment to THIS process and put the engine on `sys.path`.

        For the one script that imports the engine in-process instead of shelling out to
        upstream. MUST run before the first `import torch` / `import irodori_tts`, because
        both read these variables at import time.
        """
        self.require_tree()
        os.environ.update(
            {
                key: value
                for key, value in self.env().items()
                if key
                in {
                    "HF_HOME",
                    "HF_HUB_CACHE",
                    "HF_HUB_OFFLINE",
                    "TRANSFORMERS_OFFLINE",
                    "PYTHONUNBUFFERED",
                }
            }
        )
        if not self.offline:
            os.environ.pop("HF_HUB_OFFLINE", None)
            os.environ.pop("TRANSFORMERS_OFFLINE", None)
        for path in (self.dacvae, self.upstream):
            entry = str(path)
            if entry not in sys.path:
                sys.path.insert(0, entry)

    def require_own_interpreter(self) -> None:
        """Refuse to continue under an interpreter that has no torch.

        A stranger will run this with whatever `python` is on PATH, and the failure that
        produces is a bare `ModuleNotFoundError: torch` forty lines deep. Say what to type.
        """
        if importlib.util.find_spec("torch") is not None:
            return
        raise SystemExit(
            "this script imports the engine, so it needs the engine's Python.\n"
            f"  running under: {sys.executable}\n"
            "  use instead, whichever exists:\n"
            + "".join(f"    {candidate}\n" for candidate in self.python_candidates())
        )

    def env(self) -> dict[str, str]:
        """The environment upstream's scripts need. Assignment, not `setdefault`: a
        `--hf-home` on the command line has to beat a stale `HF_HOME` in the shell."""
        env = dict(os.environ)
        env["HF_HOME"] = str(self.hf_home)
        env["HF_HUB_CACHE"] = str(self.hub)
        if self.offline:
            # The weights are already local and total ~4.7 GB. Offline turns "silently
            # re-downloads the 1.3 GB text encoder over a metered link" into an error.
            # Verified to be enough on transformers 5.16.1 / huggingface_hub 1.29.0: it
            # overrides pristine upstream's `local_files_only=False`, PROVIDED the pinned
            # revision is the one in the cache.
            env["HF_HUB_OFFLINE"] = "1"
            env["TRANSFORMERS_OFFLINE"] = "1"
        else:
            env.pop("HF_HUB_OFFLINE", None)
            env.pop("TRANSFORMERS_OFFLINE", None)
        # tqdm buffers into a pipe and only flushes at exit, which hides a two-hour run's
        # entire progress until it is over.
        env["PYTHONUNBUFFERED"] = "1"
        return env

    def run_upstream(self, script: str, argv: list[str], *, log: Path | None = None) -> int:
        """Run one of upstream's entry points with that environment, from its own
        directory. Returns the exit status."""
        self.require_tree()
        target = self.upstream / script
        if not target.is_file():
            raise SystemExit(f"upstream script not found: {target}")
        command = [str(self.python()), str(target), *argv]
        if log is None:
            return subprocess.run(command, env=self.env(), cwd=str(self.upstream)).returncode
        log.parent.mkdir(parents=True, exist_ok=True)
        with log.open("a", encoding="utf-8", errors="replace") as handle:
            return subprocess.run(
                command,
                env=self.env(),
                cwd=str(self.upstream),
                stdout=handle,
                stderr=subprocess.STDOUT,
            ).returncode

    def command_line(self, script: str, argv: list[str]) -> str:
        interpreter = next(
            (str(c) for c in self.python_candidates() if c.is_file()), "<no interpreter>"
        )
        parts = [interpreter, str(self.upstream / script), *argv]
        return " ".join(f'"{part}"' if " " in part else part for part in parts)

    def describe(self) -> str:
        def mark(path: Path) -> str:
            return "" if path.exists() else "   MISSING"

        interpreter = next((c for c in self.python_candidates() if c.is_file()), None)
        return "\n".join(
            [
                f"  engine root   {self.root}",
                f"  upstream      {self.upstream}{mark(self.upstream)}",
                f"  codec source  {self.dacvae}{mark(self.dacvae)}",
                f"  model cache   {self.hf_home}{mark(self.hf_home)}",
                f"  hf offline    {'1' if self.offline else '0'}",
                f"  interpreter   {interpreter or 'NOT FOUND'}",
            ]
        )


def add_engine_args(parser: argparse.ArgumentParser) -> None:
    group = parser.add_argument_group("engine location")
    group.add_argument(
        "--engine-root",
        type=Path,
        default=None,
        help=(
            "Directory containing webui/Irodori-TTS and webui/dacvae. "
            f"Default: ${ENGINE_ROOT_ENV}, else <install root>/runtime/engine."
        ),
    )
    group.add_argument(
        "--hf-home",
        type=Path,
        default=None,
        help=(
            "HuggingFace cache root (the directory holding hub/). Default: $HF_HOME, else "
            "<install root>/models/huggingface, else <engine root>/model/huggingface."
        ),
    )
    group.add_argument(
        "--python",
        type=Path,
        default=None,
        help=(
            "Interpreter that has the engine's dependencies. Default: the engine repo's "
            f"own .venv, else <install root>/runtime/python, else ${ENGINE_PYTHON_ENV}."
        ),
    )
    group.add_argument(
        "--allow-hf-download",
        action="store_true",
        help=(
            "Leave HF_HUB_OFFLINE unset. Only needed when the cache is missing a revision "
            "the config pins and you accept the download."
        ),
    )


def resolve_engine(args: argparse.Namespace) -> Engine:
    root_path = install_root()
    if getattr(args, "engine_root", None) is not None:
        engine_root = Path(args.engine_root).expanduser().resolve()
    elif os.environ.get(ENGINE_ROOT_ENV):
        engine_root = Path(os.environ[ENGINE_ROOT_ENV]).expanduser().resolve()
    else:
        engine_root = root_path / "runtime" / "engine"

    if getattr(args, "hf_home", None) is not None:
        hf_home = Path(args.hf_home).expanduser().resolve()
    elif os.environ.get("HF_HOME"):
        hf_home = Path(os.environ["HF_HOME"]).expanduser().resolve()
    else:
        packaged = root_path / "models" / "huggingface"
        # The packaged location wins when it exists; the in-tree one is the fallback,
        # which is the same precedence the worker applies.
        hf_home = packaged if packaged.is_dir() else engine_root / "model" / "huggingface"

    override = getattr(args, "python", None)
    return Engine(
        root=engine_root,
        hf_home=hf_home,
        offline=not getattr(args, "allow_hf_download", False),
        python_override=Path(override).expanduser() if override is not None else None,
    )
