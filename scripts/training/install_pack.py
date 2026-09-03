#!/usr/bin/env python3
"""Install a finished voice pack and register it - the last step, the one that makes it speak.

Two jobs, and the second is the delicate half:

1. Copy the artefact to `<data dir>/voicepacks/<id>/`, one directory per pack whatever the
   kind. A LoRA adapter's files go in it; a speaker embedding or a reference clip goes in it
   under its ORIGINAL file name, because for embeddings the `.speaker.safetensors` suffix is
   not decoration - the engine refuses a file without it, by name
   (`irodori_tts/speaker_inversion.py::load_speaker_inversion_payload`).

2. Add one entry to `voicePacks` in `<data dir>/config.json` WITHOUT touching anything else
   in that file. That file is JSONC written for a human to read in Notepad: it carries
   comments explaining every key, and the tray is its other writer. So this splices the
   array surgically - comments, key order, trailing commas, line endings and any byte-order
   mark all survive - rather than parse-and-rewrite, which would silently delete every
   comment in it.

The runtime re-reads that section whenever the file's mtime changes
(`src/packs.rs::reload_if_changed`, called on every voices listing and every pack lookup in
`src/service.rs`), so nothing needs restarting: the pack is speakable as soon as this
finishes.

Engine-agnostic, unlike its siblings in `irodori/`: a pack is a kind, a path, an engine name
and a language list, and `--engine` / `--languages` are exactly the keys a second backend
would be routed by.

    install_pack.py --pack corpus/my-voice/lora/checkpoint_best_val_loss_0001000_0.885155 \\
                    --id my-voice --name "My Voice (LoRA)" --character "My Voice"

`--dry-run` prints what it would copy and the exact JSON it would insert, and changes nothing.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _layout  # noqa: E402

SPEAKER_SUFFIX = ".speaker.safetensors"
AUDIO_SUFFIXES = {".wav", ".flac", ".ogg", ".opus", ".mp3"}
# Optimizer and dataloader state for `--resume`. 200+ MB per checkpoint and of no use to
# inference, which needs only adapter_config.json plus adapter_model.safetensors
# (`irodori_tts/lora.py:263-269`).
TRAINER_STATE = "trainer_state.pt"
ID_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")

CODE, STRING, COMMENT = "c", "s", "#"


# --------------------------------------------------------------------------------------
# JSONC, the dialect the runtime accepts (`src/jsonc.rs`): // and /* */ comments and one
# trailing comma. Classifying every character once means the structural passes below never
# have to think about a brace inside a string or a bracket inside a comment.
# --------------------------------------------------------------------------------------


def classify(text: str) -> list[str]:
    """One marker per character: code, inside a string literal, or inside a comment."""
    marks = [CODE] * len(text)
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char == '"':
            marks[index] = STRING
            index += 1
            while index < length:
                marks[index] = STRING
                if text[index] == "\\":
                    # An escape consumes exactly the next character, so "\\" ends the
                    # string and "\"" does not.
                    if index + 1 < length:
                        marks[index + 1] = STRING
                    index += 2
                    continue
                if text[index] == '"':
                    index += 1
                    break
                index += 1
            continue
        if char == "/" and index + 1 < length and text[index + 1] == "/":
            while index < length and text[index] != "\n":
                marks[index] = COMMENT
                index += 1
            continue
        if char == "/" and index + 1 < length and text[index + 1] == "*":
            marks[index] = marks[index + 1] = COMMENT
            index += 2
            while index < length:
                marks[index] = COMMENT
                if text[index] == "*" and index + 1 < length and text[index + 1] == "/":
                    marks[index + 1] = COMMENT
                    index += 2
                    break
                index += 1
            continue
        index += 1
    return marks


def to_json(text: str) -> str:
    """Comments to whitespace, trailing commas dropped, so `json.loads` can read it.
    Newlines survive, so a parse error still points at the line the user is looking at."""
    marks = classify(text)
    spaced = "".join(
        ("\n" if char == "\n" else " ") if mark == COMMENT else char
        for char, mark in zip(text, marks)
    )
    marks = classify(spaced)
    out: list[str] = []
    for index, char in enumerate(spaced):
        if char == "," and marks[index] == CODE:
            cursor = index + 1
            while cursor < len(spaced) and spaced[cursor].isspace():
                cursor += 1
            if cursor < len(spaced) and spaced[cursor] in "}]":
                continue
        out.append(char)
    return "".join(out).lstrip("\ufeff")


def match_bracket(text: str, marks: list[str], start: int) -> int:
    """Index of the bracket closing the one at `start`."""
    opener = text[start]
    closer = {"[": "]", "{": "}"}[opener]
    depth = 0
    for index in range(start, len(text)):
        if marks[index] != CODE:
            continue
        if text[index] == opener:
            depth += 1
        elif text[index] == closer:
            depth -= 1
            if depth == 0:
                return index
    raise SystemExit(f"config.json has an unbalanced {opener!r}; fix it before installing")


def find_key_array(text: str, marks: list[str], key: str) -> tuple[int, int] | None:
    """Span of the `[...]` belonging to `"key":`, brackets included."""
    token = f'"{key}"'
    position = -1
    while True:
        position = text.find(token, position + 1)
        if position < 0:
            return None
        if marks[position] != STRING:
            continue
        cursor = position + len(token)
        while cursor < len(text) and (text[cursor].isspace() or marks[cursor] == COMMENT):
            cursor += 1
        if cursor >= len(text) or text[cursor] != ":":
            continue
        cursor += 1
        while cursor < len(text) and (text[cursor].isspace() or marks[cursor] == COMMENT):
            cursor += 1
        if cursor < len(text) and text[cursor] == "[":
            return cursor, match_bracket(text, marks, cursor)
        return None


def split_elements(text: str, marks: list[str], open_at: int, close_at: int) -> list[tuple[int, int]]:
    """Spans of a container's elements, separators and surrounding whitespace excluded."""
    spans: list[tuple[int, int]] = []
    depth = 0
    start: int | None = None
    for index in range(open_at + 1, close_at):
        mark = marks[index]
        if mark == COMMENT:
            continue
        char = text[index]
        if mark == STRING:
            # A string at depth 0 starts an element (an object member's key, or a bare
            # string element); inside one it is just content.
            if start is None and depth == 0:
                start = index
            continue
        if char in "[{":
            if start is None:
                start = index
            depth += 1
        elif char in "]}":
            depth -= 1
        elif char == "," and depth == 0:
            if start is not None:
                spans.append((start, index))
                start = None
        elif start is None and not char.isspace():
            start = index
    if start is not None:
        spans.append((start, close_at))
    trimmed = [(begin, rstrip_to(text, end)) for begin, end in spans]
    return [(begin, end) for begin, end in trimmed if end > begin]


