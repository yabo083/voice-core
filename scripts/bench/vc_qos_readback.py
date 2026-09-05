"""Read the Windows power-throttling (EcoQoS) policy in force for any process, by pid.

For Main's end-to-end smoke: `python vc_qos_readback.py <pid> [<pid> ...]`, or with no
arguments it finds voice-core's three processes itself.

The one trap, and it is the trap that cost me a wrong result first time round: ctypes'
default `restype` is `c_int`. `OpenProcess` returns a 64-bit HANDLE and
`GetCurrentProcess` returns the pseudo-handle (HANDLE)-1, and a 32-bit c_int truncates
both - after which every call fails with ERROR_INVALID_HANDLE (6) and, if you are not
reading the state back, looks exactly like a process Windows declined to throttle. Declare
the types.

Output per process:
  throttle-off        we (or someone) explicitly declared "never run me at efficiency
                      speed" - this is what worker.py now sets, and what a fixed presenter
                      should read
  throttle-on         explicitly throttled on purpose
  windows-heuristic   ControlMask is 0: nobody stated a policy, so Windows decides, and for
                      a windowless child of a console process it decides EcoQoS. This is the
                      3x-slower state.
"""
import ctypes
import sys
from ctypes import wintypes

PROCESS_POWER_THROTTLING = 4  # PROCESS_INFORMATION_CLASS
THROTTLING_VERSION = 1  # PROCESS_POWER_THROTTLING_CURRENT_VERSION
EXECUTION_SPEED = 0x1  # PROCESS_POWER_THROTTLING_EXECUTION_SPEED
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000


class PowerThrottlingState(ctypes.Structure):
    _fields_ = [
        ("Version", wintypes.ULONG),
        ("ControlMask", wintypes.ULONG),
        ("StateMask", wintypes.ULONG),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.GetProcessInformation.argtypes = [
    wintypes.HANDLE,
    ctypes.c_int,
    ctypes.c_void_p,
    wintypes.DWORD,
]
kernel32.GetProcessInformation.restype = wintypes.BOOL


def read_qos(pid: int) -> str:
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return f"unreadable:OpenProcess err={ctypes.get_last_error()}"
    try:
        state = PowerThrottlingState(Version=THROTTLING_VERSION)
        if not kernel32.GetProcessInformation(
            handle, PROCESS_POWER_THROTTLING, ctypes.byref(state), ctypes.sizeof(state)
        ):
            return f"unreadable:err={ctypes.get_last_error()}"
    finally:
        kernel32.CloseHandle(handle)
    if not state.ControlMask & EXECUTION_SPEED:
        return "windows-heuristic"
    return "throttle-off" if not state.StateMask & EXECUTION_SPEED else "throttle-on"


# What each of voice-core's four processes should read, and why. Recorded here because this
# file is where somebody re-checks it after everyone who decided it has moved on.
#
#   engine worker  throttle-off        MEASURED 3x. A single-threaded ATen dispatch loop;
#                                      an E-core at reduced clock is the whole cost.
#   presenter      throttle-off        Timing-sensitive and the worst-shaped of the four:
#                                      a mostly-windowless child doing typewriter reveal,
#                                      growth animation and a low-level mouse hook.
#   runtime        (either is fine)    Supervises and proxies; its latency is the worker's.
#   panel          windows-heuristic   Deliberately NOT declared. A config window Windows
#                                      parks on an E-core while nobody is looking at it is
#                                      behaving correctly, and its one latency-visible
#                                      action is spent waiting on the runtime anyway.
_EXPECTED = {
    "engine worker": "throttle-off",
    "presenter": "throttle-off",
    "runtime": "either",
    "panel": "windows-heuristic",
    # The venv launcher (see `discover`): it runs none of our code, so it declares nothing and
    # there is nothing for it to declare. Listed rather than hidden, because a reader counting
    # processes should see every python that exists.
    "worker launcher": "-",
}


def role_of(image: str, cmdline: str) -> str:
    """The role, decided by the image name, never by a role list keyed on a substring.

    This function exists because the first version of it guessed: it matched `voicecore` in
    the image name and labelled the Tauri PANEL as the presenter, which is the one mislabel
    that turns this tool into a source of wrong conclusions rather than a check on them. The
    two are separate exes - `VoiceCore.exe` is the panel from `manager/src-tauri`, and
    `VoiceCorePresenter.exe` is what `app/VoiceCoreTray` builds, renamed and spawned
    `--presenter --no-runtime`. The runtime and the worker are likewise two processes, and
    today only one of them declares anything.
    """
    lowered = image.lower()
    if lowered == "voice-core-runtime.exe":
        return "runtime"
    if lowered == "voicecorepresenter.exe":
        return "presenter"
    if lowered == "voicecore.exe":
        return "panel"
    if "worker.py" in cmdline and "irodori" in cmdline:
        return "engine worker"
    return "unrecognised"


def discover() -> list[tuple[int, str, str]]:
    """voice-core's processes: runtime, engine worker, presenter, panel - and the shim.

    The worker arrives as TWO processes, not one, and only one of them is ours to declare a
    policy on. `runtime/python/Scripts/python.exe` is a venv launcher: it re-executes the real
    interpreter (on this machine one uv manages, outside the install tree) with the SAME argv
    and then waits. So both match on `worker.py` + `irodori`, both look like the worker, and
    the launcher reads `windows-heuristic` forever because no Python of ours ever runs in it.

    Reported as `worker launcher` with nothing expected. The alternative - leaving it as a
    second `engine worker` row - flags a MISMATCH on every healthy stack, and a check that
    cries wolf on its first real run is a check people learn to ignore.
    """
    import psutil

    found = []
    parents: dict[int, int] = {}
    for proc in psutil.process_iter(["pid", "name", "cmdline", "ppid"]):
        try:
            image = proc.info["name"] or "?"
            role = role_of(image, " ".join(proc.info["cmdline"] or []))
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
        if role != "unrecognised":
            found.append((proc.info["pid"], role, image))
            parents[proc.info["pid"]] = proc.info["ppid"] or 0

    # A worker whose parent is also a worker is the real one; the parent is the launcher.
    workers = {pid for pid, role, _ in found if role == "engine worker"}
    shims = {parents[pid] for pid in workers if parents.get(pid) in workers}
    return [
        (pid, "worker launcher" if pid in shims else role, image) for pid, role, image in found
    ]


def named(pid: int) -> tuple[int, str, str]:
    """One explicitly-passed pid, still named from its own image rather than trusted."""
    try:
        import psutil

        proc = psutil.Process(pid)
        image = proc.name()
        return pid, role_of(image, " ".join(proc.cmdline() or [])), image
    except Exception:
        return pid, "unrecognised", "?"


targets = [named(int(arg)) for arg in sys.argv[1:]] if len(sys.argv) > 1 else discover()
if not targets:
    raise SystemExit("no voice-core processes found; pass pids explicitly")
print(f"{'role':15s} {'image':26s} {'pid':>7s}  {'in force':18s} expected")
for pid, role, image in sorted(targets, key=lambda row: row[1]):
    state = read_qos(pid)
    expected = _EXPECTED.get(role, "-")
    verdict = "" if expected in (state, "either", "-") else "   <-- MISMATCH"
    print(f"{role:15s} {image:26s} {pid:>7d}  {state:18s} {expected}{verdict}")
