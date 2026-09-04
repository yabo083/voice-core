// Display formatting and Windows path arithmetic. Nothing here talks to the host.

const KIB = 1024;
const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"] as const;

/** Binary units, because every number this app shows in bytes comes from a
 *  Hugging Face cache or a spool directory that is itself measured in them.
 *  Sub-unit precision drops as the unit grows: "4.83 GiB" is worth reading during
 *  a download, "512.00 B" is not. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  let value = bytes;
  let unit = 0;
  while (value >= KIB && unit < UNITS.length - 1) {
    value /= KIB;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : value < 10 ? 2 : value < 100 ? 1 : 0;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** detect() reports sizes already in GiB. */
export function formatGiB(gib: number): string {
  if (!Number.isFinite(gib)) return "-";
  if (gib >= 100) return `${gib.toFixed(0)} GiB`;
  if (gib >= 10) return `${gib.toFixed(1)} GiB`;
  return `${gib.toFixed(2)} GiB`;
}

/** Clock form for a stage that is running: it is read repeatedly, at a glance,
 *  next to six siblings, so it must not change width every second. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(h > 0 ? 2 : 1, "0");
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

/** Prose form for uptime and idle windows, where the unit carries the meaning. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "-";
  const total = Math.round(ms / 1000);
  if (total < 60) return `${total} 秒`;
  // A zero tail unit is noise: an idle window of exactly 15 minutes reads as
  // "15 分", not "15 分 0 秒".
  const m = Math.floor(total / 60);
  const s = total % 60;
  if (m < 60) return s === 0 ? `${m} 分` : `${m} 分 ${s} 秒`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 === 0 ? `${h} 小时` : `${h} 小时 ${m % 60} 分`;
  const d = Math.floor(h / 24);
  return h % 24 === 0 ? `${d} 天` : `${d} 天 ${h % 24} 小时`;
}

export function formatPercent(done: number, total: number): string {
  if (!(total > 0)) return "";
  return `${Math.min(100, Math.floor((done / total) * 100))}%`;
}

/** Middle ellipsis rather than CSS truncation: the informative half of a path is
 *  its tail, and `direction: rtl` reorders the drive letter and the separators of
 *  a Windows path. The full string always stays in a `title`. */
export function shortenPath(path: string, max = 56): string {
  if (path.length <= max) return path;
  const keepTail = Math.max(12, Math.floor((max - 3) * 0.62));
  const keepHead = max - 3 - keepTail;
  return `${path.slice(0, keepHead)}...${path.slice(path.length - keepTail)}`;
}

/** Last path segment, tolerating either separator and a trailing one. */
export function baseName(path: string): string {
  const cleaned = path.replace(/[\\/]+$/, "");
  const cut = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return cut < 0 ? cleaned : cleaned.slice(cut + 1);
}

export function dirName(path: string): string {
  const cleaned = path.replace(/[\\/]+$/, "");
  const cut = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return cut <= 0 ? cleaned : cleaned.slice(0, cut);
}

/** A pack id has to survive being a JSON key, a CLI argument and a folder name,
 *  so it is reduced to the same shape the shipped packs already use. */
export function slugify(name: string): string {
  const slug = name
    .normalize("NFKD")
    .replace(/\.speaker\.safetensors$/i, "")
    .replace(/[^\p{Letter}\p{Number}]+/gu, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase();
  return slug.length > 0 ? slug : "pack";
}
