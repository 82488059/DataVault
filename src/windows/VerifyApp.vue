<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { EVT_VERIFY_CTX, readVerifyCtx, type VerifyContext } from "../bridge";

interface ControlledFile {
  rel_path: string; size: number; mtime: number; md5: string; fast_md5: string;
  sample_ratio: number; sample_chunk_mb: number; updated_at: string;
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
interface VerifyJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  controlled: ControlledVerifyReport | null; batch: VerifyReport | null;
}
interface JobStart { job_id: string; total: number; kind: string; }
interface DirEntryInfo {
  name: string; path: string; is_dir: boolean; size: number;
  is_backup_disk: boolean; is_controlled: boolean;
}

const drive = ref("");
const isBackupDisk = ref(false);
const controlledFiles = ref<ControlledFile[]>([]);
const selectedControlled = ref<Set<string>>(new Set());
const pathFilterActive = ref(false);
const controlledVerify = ref<ControlledVerifyReport | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const verifyReport = ref<VerifyReport | null>(null);
const errorMsg = ref("");
const statusMsg = ref("");
const jobRunning = ref(false);
const jobProgress = ref<JobProgress | null>(null);

/** Mini browser: pick dirs/files on the backup disk to scope verify. */
const browsePath = ref("");
const browseEntries = ref<DirEntryInfo[]>([]);
const browseSelected = ref<Set<string>>(new Set());
const browseBusy = ref(false);

let pendingAbsPaths: string[] = [];
let unlisteners: UnlistenFn[] = [];

const displayedControlled = computed(() => {
  if (!pathFilterActive.value || selectedControlled.value.size === 0) {
    return controlledFiles.value;
  }
  return controlledFiles.value.filter((f) => selectedControlled.value.has(f.rel_path));
});

async function applyCtx(ctx: VerifyContext) {
  drive.value = ctx.drive || "";
  isBackupDisk.value = !!ctx.isBackupDisk;
  selectedControlled.value = new Set(ctx.relPaths || []);
  pendingAbsPaths = [...(ctx.paths || [])];
  pathFilterActive.value = selectedControlled.value.size > 0 || pendingAbsPaths.length > 0;
  await refreshAll();
  if (pendingAbsPaths.length) {
    await resolveAbsPaths(pendingAbsPaths);
    pendingAbsPaths = [];
  }
  if (drive.value && isBackupDisk.value) {
    await loadBrowse(drive.value);
  }
}

async function refreshAll() {
  controlledFiles.value = [];
  controlledVerify.value = null;
  try {
    batchIds.value = await invoke<string[]>("list_batches");
    if (!verifyBatchId.value && batchIds.value.length) verifyBatchId.value = batchIds.value[0];
  } catch { batchIds.value = []; }
  if (!drive.value || !isBackupDisk.value) {
    try {
      const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
      const hit = drives.find((d) => d.is_backup_disk);
      if (hit) { drive.value = hit.path; isBackupDisk.value = true; }
    } catch { /* ignore */ }
  }
  if (drive.value && isBackupDisk.value) {
    try {
      controlledFiles.value = await invoke<ControlledFile[]>("list_controlled_files", { drive: drive.value });
    } catch { controlledFiles.value = []; }
  }
}

/** Resolve absolute file/dir picks to controlled-only rel_paths (vault.db). */
async function resolveAbsPaths(paths: string[]) {
  if (!drive.value || !paths.length) return;
  try {
    const hit = await invoke<ControlledFile[]>("resolve_controlled_selection", {
      drive: drive.value,
      paths,
    });
    selectedControlled.value = new Set(hit.map((f) => f.rel_path));
    pathFilterActive.value = true;
    statusMsg.value = hit.length
      ? `已按所选目录/文件匹配 ${hit.length} 个受控项（未受控已忽略）`
      : "所选路径下没有已受控文件";
  } catch (e) {
    errorMsg.value = String(e);
  }
}