def rstrip_to(text: str, end: int) -> int:
    while end > 0 and text[end - 1].isspace():
        end -= 1
    return end


def render_entry(entry: dict, indent: str, newline: str) -> str:
    """The entry as JSON, indented for insertion. The first line carries no indent: every
    caller writes that itself, because it is already in the file at the insertion point."""
    body = json.dumps(entry, ensure_ascii=False, indent=2)
    return newline.join(indent + line for line in body.splitlines()).lstrip()


def register(config_text: str, entry: dict) -> tuple[str, str]:
    """Return the new file text and a one-line description of what changed."""
    newline = "\r\n" if "\r\n" in config_text else "\n"
    marks = classify(config_text)
    found = find_key_array(config_text, marks, "voicePacks")

    if found is None:
        # No registry yet: add the whole key just inside the root object.
        root_open = next(
            (i for i, char in enumerate(config_text) if char == "{" and marks[i] == CODE), None
        )
        if root_open is None:
            raise SystemExit("config.json has no top-level object; refusing to guess")
        root_close = match_bracket(config_text, marks, root_open)
        members = split_elements(config_text, marks, root_open, root_close)
        separator = "," if members else ""
        block = (
            f'{separator}{newline}{newline}  "voicePacks": [{newline}'
            f"    {render_entry(entry, '    ', newline)}{newline}  ]{newline}"
        )
        cut = rstrip_to(config_text, root_close)
        return (
            config_text[:cut] + block + config_text[root_close:],
            'added a "voicePacks" section',
        )

    open_at, close_at = found
    spans = split_elements(config_text, marks, open_at, close_at)

    # Element indentation: copy whatever the file already uses.
    indent = "    "
    if spans:
        line_start = config_text.rfind("\n", 0, spans[0][0]) + 1
        candidate = config_text[line_start : spans[0][0]]
        if not candidate.strip():
            indent = candidate

    for begin, end in spans:
        chunk = config_text[begin:end]
        try:
            parsed = json.loads(to_json(chunk))
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict) and parsed.get("id") == entry["id"]:
            return (
                config_text[:begin] + render_entry(entry, indent, newline) + config_text[end:],
                f"replaced the existing entry for id {entry['id']!r}",
            )

    block = render_entry(entry, indent, newline)
    if spans:
        last_end = spans[-1][1]
        return (
            config_text[:last_end] + f",{newline}{indent}{block}" + config_text[last_end:],
            f"appended entry {entry['id']!r} to voicePacks",
        )
    return (
        config_text[: open_at + 1]
        + f"{newline}{indent}{block}{newline}  "
        + config_text[close_at:],
        f"added entry {entry['id']!r} to the empty voicePacks array",
    )


