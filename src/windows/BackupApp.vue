<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  EVT_BACKUP_CTX,
  readBackupCtx,
  type BackupContext,
} from "../bridge";

interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  is_backup_disk: boolean;
  is_controlled: boolean;
}
interface FileMeta {
  rel_path: string;
  src_path: string;
  dest_path: string;
  size: number;
  md5_full: string | null;
  md5_quick: string | null;
  error: string | null;
}
interface BackupBatch {
  id: string;
  created_at: string;
  sources: string[];
  destination_root: string;
  files: FileMeta[];
}
interface JobStart { job_id: string; total: number; kind: string; }
interface JobProgress {
  job_id: string; phase: string; current: number; total: number;
  rel_path: string | null; message: string;
}
interface JobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean;
  added: number; skipped: number; failed: number; total: number; message: string;
}
interface BackupJobFinished {
  job_id: string; ok: boolean; cancelled: boolean;
  copied: number; failed: number; total: number; message: string;
  batch: BackupBatch | null;
}

const sources = ref<string[]>([]);
const destPath = ref("");
const browsePath = ref("");
const entries = ref<DirEntryInfo[]>([]);
const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);
const jobRunning = ref(false);
const jobProgress = ref<JobProgress | null>(null);
const lastBatch = ref<BackupBatch | null>(null);
let unlisteners: UnlistenFn[] = [];

