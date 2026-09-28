<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface DirEntryInfo {
  name: string; path: string; is_dir: boolean; size: number;
  is_backup_disk: boolean; is_controlled: boolean;
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
const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);
const activeJobs = ref<ActiveJob[]>([]);

const destPath = ref("");
const sources = ref<string[]>([]);
const lastBatch = ref<BackupBatch | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const verifyReport = ref<VerifyReport | null>(null);
const controlledVerify = ref<ControlledVerifyReport | null>(null);

const backupDriveSet = ref<Set<string>>(new Set());
const currentIsBackup = ref(false);

let unlisteners: UnlistenFn[] = [];

const pathLabel = computed(() => currentPath.value || "此电脑（盘符）");
const currentDrive = computed(() => {
  const m = currentPath.value.match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
});
const hasSelection = computed(() => selected.value.size > 0);
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

/** Drive for indexing: current backup path, or one selected backup drive root at 盘符 list. */
function resolveIndexDrive(): string {
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
const canIndex = computed(() => !!resolveIndexDrive());

function formatSize(n: number): string {
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
  } catch { batchIds.value = []; }
}

async function loadDir(path: string) {
  errorMsg.value = "";
  busy.value = true;
  try {
    const raw = await invoke<DirEntryInfo[]>("list_dir", { path });
    entries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
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
    if (!isDriveRootPath(target)) { errorMsg.value = "只能标记盘符根目录为备份盘，不能标记子目录"; return; }
  } else if (isDriveRootPath(currentPath.value)) {
    target = currentPath.value;
  } else {
    errorMsg.value = "只能在盘符根目录标记备份盘（请返回盘符列表或进入 X:\\ 后再标记）"; return;
  }
  const ok = window.confirm(
    `确认将「${target}」标记为 DataVault 备份盘？\n\n将在该盘根目录创建 .datavault 元数据目录。`
  );
  if (!ok) { statusMsg.value = "已取消标记"; return; }
  busy.value = true; statusMsg.value = "正在标记备份盘…";
  try {
    await invoke("mark_backup_disk", { drive: target });
    statusMsg.value = `已标记备份盘：${target}`;
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
    const start = await invoke<JobStart>("start_backup", { sources: src, dest: destPath.value.trim() });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "copying", message: "开始备份…" });
    statusMsg.value = `备份任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doAddControlled() {
  errorMsg.value = ""; statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "请先在备份盘目录下操作（或标记备份盘）"; return;
  }
  if (selected.value.size === 0) { errorMsg.value = "请先勾选要登记的文件或目录"; return; }
  try {
    statusMsg.value = "已启动后台登记（MD5 / FastMD5）…";
    const start = await invoke<JobStart>("start_add_controlled_files", {
      drive: currentDrive.value, paths: Array.from(selected.value),
    });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "scanning", message: "正在收集文件列表…" });
    statusMsg.value = `登记任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doIndex() {
  errorMsg.value = ""; statusMsg.value = "";
  const drive = resolveIndexDrive();
  if (!drive) {
    errorMsg.value = "请先进入已标记的备份盘，或在盘符列表勾选一个备份盘"; return;
  }
  try {
    statusMsg.value = "正在扫描备份盘并建立索引…";
    const start = await invoke<JobStart>("start_index_backup_disk", { drive });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "scanning", message: "正在扫描…" });
    statusMsg.value = `索引任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

/** Verify controlled files: use current browse selection as paths filter; none → all. */
async function doVerifyControlled(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) { errorMsg.value = "当前没有备份盘上下文"; return; }
  try {
    controlledVerify.value = null; verifyReport.value = null;
    const sel = Array.from(selected.value);
    statusMsg.value = mode === "full"
      ? (sel.length ? "按勾选完整校验进行中…" : "全部受控完整校验进行中…")
      : (sel.length ? "按勾选快速校验进行中…" : "全部受控快速校验进行中…");
    const start = await invoke<JobStart>("start_verify_controlled", {
      drive: currentDrive.value,
      mode,
      relPaths: null,
      paths: sel.length ? sel : null,
    });
    upsertJob({ job_id: start.job_id, kind: start.kind, phase: "verifying", message: "正在校验…" });
    statusMsg.value = `校验任务 ${start.job_id} 已开始（可与其它任务并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doVerifyBatch(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!verifyBatchId.value) { errorMsg.value = "请选择要校验的批次"; return; }
  try {
    controlledVerify.value = null; verifyReport.value = null;
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
    if (p.batch) { lastBatch.value = p.batch; verifyBatchId.value = p.batch.id; }
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
    if (p.controlled) controlledVerify.value = p.controlled;
    if (p.batch) verifyReport.value = p.batch;
    if (!p.ok && !p.cancelled) errorMsg.value = p.message;
  });
  unlisteners = [u1, u2, u3, u4, u5, u6];
});
onUnmounted(() => { for (const u of unlisteners) u(); unlisteners = []; });
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>数据管理 <span class="sub">DataVault</span></h1>
        <p class="hint">浏览 · 校验 · 备份 · 受控/索引
          <span v-if="currentIsBackup" class="badge backup">备份盘 {{ currentDrive }}</span>
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
          <button class="btn small primary-outline" title="仅可标记盘符根为备份盘（需二次确认）" :disabled="busy" @click="doMarkBackupDisk">标记为备份盘</button>
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
                <th style="width:90px">大小</th><th style="width:80px">标记</th><th style="width:60px">受控</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in entries" :key="e.path" :class="{ selected: selected.has(e.path) }" @dblclick="openEntry(e)">
                <td><input type="checkbox" :checked="selected.has(e.path)" @change="toggleSelect(e.path)" /></td>
                <td class="name" @click="e.is_dir ? openEntry(e) : toggleSelect(e.path)">
                  <span class="icon">{{ e.is_dir ? "📁" : "📄" }}</span>{{ e.name }}
                </td>
                <td>{{ e.is_dir ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir ? "—" : formatSize(e.size) }}</td>
                <td><span v-if="e.is_backup_disk" class="badge backup">备份盘</span></td>
                <td><span v-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span></td>
              </tr>
              <tr v-if="!entries.length"><td colspan="6" class="muted center">空目录或无法访问</td></tr>
            </tbody>
          </table>
        </div>
      </section>

      <aside class="side">
        <section class="panel">
          <h2>校验</h2>
          <p class="muted small">直接使用当前勾选的目录/文件（仅其中已受控项）；未勾选则校验全部受控。可与备份/索引并行。</p>
          <div class="row">
            <button class="btn primary" title="按当前勾选（或全部）快速校验" :disabled="!currentIsBackup" @click="doVerifyControlled('quick')">快速校验</button>
            <button class="btn" title="按当前勾选（或全部）完整校验" :disabled="!currentIsBackup" @click="doVerifyControlled('full')">完整校验</button>
          </div>
          <label class="field" style="margin-top:10px"><span>备份批次</span>
            <select v-model="verifyBatchId">
              <option disabled value="">请选择批次</option>
              <option v-for="id in batchIds" :key="id" :value="id">{{ id }}</option>
            </select>
          </label>
          <div class="row">
            <button class="btn primary" title="对所选批次做快速校验" @click="doVerifyBatch('quick')">批次快速校验</button>
            <button class="btn" title="对所选批次做完整校验" @click="doVerifyBatch('full')">批次完整校验</button>
          </div>
        </section>

        <section class="panel">
          <h2>备份批次</h2>
          <p class="muted small">源（{{ sources.length }}）</p>
          <ul class="src-list">
            <li v-for="s in sources" :key="s"><span>{{ s }}</span>
              <button class="btn tiny" title="移除" @click="removeSource(s)">×</button></li>
            <li v-if="!sources.length" class="muted">可勾选后「设为备份源」，或直接勾选后开始备份</li>
          </ul>
          <label class="field"><span>目标目录</span>
            <input v-model="destPath" type="text" placeholder="例如 E:\Backup\DataVault" title="备份复制到此目录" />
          </label>
          <button class="btn primary" title="开始将源复制到目标并写批次（可与其它任务并行）" @click="doBackup">开始备份</button>
          <p v-if="lastBatch" class="muted small">最近批次：<strong>{{ lastBatch.id }}</strong>（{{ lastBatch.files.length }} 个文件）</p>
        </section>

        <section class="panel">
          <h2>受控 / 索引</h2>
          <p class="muted small">添加受控须先勾选文件/目录；建立索引在进入备份盘或勾选备份盘符后即可。已在 vault.db 中的跳过，不重算哈希。可与备份/校验并行。</p>
          <div class="row">
            <button class="btn primary-outline" title="将勾选路径登记为受控并计算 MD5/FastMD5（未勾选不可用）"
              :disabled="!currentIsBackup || !hasSelection" @click="doAddControlled">添加受控文件</button>
            <button class="btn primary-outline" title="进入备份盘或勾选备份盘符后即可扫描全盘并为未受控文件建索引"
              :disabled="!canIndex" @click="doIndex">建立备份索引</button>
          </div>
        </section>
      </aside>
    </div>

    <section class="panel results-panel" aria-label="校验结果">
      <h2>校验结果</h2>
      <p class="muted small">结果独立显示在此区域，不与上方浏览 / 操作区混排。</p>
      <div v-if="controlledVerify" class="verify-summary">
        <p>受控文件 · {{ controlledVerify.mode === "full" ? "完整" : "快速" }}：通过
          <strong class="pass">{{ controlledVerify.passed }}</strong> / 失败
          <strong class="fail">{{ controlledVerify.failed }}</strong> / 缺失
          {{ controlledVerify.missing }} / 错误 {{ controlledVerify.errors }}</p>
        <ul class="result-list">
          <li v-for="(it, i) in controlledVerify.items" :key="'c-'+i" :class="{ ok: it.status === 'pass', bad: it.status !== 'pass' }">
            <div class="rel">{{ it.rel_path }}</div>
            <div class="msg">{{ it.status }} — {{ it.message }}</div>
            <div v-if="it.expected" class="hash">期望 {{ it.expected }}</div>
            <div v-if="it.actual" class="hash">实际 {{ it.actual }}</div>
          </li>
        </ul>
      </div>
      <div v-if="verifyReport" class="verify-summary">
        <p>批次 {{ verifyReport.batch_id }} · {{ verifyReport.mode === "full" ? "完整" : "快速" }}：通过
          <strong class="pass">{{ verifyReport.passed }}</strong> / 失败
          <strong class="fail">{{ verifyReport.failed }}</strong></p>
        <ul class="result-list">
          <li v-for="(it, i) in verifyReport.items" :key="'b-'+i" :class="{ ok: it.ok, bad: !it.ok }">
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
.app { min-height:100vh; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:14px 16px 18px; box-sizing:border-box; display:flex; flex-direction:column; gap:10px; }
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
.layout { display:grid; grid-template-columns:1fr 320px; gap:12px; min-height:0; flex:1; }
.side { display:flex; flex-direction:column; gap:10px; min-height:0; overflow:auto; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; }
.explorer { display:flex; flex-direction:column; min-height:0; max-height:calc(100vh - 280px); }
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
.side .row .btn { width:auto; flex:1; margin:0; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.badge { font-size:0.72rem; background:#2f5bff; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; }
.badge.controlled { background:#5b3db8; margin-left:0; }
.src-list { list-style:none; padding:0; margin:0 0 8px; max-height:90px; overflow:auto; font-size:0.78rem; }
.src-list li { display:flex; justify-content:space-between; gap:6px; padding:4px 0; border-bottom:1px solid #243041; word-break:break-all; }
.results-panel { max-height:min(320px, 36vh); overflow:auto; border-color:#3a4a63; }
.results-panel h2 { color:#9db4ff; }
.empty-results { padding:16px 8px; }
.result-list { list-style:none; padding:0; margin:8px 0 0; max-height:220px; overflow:auto; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.rel { font-weight:600; } .hash { font-family:ui-monospace,Consolas,monospace; color:#9aa7b8; word-break:break-all; }
.verify-summary { margin-top:8px; }
@media (max-width:1000px) { .layout { grid-template-columns:1fr; } .explorer { max-height:none; } }
.progress-row { display:flex; align-items:center; gap:12px; }
.progress-main { flex:1; min-width:0; }
</style>