# --------------------------------------------------------------------------------------
# Pack installation
# --------------------------------------------------------------------------------------


def detect_kind(pack: Path) -> str:
    if pack.is_dir():
        if (pack / "adapter_config.json").is_file():
            return "lora-adapter"
        raise SystemExit(
            f"{pack} is a directory but not a LoRA adapter (no adapter_config.json).\n"
            "  If that is a training output directory, point --pack at the checkpoint inside\n"
            "  it rather than at the run."
        )
    if pack.name.endswith(SPEAKER_SUFFIX):
        return "speaker-embedding"
    if pack.suffix.lower() in AUDIO_SUFFIXES:
        return "reference-audio"
    if pack.suffix.lower() == ".safetensors":
        raise SystemExit(
            f"{pack.name} looks like a speaker embedding but does not end in {SPEAKER_SUFFIX!r}.\n"
            "  The engine rejects it by name: \"Speaker Inversion embeddings must use the\n"
            "  '.speaker.safetensors' suffix\". Rename it back rather than registering it."
        )
    raise SystemExit(f"cannot tell what kind of pack {pack} is; pass --kind explicitly")


def plan_files(pack: Path, kind: str, keep_trainer_state: bool) -> list[Path]:
    if kind != "lora-adapter":
        return [pack]
    return [
        item
        for item in sorted(pack.iterdir())
        # Nested directories are other checkpoints of the same run, not part of this adapter.
        if item.is_file() and (keep_trainer_state or item.name != TRAINER_STATE)
    ]


