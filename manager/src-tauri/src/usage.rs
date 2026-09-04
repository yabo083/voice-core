//! What the stack is actually costing: process memory and GPU memory, measured
//! locally rather than reported by the runtime.
//!
//! `/api/status` deliberately carries no memory figures - the runtime does not know
//! them. The engine's VRAM lives in a Python process it spawned, and its RSS is an OS
//! fact about a process tree. Asking the runtime to proxy both would put a monitoring
//! agent inside a synthesis service; the panel is already the process supervisor, so
//! it is the honest place to measure.
//!
//! Two sources, both cheap:
//! - Working set per PID via `GetProcessMemoryInfo`, over the tree this app owns
//!   (runtime, its Python child, the presenter, and this window itself).
//! - `nvidia-smi --query-compute-apps`, which reports VRAM *per process*, so the
//!   engine's share is a measurement and not the whole card's usage attributed to us.
//!   No NVIDIA driver, no numbers: the fields go null and the UI says so.

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::host::Host;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// None when nvidia-smi is absent or unreadable.
    pub gpu_name: Option<String>,
    pub gpu_used_mib: Option<u64>,
    pub gpu_total_mib: Option<u64>,
    /// VRAM held by this stack's processes, not by the whole card.
    pub engine_gpu_mib: Option<u64>,
    pub rss_runtime_mib: u64,
    pub rss_engine_mib: u64,
    pub rss_presenter_mib: u64,
    pub rss_manager_mib: u64,
}

#[tauri::command]
pub async fn resource_usage(app: AppHandle) -> Usage {
    let host = app.state::<Host>();
    let pids = crate::supervise::pids(&host);
    let mut usage = Usage::default();

    #[cfg(windows)]
    {
        let processes = snapshot();
        let mine = std::process::id();

        // Every descendant of the runtime, not just its children. The engine is a
        // Python process the runtime starts, but torch reaches CUDA through further
        // processes of its own, and one of those is where the model actually lives -
        // counting only the first level reported 13 MiB of RSS and no VRAM at all for a
        // loaded model.
        let engine: Vec<u32> = match pids.runtime {
            Some(runtime) => descendants(&processes, runtime),
            None => Vec::new(),
        };

        usage.rss_manager_mib = rss_mib(&[mine]);
        usage.rss_runtime_mib = rss_mib(pids.runtime.as_slice());
        usage.rss_presenter_mib = rss_mib(pids.presenter.as_slice());
        usage.rss_engine_mib = rss_mib(&engine);

        let mut ours: Vec<u32> = engine;
        ours.extend(pids.runtime);
        ours.extend(pids.presenter);
        ours.push(mine);
        if let Some(gpu) = query_gpu(&ours) {
            usage.gpu_name = gpu.name;
            usage.gpu_used_mib = gpu.used;
            usage.gpu_total_mib = gpu.total;
            usage.engine_gpu_mib = gpu.mine;
        }
    }

    usage
}

/// Transitive children of `root`, breadth-first.
///
/// Bounded by the number of processes on the machine, and each PID is visited once, so
/// a parent cycle from PID reuse cannot loop.
#[cfg(windows)]
fn descendants(processes: &[Proc], root: u32) -> Vec<u32> {
    let mut found: Vec<u32> = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for process in processes {
            if process.parent != parent || process.pid == root || found.contains(&process.pid) {
                continue;
            }
            found.push(process.pid);
            frontier.push(process.pid);
        }
    }
    found
}

#[cfg(windows)]
struct Proc {
    pid: u32,
    parent: u32,
}

/// One walk of the process table. Cheaper and more reliable than repeated parent
/// lookups, which have no API that answers for an arbitrary PID.
#[cfg(windows)]
fn snapshot() -> Vec<Proc> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    // SAFETY: the handle is checked before use and closed on every exit path.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                out.push(Proc {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                });
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Working set, which is the number Task Manager shows and therefore the number a
/// user can check this panel against.
#[cfg(windows)]
fn rss_mib(pids: &[u32]) -> u64 {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut total: u64 = 0;
    for &pid in pids {
        // SAFETY: a failed open yields a null handle, which is checked; the counters
        // struct is zeroed and its size is passed exactly as the API requires.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                continue;
            }
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(handle, &mut counters, size) != 0 {
                total += counters.WorkingSetSize as u64;
            }
            CloseHandle(handle);
        }
    }
    total / (1024 * 1024)
}

#[cfg(windows)]
#[derive(Default)]
struct Gpu {
    name: Option<String>,
    used: Option<u64>,
    total: Option<u64>,
    mine: Option<u64>,
}

/// Two nvidia-smi queries: the card, and the per-process compute footprint.
///
/// A subprocess per poll is acceptable at the panel's 2 s cadence and buys exactness -
/// the alternative, linking NVML, would add a hard dependency on a driver DLL that a
/// machine without an NVIDIA card does not have, for a number that is decoration when
/// the engine is idle.
#[cfg(windows)]
fn query_gpu(ours: &[u32]) -> Option<Gpu> {
    let card = nvidia_smi(&[
        "--query-gpu=name,memory.used,memory.total",
        "--format=csv,noheader,nounits",
    ])?;
    let mut gpu = Gpu::default();
    if let Some(line) = card.lines().next() {
        let mut fields = line.split(',').map(str::trim);
        gpu.name = fields.next().map(str::to_string);
        gpu.used = fields.next().and_then(|v| v.parse().ok());
        gpu.total = fields.next().and_then(|v| v.parse().ok());
    }

    // Per-process VRAM is a driver privilege, not a given. A GeForce in WDDM mode - the
    // normal case on a desktop - answers `[N/A]` for every row, so a sum of the rows it
    // did parse would report 0 GiB for a model that is plainly resident. `mine` stays
    // None unless at least one row carried a real number, and None means "the driver
    // will not break it down", which the panel says instead of showing a false zero.
    if let Some(apps) = nvidia_smi(&[
        "--query-compute-apps=pid,used_gpu_memory",
        "--format=csv,noheader,nounits",
    ]) {
        let mut mine = 0u64;
        let mut reported = false;
        for line in apps.lines() {
            let mut fields = line.split(',').map(str::trim);
            let Some(pid) = fields.next().and_then(|v| v.parse::<u32>().ok()) else {
                continue;
            };
            let Some(mib) = fields.next().and_then(|v| v.parse::<u64>().ok()) else {
                continue;
            };
            reported = true;
            if ours.contains(&pid) {
                mine += mib;
            }
        }
        if reported {
            gpu.mine = Some(mine);
        }
    }
    Some(gpu)
}

#[cfg(windows)]
fn nvidia_smi(args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    // No console window: this runs behind a GUI, several times a minute.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("nvidia-smi")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}
