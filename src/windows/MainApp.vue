<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openAdvancedVerifyWindow, openAdvancedBackupWindow } from "../bridge";

interface DirEntryInfo {
  name: string; path: string; is_dir: boolean; size: number;
  is_backup_disk: boolean; is_controlled: boolean;
  controlled_count?: number | null; total_files?: number | null;
}
interface FileMeta {
  rel_path: string; src_path: string; dest_path: string; size: number;
  md5_full: string | null; md5_quick: string | null; error: string | null;
}
interface BackupBatch {
  id: string; created_at: string; sources: string[];
  destination_root: string; files: FileMeta[];
}
interface ControlledVerifyItem {
  rel_path: string; status: string; message: string;
  expected: string | null; actual: string | null;
  size_changed: boolean; mtime_changed: boolean;
}
interface ControlledVerifyReport {
  drive_root: string; mode: string; items: ControlledVerifyItem[];
  passed: number; failed: number; missing: number; errors: number;
}
interface VerifyItem {
  rel_path: string; src_path: string; dest_path: string;
  src_hash: string | null; dest_hash: string | null; ok: boolean; message: string;
}
interface VerifyReport {
  batch_id: string; mode: string; items: VerifyItem[]; passed: number; failed: number;
}
interface JobProgress {
  job_id: string; phase: string; current: number; total: number;
  rel_path: string | null; message: string;
}
interface JobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  added?: number; failed?: number; total?: number;
}
interface BackupJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  batch: BackupBatch | null;
}
interface VerifyJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  controlled: ControlledVerifyReport | null; batch: VerifyReport | null;
}
interface JobStart { job_id: string; total: number; kind: string; }

interface ActiveJob {
  job_id: string;
  kind: string;
  label: string;
  phase: string;
  current: number;
  total: number;
  rel_path: string | null;
  message: string;
}

const currentPath = ref("");
const entries = ref<DirEntryInfo[]>([]);

interface DirCountUpdate {
  job_id: number; path: string; controlled_count: number; total_files: number;
}
const dirCountsJobId = ref(0);

function applyDirCount(path: string, controlled_count: number, total_files: number) {
  const list = entries.value;
  const i = list.findIndex((e) => e.path === path);
  if (i < 0) return;
  const next = list.slice();
  next[i] = { ...next[i], controlled_count, total_files };
  entries.value = next;
}

async function requestDirFileCounts(list: DirEntryInfo[]) {
  const paths = list.filter((e) => e.is_dir && e.is_controlled).map((e) => e.path);
  if (!paths.length) { dirCountsJobId.value = 0; return; }
  try {
    dirCountsJobId.value = await invoke<number>("start_dir_file_counts", { paths });
  } catch { /* ignore */ }
}

const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);
const activeJobs = ref<ActiveJob[]>([]);

const destPath = ref("");
const batchName = ref("");
const sources = ref<string[]>([]);
const lastBatch = ref<BackupBatch | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const batchFilter = ref("");
const batchComboOpen = ref(false);
const filteredBatchIds = computed(() => {
  const q = batchFilter.value.trim().toLowerCase();
  if (!q) return batchIds.value;
  return batchIds.value.filter((id) => id.toLowerCase().includes(q));
});
function pickBatch(id: string) {
  verifyBatchId.value = id;
  batchFilter.value = id;
  batchComboOpen.value = false;
}
function onBatchComboBlur(ev: FocusEvent) {
  const box = ev.currentTarget as HTMLElement | null;
  const next = ev.relatedTarget as Node | null;
  if (box && next && box.contains(next)) return;
  batchComboOpen.value = false;
  if (verifyBatchId.value) batchFilter.value = verifyBatchId.value;
}
const verifyReport = ref<VerifyReport | null>(null);
const controlledVerify = ref<ControlledVerifyReport | null>(null);
/** null = show all; otherwise filter detail list by status (toggle on click). */
type ControlledStatusFilter = "pass" | "fail" | "missing" | "error";
const controlledStatusFilter = ref<ControlledStatusFilter | null>(null);
const batchStatusFilter = ref<"pass" | "fail" | null>(null);

const filteredControlledItems = computed(() => {
  const items = controlledVerify.value?.items ?? [];
  const f = controlledStatusFilter.value;
  if (!f) return items;
  return items.filter((it) => it.status === f);
});
const filteredBatchItems = computed(() => {
  const items = verifyReport.value?.items ?? [];
  const f = batchStatusFilter.value;
  if (!f) return items;
  return items.filter((it) => (f === "pass" ? it.ok : !it.ok));
});

function toggleControlledStatusFilter(status: ControlledStatusFilter) {
  controlledStatusFilter.value = controlledStatusFilter.value === status ? null : status;
}
function toggleBatchStatusFilter(status: "pass" | "fail") {
  batchStatusFilter.value = batchStatusFilter.value === status ? null : status;
}
function resetVerifyFilters() {
  controlledStatusFilter.value = null;
  batchStatusFilter.value = null;
}

const backupDriveSet = ref<Set<string>>(new Set());
const currentIsBackup = ref(false);

