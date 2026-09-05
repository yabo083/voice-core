//! What the stack is actually costing: process memory and GPU memory, measured
//! locally rather than reported by the runtime.
//!
//! `/api/status` deliberately carries no memory figures - the runtime does not know
//! them. The engine's VRAM lives in a Python process it spawned, and its RSS is an OS
//! fact about a process tree. Asking the runtime to proxy both would put a monitoring
//! agent inside a synthesis service; the panel is already the process supervisor, so
//! it is the honest place to measure.
//!
//! Three sources, all cheap:
//! - Working set per PID via `GetProcessMemoryInfo`, over the tree this app owns
//!   (runtime, its Python child, the presenter, and this window itself).
//! - `nvidia-smi --query-gpu`, for the card itself: its name, and how much of it
//!   everything on the machine is using. No NVIDIA driver, no numbers: the fields go null
//!   and the UI says so.
//! - The `GPU Process Memory` performance counters, for this stack's own share of the
//!   VRAM. `nvidia-smi --query-compute-apps` cannot answer that on a GeForce (see
//!   `pdh_dedicated_mib`), and these counters are what Task Manager reads, so the panel's
//!   number is one a user can check.

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
        if let Some(gpu) = query_gpu() {
            usage.gpu_name = gpu.name;
            usage.gpu_used_mib = gpu.used;
            usage.gpu_total_mib = gpu.total;
        }
        // Deliberately not folded into the query above: the card's totals come from the
        // NVIDIA driver and our share comes from the Windows kernel, so a machine can
        // have either without the other, and neither absence may hide the other's number.
        usage.engine_gpu_mib = pdh_dedicated_mib(&ours);
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
}

/// The card: what it is, and how much of it is in use by everything.
///
/// A subprocess per poll is acceptable at the panel's 2 s cadence and buys exactness -
/// the alternative, linking NVML, would add a hard dependency on a driver DLL that a
/// machine without an NVIDIA card does not have, for a number that is decoration when
/// the engine is idle.
#[cfg(windows)]
fn query_gpu() -> Option<Gpu> {
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
    Some(gpu)
}

/// VRAM held by `ours`, in MiB, from the counters Task Manager itself reads.
///
/// Per-process VRAM is a driver privilege that a GeForce in WDDM mode - the normal case on
/// a desktop - does not grant: `nvidia-smi --query-compute-apps` answers `[N/A]` for every
/// row on this machine, so summing what it does parse reports 0 GiB for a model that is
/// plainly resident. The Windows kernel knows the answer anyway, because in WDDM it is the
/// kernel that owns the video memory manager, and it publishes one
/// `\GPU Process Memory(pid_<PID>_luid_<hi>_<lo>_phys_<N>)\Dedicated Usage` instance per
/// process per adapter segment. That counter is the "Dedicated GPU memory" column in Task
/// Manager's Details view.
///
/// One wildcard instance and one collection: `Dedicated Usage` is an instantaneous gauge,
/// not a rate, so it needs no second sample to become meaningful. `None` means the
/// counters were not there to read, which the panel says instead of showing a false zero.
#[cfg(windows)]
fn pdh_dedicated_mib(ours: &[u32]) -> Option<u64> {
    use windows_sys::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE,
        PDH_MORE_DATA,
    };

    // English names, so a localised Windows resolves the same path: the counter set is
    // "GPU 进程内存" on this machine and `PdhAddEnglishCounterW` is the API that does not
    // care. The instance is a wildcard because the real names carry the adapter LUID and
    // segment index, which we neither know nor need.
    let path: Vec<u16> = "\\GPU Process Memory(*)\\Dedicated Usage"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: the query handle is closed on every path out; both array calls are the
    // documented size-then-fill pair, and the second is only reached after the first
    // asked for a buffer, so `count` never exceeds what was allocated.
    unsafe {
        let mut query: isize = 0;
        if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != 0 {
            return None;
        }
        let mut total = None;
        let mut counter: isize = 0;
        if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) == 0
            && PdhCollectQueryData(query) == 0
        {
            let mut bytes: u32 = 0;
            let mut count: u32 = 0;
            let sized = PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_LARGE,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            );
            // The buffer holds the item array and the instance-name strings it points at,
            // so PDH sizes it in bytes; round up to whole items to keep it aligned.
            let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let items = (bytes as usize + item - 1) / item;
            if sized == PDH_MORE_DATA && items > 0 {
                let mut buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = vec![std::mem::zeroed(); items];
                if PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_LARGE,
                    &mut bytes,
                    &mut count,
                    buffer.as_mut_ptr(),
                ) == 0
                {
                    let mut sum: u64 = 0;
                    for entry in &buffer[..(count as usize).min(items)] {
                        if entry.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA {
                            continue;
                        }
                        match instance_pid(entry.szName) {
                            Some(pid) if ours.contains(&pid) => {
                                sum += entry.FmtValue.Anonymous.largeValue.max(0) as u64;
                            }
                            _ => {}
                        }
                    }
                    total = Some(sum / (1024 * 1024));
                }
            }
        }
        PdhCloseQuery(query);
        total
    }
}

/// `pid_23188_luid_0x00000000_0x0000C3A2_phys_0` -> 23188.
///
/// SAFETY: `name` is a NUL-terminated string PDH wrote into the caller's buffer, which
/// outlives this call.
#[cfg(windows)]
unsafe fn instance_pid(name: *const u16) -> Option<u32> {
    if name.is_null() {
        return None;
    }
    let mut length = 0usize;
    while *name.add(length) != 0 {
        length += 1;
    }
    let text = String::from_utf16_lossy(std::slice::from_raw_parts(name, length));
    text.strip_prefix("pid_")?
        .split('_')
        .next()?
        .parse()
        .ok()
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