def main() -> None:
    _layout.utf8_stdout()
    parser = argparse.ArgumentParser(
        description="Copy a voice pack into the data directory and register it in config.json."
    )
    parser.add_argument(
        "--pack",
        type=Path,
        required=True,
        help="LoRA adapter directory, a .speaker.safetensors file, or a reference audio file.",
    )
    parser.add_argument("--id", required=True, help="Registry id, used by the API and the CLI.")
    parser.add_argument("--name", default=None, help="Display name. Default: the id.")
    parser.add_argument(
        "--kind",
        default=None,
        choices=["lora-adapter", "speaker-embedding", "reference-audio"],
        help="Override the kind. Detected from the artefact by default.",
    )
    parser.add_argument(
        "--languages",
        nargs="+",
        default=["ja"],
        help=(
            "Languages this pack speaks (default ja: the Irodori backend's text encoder is "
            "Japanese). With --engine, this is how a second backend would be routed to."
        ),
    )
    parser.add_argument(
        "--engine",
        default="irodori-tts-v4.1-small",
        help="Engine that can play this pack (default irodori-tts-v4.1-small).",
    )
    parser.add_argument("--character", default=None, help="Speaker name a dialog frontend shows.")
    parser.add_argument(
        "--avatar", default=None, help="Portrait path, relative to the data directory."
    )
    parser.add_argument("--data-dir", type=Path, default=None, help="Override the data directory.")
    parser.add_argument(
        "--keep-trainer-state",
        action="store_true",
        help=(
            f"Copy {TRAINER_STATE} too. It is 200+ MB of optimizer state that inference never "
            "reads; keep it only to --resume this run from the pack later."
        ),
    )
    parser.add_argument(
        "--force", action="store_true", help="Overwrite an existing pack directory."
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="Print the plan and the JSON entry, change nothing."
    )
    args = parser.parse_args()

    if not ID_PATTERN.match(args.id):
        raise SystemExit(
            f"--id {args.id!r} must be a plain name (letters, digits, dot, dash, underscore): "
            "it becomes a directory name and an API identifier."
        )
    pack = args.pack.expanduser().resolve()
    if not pack.exists():
        raise SystemExit(f"--pack does not exist: {pack}")
    kind = args.kind or detect_kind(pack)
    if kind == "speaker-embedding" and not pack.name.endswith(SPEAKER_SUFFIX):
        raise SystemExit(
            f"a speaker-embedding pack must be a file ending in {SPEAKER_SUFFIX!r}; got {pack.name}"
        )
    if kind == "lora-adapter" and not pack.is_dir():
        raise SystemExit(f"a lora-adapter pack must be a directory; got {pack}")

    data_dir = _layout.resolve_data_dir(args.data_dir)
    config_file = data_dir / "config.json"
    target = data_dir / "voicepacks" / args.id
    if target.exists() and not (args.force or args.dry_run):
        raise SystemExit(f"{target} already exists; pass --force to overwrite it")

    files = plan_files(pack, kind, args.keep_trainer_state)
    size = sum(item.stat().st_size for item in files)
    entry: dict = {"id": args.id, "name": args.name or args.id}
    if args.character:
        entry["character"] = args.character
    if args.avatar:
        entry["avatar"] = args.avatar
    entry["languages"] = list(args.languages)
    entry["kind"] = kind
    # Relative to the data directory, with forward slashes, so the tree stays portable and
    # the JSON carries no backslashes for a human to mis-escape.
    entry["path"] = (
        f"voicepacks/{args.id}" if kind == "lora-adapter" else f"voicepacks/{args.id}/{pack.name}"
    )
    entry["engine"] = args.engine

    print(f"data dir   {data_dir}")
    print(f"pack       {pack}")
    print(f"kind       {kind}")
    print(f"copy to    {target}   {len(files)} file(s), {size / 1048576:.1f} MiB")
    if kind == "lora-adapter" and not args.keep_trainer_state:
        state = pack / TRAINER_STATE
        if state.is_file():
            print(
                f"skipping   {TRAINER_STATE} ({state.stat().st_size / 1048576:.0f} MiB, resume-only)"
            )
    print("entry:")
    for line in json.dumps(entry, ensure_ascii=False, indent=2).splitlines():
        print(f"  {line}")

    if not config_file.is_file():
        raise SystemExit(
            f"no config file at {config_file}\n"
            "  Start the runtime once so it creates the data directory, or pass --data-dir."
        )
    raw_bytes = config_file.read_bytes()
    # A byte-order mark is preserved when the file already has one and never added: the tray
    # and Notepad both write this file, and a surgical edit has no business changing its
    # encoding underneath them. The runtime strips one either way (`src/jsonc.rs`).
    bom = "\ufeff" if raw_bytes.startswith(b"\xef\xbb\xbf") else ""
    updated, what = register(raw_bytes.decode("utf-8-sig"), entry)

    # Refuse to write something the runtime could not read: this file is how it finds every
    # voice, and a broken edit would take out the packs that already worked.
    try:
        reparsed = json.loads(to_json(updated))
    except json.JSONDecodeError as exc:
        raise SystemExit(f"the edited config would not parse ({exc}); nothing was written") from exc
    ids = [item.get("id") for item in reparsed.get("voicePacks", [])]
    if args.id not in ids:
        raise SystemExit("internal error: the edited config does not contain the new entry")

    if args.dry_run:
        print(f"dry run    would have {what}; voicePacks would be {ids}")
        return

    if target.exists():
        if target.is_dir():
            shutil.rmtree(target)
        else:
            target.unlink()
    target.mkdir(parents=True, exist_ok=True)
    for item in files:
        shutil.copy2(item, target / item.name)
    config_file.write_text(bom + updated, encoding="utf-8", newline="")

    print(f"copied     {size / 1048576:.1f} MiB -> {target}")
    print(f"config     {what}   ({config_file})")
    print(f"voices     {ids}")
    print(
        "ready      the runtime re-reads voicePacks on mtime change, so no restart. "
        "Check with: voice-core.exe voices"
    )


if __name__ == "__main__":
    main()