let unlisteners: UnlistenFn[] = [];

const pathLabel = computed(() => currentPath.value || "此电脑（盘符）");
const currentDrive = computed(() => {
  const m = currentPath.value.match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
});
const hasJobs = computed(() => activeJobs.value.length > 0);

function kindLabel(kind: string): string {
  switch (kind) {
    case "backup": return "备份";
    case "add": return "登记受控";
    case "index": return "建立索引";
    case "controlled-full": return "受控完整校验";
    case "controlled-quick": return "受控快速校验";
    case "batch": return "批次校验";
    default: return kind || "任务";
  }
}

function upsertJob(partial: Partial<ActiveJob> & { job_id: string; kind?: string }) {
  const list = [...activeJobs.value];
  const i = list.findIndex((j) => j.job_id === partial.job_id);
  if (i >= 0) {
    list[i] = { ...list[i], ...partial };
  } else {
    list.push({
      job_id: partial.job_id,
      kind: partial.kind || "task",
      label: kindLabel(partial.kind || "task"),
      phase: partial.phase || "running",
      current: partial.current ?? 0,
      total: partial.total ?? 0,
      rel_path: partial.rel_path ?? null,
      message: partial.message || "",
    });
  }
  activeJobs.value = list;
}

function removeJob(jobId: string) {
  activeJobs.value = activeJobs.value.filter((j) => j.job_id !== jobId);
}

function progressPct(j: ActiveJob): string {
  if (!j.total) return "15%";
  return Math.min(100, (100 * j.current) / j.total) + "%";
}