async function loadBrowse(path: string) {
  browseBusy.value = true;
  try {
    const raw = await invoke<DirEntryInfo[]>("list_dir", { path });
    // Only show controlled files, and dirs that contain controlled files (or any dir for navigation).
    // Dirs always shown for navigation; files only if is_controlled.
    browseEntries.value = raw.filter((e) => e.is_dir || e.is_controlled);
    browsePath.value = path;
    browseSelected.value = new Set();
  } catch (e) {
    errorMsg.value = String(e);
  } finally {
    browseBusy.value = false;
  }
}

async function browseRoot() {
  if (drive.value) await loadBrowse(drive.value);
}
async function browseUp() {
  if (!browsePath.value) return;
  const p = browsePath.value.replace(/[\\/]+$/, "");
  const m = p.match(/^([A-Za-z]:)(?:\\|$)/);
  if (m && (p === m[1] || p === m[1] + "\\")) { await browseRoot(); return; }
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (idx <= 2) await loadBrowse(p.slice(0, 3));
  else await loadBrowse(p.slice(0, idx));
}
function toggleBrowse(path: string) {
  const next = new Set(browseSelected.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  browseSelected.value = next;
}
async function openBrowseEntry(e: DirEntryInfo) {
  if (e.is_dir) await loadBrowse(e.path);
}

async function applyBrowseSelection() {
  errorMsg.value = "";
  const paths = Array.from(browseSelected.value);
  if (!paths.length) {
    // Use current browse folder as scope
    if (!browsePath.value) { errorMsg.value = "请勾选目录/文件，或进入要校验的目录"; return; }
    await resolveAbsPaths([browsePath.value]);
    return;
  }
  await resolveAbsPaths(paths);
}

async function verifyFromBrowse(mode: "full" | "quick") {
  // If browse has checkboxes (or a current folder), apply to controlled range first;
  // otherwise verify whatever is already checked in the controlled list (or all).
  if (browseSelected.value.size > 0 || browsePath.value) {
    await applyBrowseSelection();
    if (selectedControlled.value.size === 0) {
      // applyBrowseSelection already set status/error for empty match
      if (pathFilterActive.value) return;
    }
  }
  await doVerifyControlled(mode);
}
function clearPathFilter() {
  selectedControlled.value = new Set();
  pathFilterActive.value = false;
  statusMsg.value = "已清除范围，校验将针对全部受控文件";
}

function toggleControlled(rel: string) {
  const next = new Set(selectedControlled.value);
  if (next.has(rel)) next.delete(rel); else next.add(rel);
  selectedControlled.value = next;
  pathFilterActive.value = next.size > 0;
}

function selectAllDisplayed() {
  selectedControlled.value = new Set(displayedControlled.value.map((f) => f.rel_path));
  pathFilterActive.value = selectedControlled.value.size > 0;
}

async function doVerifyControlled(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!drive.value || !isBackupDisk.value) { errorMsg.value = "当前没有备份盘上下文"; return; }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null; controlledVerify.value = null;
    statusMsg.value = mode === "full" ? "完整校验进行中…" : "快速校验（FastMD5）进行中…";
    const relPaths =
      selectedControlled.value.size > 0 ? Array.from(selectedControlled.value) : null;
    const start = await invoke<JobStart>("start_verify_controlled", {
      drive: drive.value,
      mode,
      relPaths,
      paths: null,
    });
    statusMsg.value = `校验任务 ${start.job_id} 已开始`;
  } catch (e) { jobRunning.value = false; errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doVerifyBatch(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!verifyBatchId.value) { errorMsg.value = "请选择要校验的批次"; return; }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null; verifyReport.value = null;
    statusMsg.value = mode === "full" ? "批次完整校验进行中…" : "批次快速校验进行中…";
    const start = await invoke<JobStart>("start_verify_backup", {
      batchId: verifyBatchId.value, mode,
    });
    statusMsg.value = `批次校验 ${start.job_id} 已开始`;
  } catch (e) { jobRunning.value = false; errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doCancel() {
  try { await invoke<boolean>("cancel_verify_job"); statusMsg.value = "正在取消…"; }
  catch (e) { errorMsg.value = String(e); }
}

async function closeWin() {
  try { await getCurrentWebviewWindow().close(); } catch { /* ignore */ }
}

onMounted(async () => {
  const ctx = readVerifyCtx();
  if (ctx) await applyCtx(ctx); else await refreshAll();
  const u0 = await listen<VerifyContext>(EVT_VERIFY_CTX, (ev) => { void applyCtx(ev.payload); });
  const u1 = await listen<JobProgress>("verify-job-progress", (ev) => {
    jobProgress.value = ev.payload; jobRunning.value = true; statusMsg.value = ev.payload.message;
  });
  const u2 = await listen<VerifyJobFinished>("verify-job-finished", (ev) => {
    jobRunning.value = false; jobProgress.value = null; statusMsg.value = ev.payload.message;
    if (ev.payload.controlled) controlledVerify.value = ev.payload.controlled;
    if (ev.payload.batch) verifyReport.value = ev.payload.batch;
    if (!ev.payload.ok && !ev.payload.cancelled) errorMsg.value = ev.payload.message;
  });
  unlisteners = [u0, u1, u2];
});
onUnmounted(() => { for (const u of unlisteners) u(); unlisteners = []; });
</script>
<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>校验 <span class="sub">DataVault</span></h1>
        <p class="hint">完整 / 快速校验与结果
          <span v-if="isBackupDisk" class="badge backup">{{ drive }}</span>
        </p>
      </div>
      <button class="btn ghost" title="关闭校验窗口" @click="closeWin">关闭</button>
    </header>
    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-if="statusMsg" class="banner ok">{{ statusMsg }}</div>
    <div v-if="jobRunning && jobProgress" class="banner progress">
      <div class="progress-meta"><span>{{ jobProgress.phase }}</span>
        <span v-if="jobProgress.total">{{ jobProgress.current }} / {{ jobProgress.total }}</span></div>
      <div class="progress-track"><div class="progress-fill" :style="{ width: jobProgress.total ? Math.min(100, (100 * jobProgress.current) / jobProgress.total) + '%' : '15%' }"></div></div>
      <div v-if="jobProgress.rel_path" class="progress-file">{{ jobProgress.rel_path }}</div>
    </div>
    <div class="layout">
      <section class="panel">
        <h2>按目录 / 文件选择范围</h2>
        <p class="muted small">仅显示已受控项；勾选目录/文件后，可在下方操作区直接「快速校验」「完整校验」（会先应用范围），或先「应用到校验范围」。未受控不会进入结果。</p>
        <div class="pathbar"><code>{{ browsePath || "（进入备份盘后浏览）" }}</code></div>
        <div class="toolbar single-row">
          <button class="btn small" title="回到备份盘根" :disabled="browseBusy || !drive" @click="browseRoot">盘根</button>
          <button class="btn small" title="上一级" :disabled="browseBusy || !browsePath" @click="browseUp">上级</button>
          <button class="btn small" title="刷新" :disabled="browseBusy" @click="loadBrowse(browsePath || drive)">刷新</button>
          <button class="btn small primary-outline" title="将勾选的目录/文件解析为受控项并勾选" :disabled="jobRunning || !isBackupDisk" @click="applyBrowseSelection">应用到校验范围</button>
          <button class="btn small" title="清除范围，校验全部受控文件" @click="clearPathFilter">清除范围</button>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th><th style="width:60px">受控</th></tr></thead>
            <tbody>
              <tr v-for="e in browseEntries" :key="e.path" :class="{ selected: browseSelected.has(e.path) }" @dblclick="openBrowseEntry(e)">
                <td><input type="checkbox" :checked="browseSelected.has(e.path)" @change="toggleBrowse(e.path)" /></td>
                <td class="name" @click="e.is_dir ? openBrowseEntry(e) : toggleBrowse(e.path)">{{ e.is_dir ? "📁" : "📄" }} {{ e.name }}</td>
                <td>{{ e.is_dir ? "目录" : "文件" }}</td>
                <td><span v-if="e.is_controlled" class="badge controlled">受控</span></td>
              </tr>
              <tr v-if="!browseEntries.length"><td colspan="4" class="muted center">无已受控项或未进入备份盘</td></tr>
            </tbody>
          </table>
        </div>
        <div class="row" style="margin-top:10px">
          <button class="btn primary" title="将当前勾选目录/文件应用到范围后做快速校验（FastMD5）" :disabled="jobRunning || !isBackupDisk" @click="verifyFromBrowse('quick')">快速校验</button>
          <button class="btn" title="将当前勾选目录/文件应用到范围后做完整 MD5 校验" :disabled="jobRunning || !isBackupDisk" @click="verifyFromBrowse('full')">完整校验</button>
        </div>
      </section>
      <section class="panel">
        <h2>受控文件校验</h2>
        <p class="muted small">
          列表仅含 vault.db 中的受控文件。
          <span v-if="pathFilterActive">当前范围 {{ selectedControlled.size }} 项；</span>
          未勾选则校验全部。完整 = 全文 MD5；快速 = FastMD5。
        </p>
        <div class="row">
          <button class="btn primary" title="按库中 FastMD5 快速校验所选（或全部）受控文件" :disabled="jobRunning || !isBackupDisk" @click="doVerifyControlled('quick')">快速校验</button>
          <button class="btn" title="按库中完整 MD5 校验所选（或全部）受控文件" :disabled="jobRunning || !isBackupDisk" @click="doVerifyControlled('full')">完整校验</button>
          
          <button v-if="jobRunning" class="btn" title="取消正在进行的校验任务" @click="doCancel">取消</button>
        </div>
        <div class="toolbar tight">
          <button class="btn small" @click="selectAllDisplayed">全选当前列表</button>
          <button class="btn small" @click="selectedControlled = new Set(); pathFilterActive = false">清空勾选</button>
          <span class="muted">已勾选 {{ selectedControlled.size }} / 共 {{ controlledFiles.length }}</span>
        </div>
        <ul class="result-list controlled">
          <li v-for="f in displayedControlled" :key="f.rel_path">
            <label class="ctrl-row">
              <input type="checkbox" :checked="selectedControlled.has(f.rel_path)" @change="toggleControlled(f.rel_path)" />
              <div>
                <div class="rel">{{ f.rel_path }}</div>
                <div class="hash">MD5 {{ f.md5 }}</div>
                <div class="hash">Fast {{ f.fast_md5 }}</div>
              </div>
            </label>
          </li>
          <li v-if="!displayedControlled.length" class="muted center">暂无受控文件</li>
        </ul>
        <div v-if="controlledVerify" class="verify-summary">
          <p>{{ controlledVerify.mode === "full" ? "完整" : "快速" }}：通过
            <strong class="pass">{{ controlledVerify.passed }}</strong> / 失败
            <strong class="fail">{{ controlledVerify.failed }}</strong> / 缺失
            {{ controlledVerify.missing }} / 错误 {{ controlledVerify.errors }}</p>
          <ul class="result-list">
            <li v-for="(it, i) in controlledVerify.items" :key="i" :class="{ ok: it.status === 'pass', bad: it.status !== 'pass' }">
              <div class="rel">{{ it.rel_path }}</div>
              <div class="msg">{{ it.status }} — {{ it.message }}</div>
              <div v-if="it.expected" class="hash">期望 {{ it.expected }}</div>
              <div v-if="it.actual" class="hash">实际 {{ it.actual }}</div>
            </li>
          </ul>
        </div>
      </section>
      <section class="panel">
        <h2>批次校验</h2>
        <label class="field"><span>备份批次</span>
          <select v-model="verifyBatchId">
            <option disabled value="">请选择批次</option>
            <option v-for="id in batchIds" :key="id" :value="id">{{ id }}</option>
          </select>
        </label>
        <div class="row">
          <button class="btn primary" title="对所选批次做快速校验" :disabled="jobRunning" @click="doVerifyBatch('quick')">快速校验</button>
          <button class="btn" title="对所选批次做完整校验" :disabled="jobRunning" @click="doVerifyBatch('full')">完整校验</button>
          
        </div>
        <div v-if="verifyReport" class="verify-summary">
          <p>{{ verifyReport.mode === "full" ? "完整" : "快速" }}：通过
            <strong class="pass">{{ verifyReport.passed }}</strong> / 失败
            <strong class="fail">{{ verifyReport.failed }}</strong></p>
          <ul class="result-list">
            <li v-for="(it, i) in verifyReport.items" :key="i" :class="{ ok: it.ok, bad: !it.ok }">
              <div class="rel">{{ it.rel_path }}</div>
              <div class="msg">{{ it.message }}</div>
              <div v-if="it.src_hash" class="hash">源 {{ it.src_hash }}</div>
              <div v-if="it.dest_hash" class="hash">目标 {{ it.dest_hash }}</div>
            </li>
          </ul>
        </div>
      </section>
    </div>
  </div>
</template>
<style scoped>
.app { min-height:100vh; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:14px 16px 20px; box-sizing:border-box; }
.header { display:flex; justify-content:space-between; align-items:flex-start; margin-bottom:10px; }
h1 { margin:0; font-size:1.25rem; } .sub { color:#7aa2ff; font-weight:500; font-size:0.9rem; }
.hint { margin:4px 0 0; color:#9aa7b8; font-size:0.82rem; }
.banner { padding:8px 12px; border-radius:8px; margin-bottom:8px; font-size:0.88rem; }
.banner.error { background:#3a1515; color:#ffb4b4; border:1px solid #7a2e2e; }
.banner.ok { background:#14301f; color:#b6f0c8; border:1px solid #2d6a45; }
.banner.progress { background:#152038; color:#c5d4ff; border:1px solid #2f5bff; }
.progress-meta { display:flex; justify-content:space-between; font-size:0.85rem; margin-bottom:6px; }
.progress-track { height:8px; background:#0f1419; border-radius:999px; overflow:hidden; }
.progress-fill { height:100%; background:linear-gradient(90deg,#2f5bff,#6d9bff); }
.progress-file { margin-top:6px; font-size:0.78rem; color:#9aa7b8; word-break:break-all; }
.layout { display:grid; grid-template-columns:1fr 1fr 1fr; gap:12px; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; max-height:calc(100vh - 140px); overflow:auto; }
h2 { margin:0 0 8px; font-size:0.95rem; }
.field { display:flex; flex-direction:column; gap:4px; margin-bottom:10px; font-size:0.8rem; color:#9aa7b8; }
select { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.row { display:flex; gap:8px; flex-wrap:wrap; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; flex:1; }
.btn:disabled { opacity:0.5; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.primary-outline { border-color:#2f5bff; color:#9db4ff; background:transparent; flex:0; }
.btn.ghost { background:transparent; flex:0; width:auto; }
.btn.small { padding:4px 8px; font-size:0.78rem; flex:0; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.badge { font-size:0.72rem; background:#1f6b45; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.controlled { background:#5b3db8; margin-left:0; }
.pathbar { margin-bottom:6px; } .pathbar code { display:block; background:#0f1419; padding:6px 8px; border-radius:6px; border:1px solid #2a3442; font-size:0.78rem; word-break:break-all; }
.toolbar { display:flex; gap:6px; flex-wrap:wrap; margin-bottom:8px; align-items:center; }
.toolbar.single-row { flex-wrap:nowrap; overflow-x:auto; white-space:nowrap; }
.toolbar.single-row .btn { flex:0 0 auto; white-space:nowrap; }
.toolbar.tight { margin-top:8px; }
.table-wrap { border:1px solid #2a3442; border-radius:8px; max-height:220px; overflow:auto; }
table { width:100%; border-collapse:collapse; font-size:0.8rem; }
th, td { padding:6px 8px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; position:sticky; top:0; }
tr.selected { background:#1e2a40; } .name { cursor:pointer; }
.result-list { list-style:none; padding:0; margin:8px 0 0; max-height:280px; overflow:auto; }
.result-list.controlled { max-height:200px; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.ctrl-row { display:flex; gap:8px; align-items:flex-start; cursor:pointer; }
.rel { font-weight:600; } .hash { font-family:ui-monospace,Consolas,monospace; color:#9aa7b8; word-break:break-all; }
.verify-summary { margin-top:10px; }
@media (max-width:1100px) { .layout { grid-template-columns:1fr; } }
</style>