const currentDrive = computed(() => {
  const m = (browsePath.value || destPath.value || sources.value[0] || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
});
const currentIsBackup = ref(false);

function applyCtx(ctx: BackupContext) {
  if (ctx.sources?.length) sources.value = [...ctx.sources];
  if (ctx.destHint) destPath.value = ctx.destHint;
  if (ctx.currentPath) browsePath.value = ctx.currentPath;
}

async function refreshBackupFlag() {
  try {
    const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
    const d = currentDrive.value.toUpperCase();
    currentIsBackup.value = drives.some((x) => x.is_backup_disk && x.path.toUpperCase() === d);
  } catch { currentIsBackup.value = false; }
}

async function loadDir(path: string) {
  errorMsg.value = "";
  busy.value = true;
  try {
    const raw = await invoke<DirEntryInfo[]>("list_dir", { path });
    entries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
    browsePath.value = path;
    selected.value = new Set();
    await refreshBackupFlag();
  } catch (e) { errorMsg.value = String(e); }
  finally { busy.value = false; }
}

async function goRoot() { await loadDir(""); }
async function goUp() {
  if (!browsePath.value) return;
  const p = browsePath.value.replace(/[\\/]+$/, "");
  const m = p.match(/^([A-Za-z]:)(?:\\|$)/);
  if (m && (p === m[1] || p === m[1] + "\\")) { await goRoot(); return; }
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (idx <= 2) await loadDir(p.slice(0, 3)); else await loadDir(p.slice(0, idx));
}
function toggleSelect(path: string) {
  const next = new Set(selected.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  selected.value = next;
}
async function openEntry(e: DirEntryInfo) { if (e.is_dir) await loadDir(e.path); }
function useSelectedAsSources() {
  if (selected.value.size === 0) { errorMsg.value = "请先勾选要作为备份源的项"; return; }
  sources.value = Array.from(selected.value);
  statusMsg.value = `已设置 ${sources.value.length} 个备份源`;
}
function useCurrentAsDest() {
  errorMsg.value = "";
  // Prefer a single checked directory; otherwise use the folder currently browsed.
  // Resolve via entries first (accurate is_dir)
  const fromEntries = entries.value.filter((e) => selected.value.has(e.path) && e.is_dir);
  if (fromEntries.length === 1) {
    destPath.value = fromEntries[0].path;
    statusMsg.value = `目标目录（勾选）：${destPath.value}`;
    return;
  }
  if (fromEntries.length > 1) {
    errorMsg.value = "请只勾选一个目录作为目标，或进入该目录后点击「设为目标目录」";
    return;
  }
  // If user checked a mix / files only, still allow current browse path
  if (!browsePath.value) {
    errorMsg.value = "请勾选一个目录，或先进入目标目录后再点「设为目标目录」";
    return;
  }
  destPath.value = browsePath.value;
  statusMsg.value = `目标目录（当前浏览）：${destPath.value}`;
}
function removeSource(p: string) { sources.value = sources.value.filter((x) => x !== p); }

async function doBackup() {
  errorMsg.value = ""; statusMsg.value = "";
  if (!sources.value.length) { errorMsg.value = "请设置备份源"; return; }
  if (!destPath.value.trim()) { errorMsg.value = "请填写备份目标目录"; return; }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null;
    statusMsg.value = "已启动后台备份…";
    const start = await invoke<JobStart>("start_backup", {
      sources: sources.value, dest: destPath.value.trim(),
    });
    statusMsg.value = `备份任务 ${start.job_id} 已开始`;
  } catch (e) { jobRunning.value = false; errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doAddControlled() {
  errorMsg.value = ""; statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "请先在备份盘目录下操作（或在主窗口标记备份盘）"; return;
  }
  const paths = selected.value.size ? Array.from(selected.value) : [...sources.value];
  if (!paths.length) { errorMsg.value = "请勾选或设置要登记的路径"; return; }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null;
    statusMsg.value = "已启动后台登记（MD5 / FastMD5）…";
    const start = await invoke<JobStart>("start_add_controlled_files", {
      drive: currentDrive.value, paths,
    });
    statusMsg.value = `登记任务 ${start.job_id} 已开始`;
  } catch (e) { jobRunning.value = false; errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doIndex() {
  errorMsg.value = ""; statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "请先进入已标记的备份盘"; return;
  }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null;
    statusMsg.value = "正在扫描备份盘并建立索引…";
    const start = await invoke<JobStart>("start_index_backup_disk", { drive: currentDrive.value });
    statusMsg.value = `索引任务 ${start.job_id} 已开始`;
  } catch (e) { jobRunning.value = false; errorMsg.value = String(e); statusMsg.value = ""; }
}

async function doCancel() {
  try {
    await invoke<boolean>("cancel_controlled_job");
    await invoke<boolean>("cancel_backup_job").catch(() => false);
    statusMsg.value = "正在取消…";
  } catch (e) { errorMsg.value = String(e); }
}

async function closeWin() {
  try { await getCurrentWebviewWindow().close(); } catch { /* ignore */ }
}

onMounted(async () => {
  const ctx = readBackupCtx();
  if (ctx) applyCtx(ctx);
  const u0 = await listen<BackupContext>(EVT_BACKUP_CTX, (ev) => applyCtx(ev.payload));
  const u1 = await listen<JobProgress>("controlled-job-progress", (ev) => {
    jobProgress.value = ev.payload; jobRunning.value = true; statusMsg.value = ev.payload.message;
  });
  const u2 = await listen<JobFinished>("controlled-job-finished", (ev) => {
    jobRunning.value = false; jobProgress.value = null; statusMsg.value = ev.payload.message;
    if (!ev.payload.ok && !ev.payload.cancelled) errorMsg.value = ev.payload.message;
  });
  const u3 = await listen<JobProgress>("backup-job-progress", (ev) => {
    jobProgress.value = ev.payload; jobRunning.value = true; statusMsg.value = ev.payload.message;
  });
  const u4 = await listen<BackupJobFinished>("backup-job-finished", (ev) => {
    jobRunning.value = false; jobProgress.value = null; statusMsg.value = ev.payload.message;
    if (ev.payload.batch) lastBatch.value = ev.payload.batch;
    if (!ev.payload.ok && !ev.payload.cancelled) errorMsg.value = ev.payload.message;
  });
  unlisteners = [u0, u1, u2, u3, u4];
  await loadDir(browsePath.value || "");
});
onUnmounted(() => { for (const u of unlisteners) u(); unlisteners = []; });
</script>
<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>备份 <span class="sub">DataVault</span></h1>
        <p class="hint">批次备份 · 受控登记 · 建立索引
          <span v-if="currentIsBackup" class="badge backup">备份盘 {{ currentDrive }}</span>
        </p>
      </div>
      <button class="btn ghost" title="关闭备份窗口" @click="closeWin">关闭</button>
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
        <h2>浏览（选源 / 目标）</h2>
        <div class="pathbar"><code>{{ browsePath || "此电脑（盘符）" }}</code></div>
        <div class="toolbar">
          <button class="btn small" title="返回盘符列表" :disabled="busy" @click="goRoot">盘符</button>
          <button class="btn small" title="返回上一级" :disabled="busy || !browsePath" @click="goUp">上级</button>
          <button class="btn small" title="刷新目录" :disabled="busy" @click="loadDir(browsePath)">刷新</button>
          <button class="btn small primary-outline" title="将当前勾选设为备份源" @click="useSelectedAsSources">设为备份源</button>
          <button class="btn small primary-outline" title="将勾选的目录（或当前浏览目录）设为备份目标" @click="useCurrentAsDest">设为目标目录</button>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th><th style="width:70px">受控</th></tr></thead>
            <tbody>
              <tr v-for="e in entries" :key="e.path" :class="{ selected: selected.has(e.path) }" @dblclick="openEntry(e)">
                <td><input type="checkbox" :checked="selected.has(e.path)" @change="toggleSelect(e.path)" /></td>
                <td class="name" @click="e.is_dir ? openEntry(e) : toggleSelect(e.path)">{{ e.is_dir ? "📁" : "📄" }} {{ e.name }}</td>
                <td>{{ e.is_dir ? "文件夹" : "文件" }}</td>
                <td><span v-if="e.is_controlled" class="badge controlled" :title="e.is_dir ? '目录下存在已受控文件' : '已在 vault.db 登记'">受控</span></td>
              </tr>
              <tr v-if="!entries.length"><td colspan="4" class="muted center">空</td></tr>
            </tbody>
          </table>
        </div>
      </section>
      <aside class="side">
        <section class="panel">
          <h2>备份批次</h2>
          <p class="muted small">源（{{ sources.length }}）</p>
          <ul class="src-list">
            <li v-for="s in sources" :key="s"><span>{{ s }}</span>
              <button class="btn small" title="从源列表移除该项" @click="removeSource(s)">×</button></li>
            <li v-if="!sources.length" class="muted">尚未设置源，可从左侧勾选后「设为备份源」</li>
          </ul>
          <label class="field"><span>目标目录</span>
            <input v-model="destPath" type="text" placeholder="例如 E:\Backup\DataVault" title="备份文件复制到此目录" />
          </label>
          <button class="btn primary" title="开始将源复制到目标目录并写入批次记录" :disabled="jobRunning" @click="doBackup">开始备份</button>
          <p v-if="lastBatch" class="muted small">最近批次：<strong>{{ lastBatch.id }}</strong>（{{ lastBatch.files.length }} 个文件）</p>
        </section>
        <section class="panel">
          <h2>受控 / 索引</h2>
          <p class="muted small">已在 vault.db 中的文件会跳过，不重复计算哈希。</p>
          <button class="btn primary-outline" title="将勾选或备份源路径登记为受控文件并计算 MD5 / FastMD5" :disabled="jobRunning || !currentIsBackup" @click="doAddControlled">添加受控文件</button>
          <button class="btn primary-outline" title="扫描当前备份盘；仅对尚未受控的文件计算 MD5/FastMD5 写入 vault.db" :disabled="jobRunning || !currentIsBackup" @click="doIndex">建立备份索引</button>
          <button v-if="jobRunning" class="btn" title="取消正在进行的后台任务" @click="doCancel">取消任务</button>
        </section>
      </aside>
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
.layout { display:grid; grid-template-columns:1fr 300px; gap:12px; min-height:calc(100vh - 130px); }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; display:flex; flex-direction:column; min-height:0; }
.side { display:flex; flex-direction:column; gap:10px; }
h2 { margin:0 0 8px; font-size:0.95rem; }
.pathbar code { display:block; background:#0f1419; padding:6px 8px; border-radius:6px; border:1px solid #2a3442; font-size:0.8rem; margin-bottom:8px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.toolbar { display:flex; gap:6px; flex-wrap:wrap; margin-bottom:8px; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; }
table { width:100%; border-collapse:collapse; font-size:0.85rem; }
th, td { padding:6px 8px; border-bottom:1px solid #243041; text-align:left; }
th { background:#1c2430; color:#9aa7b8; position:sticky; top:0; }
tr.selected { background:#1e2a40; } .name { cursor:pointer; }
.field { display:flex; flex-direction:column; gap:4px; margin:8px 0; font-size:0.8rem; color:#9aa7b8; }
input[type=text] { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; font-size:0.88rem; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; margin-top:6px; width:100%; }
.btn:disabled { opacity:0.5; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.primary-outline { border-color:#2f5bff; color:#9db4ff; background:#243044; }
.btn.ghost { background:transparent; width:auto; margin:0; }
.btn.small { padding:4px 8px; font-size:0.78rem; width:auto; margin:0; }
.src-list { list-style:none; padding:0; margin:0 0 8px; max-height:120px; overflow:auto; font-size:0.78rem; }
.src-list li { display:flex; justify-content:space-between; gap:6px; padding:4px 0; border-bottom:1px solid #243041; word-break:break-all; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.badge { font-size:0.72rem; background:#1f6b45; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; }
.badge.controlled { background:#5b3db8; margin-left:0; font-size:0.72rem; }
</style>