/** Drive for index/verify: current backup path, or one selected backup drive root at 盘符 list. */
function resolveBackupDrive(): string {
  if (currentIsBackup.value && currentDrive.value) return currentDrive.value;
  if (!currentPath.value && selected.value.size > 0) {
    for (const p of selected.value) {
      if (!isDriveRootPath(p)) continue;
      const key = p.replace(/\//g, "\\").toUpperCase();
      const norm = key.endsWith("\\") ? key : key + "\\";
      if (backupDriveSet.value.has(norm) || backupDriveSet.value.has(key)) return norm;
    }
  }
  return "";
}
/** Single selected drive-letter root at 盘符 list (backup or not). */
function resolveSelectedDriveRoot(): string {
  if (currentPath.value) return "";
  if (selected.value.size !== 1) return "";
  const key = Array.from(selected.value)[0].replace(/\//g, "\\").toUpperCase();
  if (!isDriveRootPath(key)) return "";
  return key.endsWith("\\") ? key : key + "\\";
}
const canIndex = computed(() => !!resolveBackupDrive() || !!resolveSelectedDriveRoot());
/** Enable quick/full verify when browsing a backup disk or when a backup drive letter is checked. */
const canVerifyControlled = computed(() => !!resolveBackupDrive());

function formatSize(n: number | null | undefined): string {
  if (n == null || typeof n !== "number" || !Number.isFinite(n) || n < 0) return "-";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function isDriveRootPath(p: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test((p || "").trim());
}

async function copyCurrentPath() {
  const text = currentPath.value || "";
  try {
    await navigator.clipboard.writeText(text);
    statusMsg.value = text ? `已复制路径：${text}` : "当前在盘符列表（空路径）";
  } catch {
    errorMsg.value = "复制路径失败";
  }
}

async function refreshBackupDrives() {
  try {
    const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
    const next = new Set<string>();
    for (const d of drives) if (d.is_backup_disk) next.add(d.path.toUpperCase());
    backupDriveSet.value = next;
    if (currentDrive.value) currentIsBackup.value = next.has(currentDrive.value.toUpperCase());
  } catch { /* ignore */ }
}

async function refreshBatches() {
  try {
    batchIds.value = await invoke<string[]>("list_batches");
    if (!verifyBatchId.value && batchIds.value.length) verifyBatchId.value = batchIds.value[0];
    if (verifyBatchId.value) batchFilter.value = verifyBatchId.value;
  } catch { batchIds.value = []; }
}

async function loadDir(path: string) {
  errorMsg.value = "";
  busy.value = true;
  try {
    const raw = await invoke<DirEntryInfo[]>("list_dir", { path });
    entries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
    void requestDirFileCounts(entries.value);
    currentPath.value = path;
    selected.value = new Set();
    await refreshBackupDrives();
    if (!path) currentIsBackup.value = false;
    else {
      const drive = path.match(/^([A-Za-z]:)/)?.[1]?.toUpperCase() + "\\";
      currentIsBackup.value = !!drive && backupDriveSet.value.has(drive);
    }
  } catch (e) { errorMsg.value = String(e); }
  finally { busy.value = false; }
}

async function goRoot() { await loadDir(""); }
async function goUp() {
  if (!currentPath.value) return;
  const p = currentPath.value.replace(/[\\/]+$/, "");
  const m = p.match(/^([A-Za-z]:)(?:\\|$)/);
  if (m && (p === m[1] || p === m[1] + "\\")) { await goRoot(); return; }
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (idx <= 2) await loadDir(p.slice(0, 3));
  else await loadDir(p.slice(0, idx));
}

function toggleSelect(path: string) {
  const next = new Set(selected.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  selected.value = next;
}
function selectAllFiles() { selected.value = new Set(entries.value.map((e) => e.path)); }
function clearSelection() { selected.value = new Set(); }
async function openEntry(e: DirEntryInfo) { if (e.is_dir) await loadDir(e.path); }

function useSelectedAsSources() {
  if (!selected.value.size) { errorMsg.value = "请先勾选要作为备份源的项"; return; }
  sources.value = Array.from(selected.value);
  statusMsg.value = `已设置 ${sources.value.length} 个备份源`;
}
function useCurrentAsDest() {
  const dirs = entries.value.filter((e) => selected.value.has(e.path) && e.is_dir);
  if (dirs.length === 1) { destPath.value = dirs[0].path; statusMsg.value = `目标：${destPath.value}`; return; }
  if (dirs.length > 1) { errorMsg.value = "设为目标时请只勾选一个目录"; return; }
  if (!currentPath.value) { errorMsg.value = "请先进入目录或勾选一个目录"; return; }
  destPath.value = currentPath.value;
  statusMsg.value = `目标：${destPath.value}`;
}
function removeSource(s: string) { sources.value = sources.value.filter((x) => x !== s); }

async function doMarkBackupDisk() {
  errorMsg.value = ""; statusMsg.value = "";
  let target = "";
  if (!currentPath.value) {
    if (selected.value.size !== 1) { errorMsg.value = "请在盘符列表中勾选一个盘符根（不可对子目录标记）"; return; }
    target = Array.from(selected.value)[0];
    if (!isDriveRootPath(target)) { errorMsg.value = "只能标记盘符根目录为受控盘，不能标记子目录"; return; }
  } else if (isDriveRootPath(currentPath.value)) {
    target = currentPath.value;
  } else {
    errorMsg.value = "只能在盘符根目录标记受控盘（请返回盘符列表或进入 X:\\ 后再标记）"; return;
  }
  const ok = window.confirm(
    `确认将「${target}」标记为 DataVault 受控盘？\n\n将在该盘根目录创建 .datavault 元数据目录。`
  );
  if (!ok) { statusMsg.value = "已取消标记"; return; }
  busy.value = true; statusMsg.value = "正在标记受控盘…";
  try {
    await invoke("mark_backup_disk", { drive: target });
    statusMsg.value = `已标记受控盘：${target}`;
    await refreshBackupDrives(); currentIsBackup.value = true;
    if (!currentPath.value) await loadDir("");
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
  finally { busy.value = false; }
}

async function doBackup() {
  errorMsg.value = ""; statusMsg.value = "";
  const src = sources.value.length ? sources.value : Array.from(selected.value);
  if (!src.length) { errorMsg.value = "请先勾选或「设为备份源」"; return; }
  if (!destPath.value.trim()) { errorMsg.value = "请填写备份目标目录"; return; }
  try {
    statusMsg.value = "已启动后台备份…";
    const start = await invoke<JobStart>("start_backup", { sources: src, dest: destPath.value.trim(), batchName: batchName.value.trim() || null });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "copying", message: "开始备份…" });
    statusMsg.value = `备份任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

/** Unified index: selected files/dirs → add those; backup drive letter / no selection on backup disk → full-disk index. */
async function doIndex() {
  errorMsg.value = ""; statusMsg.value = "";
  let drive = resolveBackupDrive();
  const sel = Array.from(selected.value);
  const selectedRoot = resolveSelectedDriveRoot();
  const onlyDriveRootAtList = !currentPath.value && sel.length > 0 && sel.every((p) => isDriveRootPath(p));
  const pathSelection = sel.length > 0 && !onlyDriveRootAtList;

  // Non-backup drive letter at 盘符 list: confirm → mark → full-disk index
  if (!drive && selectedRoot) {
    const ok = window.confirm(
      `「${selectedRoot}」尚未标记为受控。\n\n确认标记为 DataVault 受控盘并建立索引？\n将在该盘根目录创建 .datavault 元数据目录，然后扫描建索引。`
    );
    if (!ok) { statusMsg.value = "已取消"; return; }
    busy.value = true;
    try {
      statusMsg.value = "正在标记受控盘…";
      await invoke("mark_backup_disk", { drive: selectedRoot });
      await refreshBackupDrives();
      drive = selectedRoot;
      statusMsg.value = `已标记受控盘：${selectedRoot}`;
    } catch (e) {
      errorMsg.value = String(e); statusMsg.value = ""; return;
    } finally {
      busy.value = false;
    }
  }

  if (!drive) {
    errorMsg.value = "请先进入已标记的受控盘，或在盘符列表勾选一个盘符"; return;
  }
  try {
    if (pathSelection) {
      if (!currentIsBackup.value && !backupDriveSet.value.has(drive.toUpperCase())) {
        errorMsg.value = "请先在受控盘目录下勾选要索引的文件或目录"; return;
      }
      statusMsg.value = "已启动后台索引（勾选路径，跳过已受控）…";
      const start = await invoke<JobStart>("start_add_controlled_files", {
        drive, paths: sel,
      });
      upsertJob({ job_id: start.job_id, kind: start.kind, phase: "scanning", message: "正在收集文件列表…" });
      statusMsg.value = `索引任务 ${start.job_id} 已开始（可与其它任务并行）`;
    } else {
      statusMsg.value = "正在扫描受控盘并建立索引…";
      const start = await invoke<JobStart>("start_index_backup_disk", { drive });
      upsertJob({ job_id: start.job_id, kind: start.kind, phase: "scanning", message: "正在扫描…" });
      statusMsg.value = `索引任务 ${start.job_id} 已开始（可与其它任务并行）`;
    }
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

/** Verify controlled files: selection filters scope; backup drive-letter check = all controlled on that disk. */
async function doVerifyControlled(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  const drive = resolveBackupDrive();
  if (!drive) { errorMsg.value = "请先进入已标记的受控盘，或在盘符列表勾选一个受控盘"; return; }
  try {
    controlledVerify.value = null; verifyReport.value = null; resetVerifyFilters();
    const sel = Array.from(selected.value);
    // At 盘符 list with backup root checked: paths include drive root → backend expands to all controlled.
    // Nothing checked → paths null → verify all controlled files on this backup drive.
    const onlyBackupRoot = !currentPath.value && sel.length > 0 && sel.every((p) => isDriveRootPath(p));
    const pathsArg = onlyBackupRoot ? sel : (sel.length > 0 ? sel : null);
    statusMsg.value = mode === "full"
      ? (pathsArg && !onlyBackupRoot ? "按勾选完整校验进行中…" : "全部受控完整校验进行中…")
      : (pathsArg && !onlyBackupRoot ? "按勾选快速校验进行中…" : "全部受控快速校验进行中…");
    const start = await invoke<JobStart>("start_verify_controlled", {
      drive,
      mode,
      relPaths: null,
      paths: pathsArg,
    });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "verifying", message: "正在校验…" });
    statusMsg.value = `校验任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

async function openAdvancedVerify() {
  errorMsg.value = "";
  try { await openAdvancedVerifyWindow(); }
  catch (e) { errorMsg.value = String(e); }
}
async function openAdvancedBackup() {
  errorMsg.value = "";
  try { await openAdvancedBackupWindow(); }
  catch (e) { errorMsg.value = String(e); }
}
async function doVerifyBatch(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!verifyBatchId.value) { errorMsg.value = "请选择要校验的批次"; return; }
  try {
    controlledVerify.value = null; verifyReport.value = null; resetVerifyFilters();
    statusMsg.value = mode === "full" ? "批次完整校验进行中…" : "批次快速校验进行中…";
    const start = await invoke<JobStart>("start_verify_backup", {
      batchId: verifyBatchId.value, mode,
    });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "verifying", message: "正在校验批次…" });
    statusMsg.value = `批次校验 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doCancelJob(jobId: string) {
  try {
    const ok = await invoke<boolean>("cancel_job", { jobId });
    statusMsg.value = ok ? `正在取消任务 ${jobId}…` : `未找到任务 ${jobId}`;
  } catch (e) { errorMsg.value = String(e); }
}

onMounted(async () => {
  await goRoot();
  await refreshBatches();
  const bind = async (ev: string, fn: (p: any) => void) => listen(ev, (e) => fn(e.payload));
  const u1 = await bind("controlled-job-progress", (p: JobProgress) => {
    upsertJob({
      job_id: p.job_id,
      phase: p.phase,
      current: p.current,
      total: p.total,
      rel_path: p.rel_path,
      message: p.message,
    });
    statusMsg.value = p.message;
  });
  const u2 = await bind("controlled-job-finished", (p: JobFinished) => {
    removeJob(p.job_id);
    statusMsg.value = p.message;
    if (!p.ok && !p.cancelled) errorMsg.value = p.message;
  });
  const u3 = await bind("backup-job-progress", (p: JobProgress) => {
    upsertJob({
      job_id: p.job_id,
      kind: "backup",
      phase: p.phase,
      current: p.current,
      total: p.total,
      rel_path: p.rel_path,
      message: p.message,
    });
    statusMsg.value = p.message;
  });
  const u4 = await bind("backup-job-finished", (p: BackupJobFinished) => {
    removeJob(p.job_id);
    statusMsg.value = p.message;
    if (p.batch) { lastBatch.value = p.batch; verifyBatchId.value = p.batch.id; batchFilter.value = p.batch.id; }
    if (!p.ok && !p.cancelled) errorMsg.value = p.message;
    void refreshBatches();
  });
  const u5 = await bind("verify-job-progress", (p: JobProgress) => {
    upsertJob({
      job_id: p.job_id,
      phase: p.phase,
      current: p.current,
      total: p.total,
      rel_path: p.rel_path,
      message: p.message,
    });
    statusMsg.value = p.message;
  });
  const u6 = await bind("verify-job-finished", (p: VerifyJobFinished) => {
    removeJob(p.job_id);
    statusMsg.value = p.message;
    resetVerifyFilters();
    if (p.controlled) controlledVerify.value = p.controlled;
    if (p.batch) verifyReport.value = p.batch;
    if (!p.ok && !p.cancelled) errorMsg.value = p.message;
  });
  const u7 = await bind("dir-counts-update", (p: DirCountUpdate) => {
    if (p.job_id !== dirCountsJobId.value) return;
    applyDirCount(p.path, p.controlled_count, p.total_files);
  });
  unlisteners = [u1, u2, u3, u4, u5, u6, u7];
});
onUnmounted(() => { for (const u of unlisteners) u(); unlisteners = []; });
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>数据管理 <span class="sub">DataVault</span></h1>
        <p class="hint">浏览 · 校验 · 备份 · 受控/索引
          <span v-if="currentIsBackup" class="badge backup">受控盘 {{ currentDrive }}</span>
        </p>
      </div>
    </header>

    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-if="statusMsg" class="banner ok">{{ statusMsg }}</div>
    <div v-if="hasJobs" class="banner progress jobs-panel">
      <div class="jobs-title">进行中的任务（{{ activeJobs.length }}）— 可并行，各自取消</div>
      <div v-for="j in activeJobs" :key="j.job_id" class="progress-row">
        <div class="progress-main">
          <div class="progress-meta">
            <span><strong>{{ j.label }}</strong> · {{ j.phase }} <span class="job-id">{{ j.job_id }}</span></span>
            <span v-if="j.total">{{ j.current }} / {{ j.total }}</span>
          </div>
          <div class="progress-track">
            <div class="progress-fill" :style="{ width: progressPct(j) }"></div>
          </div>
          <div v-if="j.message" class="progress-file">{{ j.message }}</div>
          <div v-if="j.rel_path" class="progress-file">{{ j.rel_path }}</div>
        </div>
        <button class="btn small" title="取消此任务" @click="doCancelJob(j.job_id)">取消</button>
      </div>
    </div>

    <div class="layout">
      <section class="panel explorer">
        <div class="toolbar">
          <button class="btn small" title="返回盘符列表" :disabled="busy" @click="goRoot">盘符</button>
          <button class="btn small" title="返回上一级" :disabled="busy || !currentPath" @click="goUp">上级</button>
          <button class="btn small" title="刷新当前目录" :disabled="busy" @click="loadDir(currentPath)">刷新</button>
          <button class="btn small" title="全选当前列表" @click="selectAllFiles">全选</button>
          <button class="btn small" title="清空勾选" @click="clearSelection">清空选择</button>
          <button class="btn small primary-outline" title="将勾选设为备份源" @click="useSelectedAsSources">设为备份源</button>
          <button class="btn small primary-outline" title="将勾选的唯一目录或当前目录设为目标" @click="useCurrentAsDest">设为目标目录</button>
          <button class="btn small primary-outline" title="仅可标记盘符根为受控（需二次确认）" :disabled="busy" @click="doMarkBackupDisk">标记为受控</button>
          <span class="muted">已选 {{ selected.size }} 项</span>
        </div>
        <div class="pathbar" title="点击复制路径">
          <span class="label">当前位置</span>
          <code class="path-copy" tabindex="0" @click="copyCurrentPath" @keydown.enter.prevent="copyCurrentPath">{{ pathLabel }}</code>
          <button class="btn small" type="button" title="复制路径" @click="copyCurrentPath">复制</button>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th>
                <th style="width:90px">大小</th><th style="width:60px">受控</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in entries" :key="e.path" :class="{ selected: selected.has(e.path) }" @dblclick="openEntry(e)">
                <td><input type="checkbox" :checked="selected.has(e.path)" @change="toggleSelect(e.path)" /></td>
                <td class="name" @click="e.is_dir ? openEntry(e) : toggleSelect(e.path)">
                  <span class="icon" aria-hidden="true">{{ e.is_dir || isDriveRootPath(e.path) ? "📁" : "📄" }}</span>{{ e.name }}
                </td>
                <td>{{ e.is_dir || isDriveRootPath(e.path) ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir || isDriveRootPath(e.path) ? "—" : formatSize(e.size) }}</td>
                <td>
                  <span v-if="e.is_dir && e.is_controlled" class="badge controlled" title="受控文件数/总文件数">{{ e.controlled_count != null && e.total_files != null ? e.controlled_count + '/' + e.total_files : '…' }}</span>
                  <span v-else-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span>
                </td>
              </tr>
              <tr v-if="!entries.length"><td colspan="5" class="muted center">空目录或无法访问</td></tr>
            </tbody>
          </table>
        </div>
      </section>

      <aside class="side">
        <section class="panel">
          <div class="panel-head">
            <h2>校验</h2>
            <button type="button" class="btn small primary-outline panel-head-action" title="打开高级校验窗口：批次号/时间/文件数/盘符，可多选校验" @click="openAdvancedVerify">高级校验</button>
          </div>
          <p class="muted small">直接使用当前勾选的目录/文件（仅其中已受控项）；未勾选则校验全部受控。在盘符列表勾选受控盘符亦可（等同该盘全部受控）。非受控盘符不启用。</p>
          <div class="row">
            <button class="btn primary" title="按当前勾选（或全部）快速校验；盘符列表勾选受控盘=该盘全部受控" :disabled="!canVerifyControlled" @click="doVerifyControlled('quick')">快速校验</button>
            <button class="btn" title="按当前勾选（或全部）完整校验；盘符列表勾选受控盘=该盘全部受控" :disabled="!canVerifyControlled" @click="doVerifyControlled('full')">完整校验</button>
          </div>
          <label class="field" style="margin-top:10px"><span>备份批次</span>
            <div class="batch-combo" @focusout="onBatchComboBlur">
              <input type="text" class="batch-combo-input" v-model="batchFilter" :placeholder="verifyBatchId || '请选择或输入过滤'" title="输入关键词过滤批次列表，点选一项" autocomplete="off" @focus="batchComboOpen = true" @input="batchComboOpen = true" @keydown.down.prevent="batchComboOpen = true" />
              <ul v-if="batchComboOpen" class="batch-combo-list">
                <li v-if="!filteredBatchIds.length" class="muted">无匹配批次</li>
                <li v-for="id in filteredBatchIds" :key="id" :class="{ active: id === verifyBatchId }" @mousedown.prevent="pickBatch(id)">{{ id }}</li>
              </ul>
            </div>
          </label>
          <div class="row">
            <button class="btn primary" title="对所选批次做快速校验" @click="doVerifyBatch('quick')">批次快速校验</button>
            <button class="btn" title="对所选批次做完整校验" @click="doVerifyBatch('full')">批次完整校验</button>
          </div>
        </section>

        <section class="panel">
          <div class="panel-head">
            <h2>备份批次</h2>
            <button type="button" class="btn small primary-outline panel-head-action" title="打开双栏高级备份：左多选源，右单选受控盘/子目录" @click="openAdvancedBackup">高级备份</button>
          </div>
          <p class="muted small">源（{{ sources.length }}）</p>
          <ul class="src-list">
            <li v-for="s in sources" :key="s"><span>{{ s }}</span>
              <button class="btn tiny" title="移除" @click="removeSource(s)">×</button></li>
            <li v-if="!sources.length" class="muted">可勾选后「设为备份源」，或直接勾选后开始备份</li>
          </ul>
          <label class="field"><span>批次名称（可选，默认年月日时分秒）</span>
            <input v-model="batchName" type="text" placeholder="例如 项目A-全量" title="留空则使用本地时间 YYYYMMDDHHmmss 作为批次号" />
          </label>
          <label class="field"><span>目标目录</span>
            <input v-model="destPath" type="text" placeholder="例如 E:\Backup\DataVault" title="备份复制到此目录" />
          </label>
          <button class="btn primary" title="开始将源复制到目标并写批次（可与其它任务并行）" @click="doBackup">开始备份</button>
          <p v-if="lastBatch" class="muted small">最近批次：<strong>{{ lastBatch.id }}</strong>（{{ lastBatch.files.length }} 个文件）</p>
        </section>

        <section class="panel">
          <div class="panel-head">
            <h2>备份索引</h2>
          </div>
          <p class="muted small" title="勾选文件或目录后，索引所选项，并将所选盘标记为受控；已在 vault.db 中的跳过，不重算哈希。">勾选文件/目录后索引所选项，并将所选盘标记为受控；已在 vault.db 中的跳过，不重算哈希。</p>
          <button class="btn primary-outline" title="勾选文件/目录→索引所选并标记该盘为受控；已在 vault.db 中的跳过，不重算哈希"
            :disabled="!canIndex" @click="doIndex">建立备份索引</button>
        </section>
      </aside>
    </div>

    <section class="panel results-panel" aria-label="校验结果">
      <h2>校验结果</h2>
      <div v-if="controlledVerify" class="verify-summary">
        <p>受控文件 · {{ controlledVerify.mode === "full" ? "完整" : "快速" }}：
          <button type="button" class="stat-filter pass" :class="{ active: controlledStatusFilter === 'pass' }" title="筛选：通过（再点取消）" @click="toggleControlledStatusFilter('pass')">通过 <strong>{{ controlledVerify.passed }}</strong></button>
          <button type="button" class="stat-filter fail" :class="{ active: controlledStatusFilter === 'fail' }" title="筛选：失败（再点取消）" @click="toggleControlledStatusFilter('fail')">失败 <strong>{{ controlledVerify.failed }}</strong></button>
          <button type="button" class="stat-filter miss" :class="{ active: controlledStatusFilter === 'missing' }" title="筛选：缺失（再点取消）" @click="toggleControlledStatusFilter('missing')">缺失 <strong>{{ controlledVerify.missing }}</strong></button>
          <button type="button" class="stat-filter err" :class="{ active: controlledStatusFilter === 'error' }" title="筛选：错误（再点取消）" @click="toggleControlledStatusFilter('error')">错误 <strong>{{ controlledVerify.errors }}</strong></button>
        </p>
        <ul class="result-list">
          <li v-for="(it, i) in filteredControlledItems" :key="'c-'+i" :class="{ ok: it.status === 'pass', bad: it.status !== 'pass' }">
            <div class="rel">{{ it.rel_path }}</div>
            <div class="msg">{{ it.status }} — {{ it.message }}</div>
            <div v-if="it.expected" class="hash">期望 {{ it.expected }}</div>
            <div v-if="it.actual" class="hash">实际 {{ it.actual }}</div>
          </li>
        </ul>
      </div>
      <div v-if="verifyReport" class="verify-summary">
        <p>批次 {{ verifyReport.batch_id }} · {{ verifyReport.mode === "full" ? "完整" : "快速" }}：
          <button type="button" class="stat-filter pass" :class="{ active: batchStatusFilter === 'pass' }" title="筛选：通过（再点取消）" @click="toggleBatchStatusFilter('pass')">通过 <strong>{{ verifyReport.passed }}</strong></button>
          <button type="button" class="stat-filter fail" :class="{ active: batchStatusFilter === 'fail' }" title="筛选：失败（再点取消）" @click="toggleBatchStatusFilter('fail')">失败 <strong>{{ verifyReport.failed }}</strong></button>
        </p>
        <ul class="result-list">
          <li v-for="(it, i) in filteredBatchItems" :key="'b-'+i" :class="{ ok: it.ok, bad: !it.ok }">
            <div class="rel">{{ it.rel_path }}</div>
            <div class="msg">{{ it.message }}</div>
            <div v-if="it.src_hash" class="hash">源 {{ it.src_hash }}</div>
            <div v-if="it.dest_hash" class="hash">目标 {{ it.dest_hash }}</div>
          </li>
        </ul>
      </div>
      <p v-if="!controlledVerify && !verifyReport" class="muted center empty-results">暂无校验结果。在上方执行受控/批次校验后显示于此。</p>
    </section>
  </div>
</template>

<style scoped>
.app { min-height:100vh; height:100vh; overflow-x:hidden; overflow-y:auto; scrollbar-width:thin; scrollbar-color:#3a4a63 transparent; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:12px 16px 12px; box-sizing:border-box; display:flex; flex-direction:column; gap:8px; }
.header { display:flex; justify-content:space-between; align-items:flex-start; gap:12px; }
h1 { margin:0; font-size:1.35rem; font-weight:700; }
.sub { color:#7aa2ff; font-weight:500; font-size:0.95rem; }
.hint { margin:4px 0 0; color:#9aa7b8; font-size:0.85rem; }
.banner { padding:8px 12px; border-radius:8px; font-size:0.88rem; }
.banner.error { background:#3a1515; color:#ffb4b4; border:1px solid #7a2e2e; }
.banner.ok { background:#14301f; color:#b6f0c8; border:1px solid #2d6a45; }
.banner.progress { background:#152038; color:#c5d4ff; border:1px solid #2f5bff; }
.jobs-panel { display:flex; flex-direction:column; gap:10px; }
.jobs-title { font-size:0.82rem; color:#9db4ff; margin-bottom:2px; }
.progress-meta { display:flex; justify-content:space-between; font-size:0.85rem; margin-bottom:6px; gap:8px; }
.job-id { color:#7a8aa0; font-size:0.72rem; margin-left:6px; font-family:ui-monospace,Consolas,monospace; }
.progress-track { height:8px; background:#0f1419; border-radius:999px; overflow:hidden; }
.progress-fill { height:100%; background:linear-gradient(90deg,#2f5bff,#6d9bff); }
.progress-file { margin-top:6px; font-size:0.78rem; color:#9aa7b8; word-break:break-all; }
.layout { display:grid; grid-template-columns:1fr 340px; gap:12px; min-height:0; flex:0 0 auto; align-items:stretch; }
.side { display:flex; flex-direction:column; gap:8px; min-height:0; height:auto; align-self:start; overflow:visible; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; }
.side > .panel { flex:0 0 auto; min-height:auto; overflow:visible; padding:8px 10px; }
.panel-head { display:flex; align-items:center; justify-content:space-between; gap:8px; margin:0 0 4px; }
.panel-head h2 { margin:0; font-size:0.88rem; flex:1; min-width:0; }
.panel-head-action { flex:0 0 auto; width:auto; margin:0; padding:4px 8px; white-space:nowrap; }
.side h2 { margin:0 0 4px; font-size:0.88rem; }
.side .muted.small { margin:0 0 4px; line-height:1.3; display:-webkit-box; -webkit-box-orient:vertical; -webkit-line-clamp:2; overflow:hidden; }
.side .field { margin-bottom:6px; }
.side .row { gap:6px; }
/* Left browser height follows right column (校验+备份+索引); list scrolls inside. height:0 + min-height:100% => row sized by .side. */
.explorer { display:flex; flex-direction:column; height:0; min-height:100%; max-height:none; overflow:hidden; }
.explorer .table-wrap { flex:1; min-height:0; overflow:auto; }
.pathbar { display:flex; align-items:center; gap:10px; margin-bottom:8px; width:100%; }
.pathbar .label { color:#9aa7b8; font-size:0.8rem; white-space:nowrap; }
.pathbar code.path-copy {
  flex:1; background:#0f1419; padding:8px 10px; border-radius:6px; border:1px solid #2a3442;
  font-size:0.85rem; user-select:all; cursor:pointer; word-break:break-all; white-space:pre-wrap;
}
.pathbar code.path-copy:hover { border-color:#2f5bff; }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:8px; flex-wrap:wrap; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; min-height:180px; }
table { width:100%; border-collapse:collapse; font-size:0.88rem; }
th, td { padding:7px 9px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; }
.name { cursor:pointer; user-select:none; } .icon { margin-right:6px; }
h2 { margin:0 0 8px; font-size:0.95rem; }
.field { display:flex; flex-direction:column; gap:4px; margin-bottom:10px; font-size:0.8rem; color:#9aa7b8; }
input[type="text"], select { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.row { display:flex; gap:8px; flex-wrap:wrap; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; }
.btn:disabled { opacity:0.5; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; width:100%; }
.btn.primary-outline { border-color:#2f5bff; color:#9db4ff; background:transparent; width:100%; margin-top:6px; }
.btn.small { padding:4px 8px; font-size:0.78rem; width:auto; margin:0; }
.btn.tiny { padding:0 6px; font-size:0.75rem; }
.side .row .btn { width:auto; flex:1 1 calc(50% - 4px); min-width:0; margin:0; padding:6px 8px; }
.side > .panel > .btn { padding:6px 10px; }
.batch-combo { position:relative; }
.batch-combo-input { width:100%; }
.batch-combo-list { position:absolute; z-index:20; left:0; right:0; top:calc(100% + 2px); max-height:160px; overflow:auto; margin:0; padding:4px 0; list-style:none; background:#0f1419; border:1px solid #2a3442; border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.35); }
.batch-combo-list li { padding:6px 10px; cursor:pointer; font-size:0.82rem; word-break:break-all; }
.batch-combo-list li:hover, .batch-combo-list li.active { background:#1e2a40; }
.batch-combo-list li.muted { cursor:default; color:#9aa7b8; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.badge { font-size:0.72rem; background:#2f5bff; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; }
.badge.controlled { background:#5b3db8; margin-left:0; }
.src-list { list-style:none; padding:0; margin:0 0 4px; max-height:52px; overflow:auto; font-size:0.78rem; scrollbar-width:none; }
.src-list::-webkit-scrollbar { width:0; height:0; display:none; }
.src-list li { display:flex; justify-content:space-between; gap:6px; padding:4px 0; border-bottom:1px solid #243041; word-break:break-all; }
.results-panel { flex:0 0 auto; max-height:min(480px, 48vh); min-height:200px; overflow:hidden; border-color:#3a4a63; padding:8px 12px; display:flex; flex-direction:column; }
.results-panel h2 { color:#9db4ff; flex:0 0 auto; }
.results-panel > .muted.small { flex:0 0 auto; }
.empty-results { padding:6px 8px; margin:0; flex:0 0 auto; }
.verify-summary { margin-top:6px; flex:1 1 auto; min-height:0; display:flex; flex-direction:column; overflow:hidden; }
.verify-summary > p { flex:0 0 auto; margin:0; }
.result-list { list-style:none; padding:0; margin:6px 0 0; flex:1 1 auto; min-height:0; max-height:none; overflow:auto; scrollbar-width:thin; scrollbar-color:#3a4a63 transparent; }
.result-list::-webkit-scrollbar { width:6px; height:6px; }
.result-list::-webkit-scrollbar-track { background:transparent; }
.result-list::-webkit-scrollbar-thumb { background:#3a4a63; border-radius:999px; }
.result-list::-webkit-scrollbar-thumb:hover { background:#4a5a73; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.rel { font-weight:600; } .hash { font-family:ui-monospace,Consolas,monospace; color:#9aa7b8; word-break:break-all; }
.stat-filter {
  appearance:none; background:transparent; border:1px solid transparent; color:inherit;
  font:inherit; padding:1px 6px; margin:0 2px; border-radius:6px; cursor:pointer; line-height:1.4;
}
.stat-filter strong { font-weight:700; }
.stat-filter.pass, .stat-filter.pass strong { color:#3ecf8e; }
.stat-filter.fail, .stat-filter.fail strong { color:#ff6b6b; }
.stat-filter.miss, .stat-filter.miss strong { color:#e6c07b; }
.stat-filter.err, .stat-filter.err strong { color:#ff9f43; }
.stat-filter:hover { border-color:#3a4a63; background:#0f1419; }
.stat-filter.active { border-color:#6d9bff; background:#1a2433; box-shadow:inset 0 0 0 1px #2f5bff55; }
@media (max-width:1000px) { .layout { grid-template-columns:1fr; } .explorer { height:auto; min-height:0; max-height:50vh; } .side { height:auto; align-self:stretch; overflow:visible; } }
.progress-row { display:flex; align-items:center; gap:12px; }
.progress-main { flex:1; min-width:0; }
</style>
