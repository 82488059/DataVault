<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface DirEntry {
  name: string; path: string; is_dir: boolean; size: number;
  is_backup_disk: boolean; is_controlled: boolean;
  controlled_count?: number | null; total_files?: number | null;
}
interface JobStart { job_id: string; total: number; kind: string; }
interface JobProgress {
  job_id: string; phase: string; current: number; total: number;
  rel_path: string | null; message: string;
}
interface BackupBatch {
  id: string; created_at: string; sources: string[];
  destination_root: string; files: { error: string | null }[];
}
interface BackupJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean;
  message: string; batch: BackupBatch | null;
}

const srcPath = ref("");
const dstPath = ref("");
const srcEntries = ref<DirEntry[]>([]);

interface DirCountUpdate {
  job_id: number; path: string; controlled_count: number; total_files: number;
}
const dirCountsJobId = ref(0);
function applyDirCount(path: string, controlled_count: number, total_files: number) {
  const list = srcEntries.value;
  const i = list.findIndex((e) => e.path === path);
  if (i < 0) return;
  const next = list.slice();
  next[i] = { ...next[i], controlled_count, total_files };
  srcEntries.value = next;
    void requestDirFileCounts(srcEntries.value);
}
async function requestDirFileCounts(list: DirEntry[]) {
  const paths = list.filter((e) => e.is_dir && e.is_controlled).map((e) => e.path);
  if (!paths.length) { dirCountsJobId.value = 0; return; }
  try { dirCountsJobId.value = await invoke<number>("start_dir_file_counts", { paths }); } catch { /* ignore */ }
}

const dstEntries = ref<DirEntry[]>([]);
const srcSelected = ref<Set<string>>(new Set());
const dstSelected = ref<string>(""); // single path
const backupRoots = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const progress = ref<JobProgress | null>(null);
const lastBatch = ref<BackupBatch | null>(null);
const busy = ref(false);
const batchName = ref("");
const unlisteners: UnlistenFn[] = [];

function normDrive(p: string): string {
  const m = (p || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
}
function isDriveRoot(p: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test((p || "").trim());
}
function formatSize(n: number | null | undefined): string {
  if (n == null || typeof n !== "number" || !Number.isFinite(n) || n < 0) return "-";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

const canBackup = computed(() => srcSelected.value.size > 0 && !!dstSelected.value && !busy.value);

async function refreshBackupRoots() {
  const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
  const next = new Set<string>();
  for (const d of drives) if (d.is_backup_disk) next.add(d.path.toUpperCase());
  backupRoots.value = next;
}

async function loadSrc(path: string) {
  errorMsg.value = "";
  try {
    if (!path) {
      const drives = await invoke<DirEntry[]>("list_drives");
      // Source pane: exclude already-marked backup disks (dest side only).
      srcEntries.value = drives.filter((d) => !d.is_backup_disk);
    void requestDirFileCounts(srcEntries.value);
    } else {
      const raw = await invoke<DirEntry[]>("list_dir", { path });
      srcEntries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
    }
    srcPath.value = path;
    srcSelected.value = new Set();
  } catch (e) { errorMsg.value = String(e); }
}

/** Destination: at root only show backup disks; inside only under backup disks. */
async function loadDst(path: string) {
  errorMsg.value = "";
  try {
    await refreshBackupRoots();
    if (!path) {
      const drives = await invoke<DirEntry[]>("list_drives");
      dstEntries.value = drives.filter((d) => d.is_backup_disk);
      dstPath.value = "";
      dstSelected.value = "";
      return;
    }
    const drive = normDrive(path);
    if (!drive || ![...backupRoots.value].some((r) => r === drive || r.startsWith(drive))) {
      errorMsg.value = "目标只能选择已标记的受控盘或其子目录";
      return;
    }
    const raw = await invoke<DirEntry[]>("list_dir", { path });
    dstEntries.value = raw.filter((e) => e.is_dir && e.name.toLowerCase() !== ".datavault");
    dstPath.value = path;
    // keep selection if still under this tree; else clear / default to current folder
    if (dstSelected.value) {
      const sel = dstSelected.value.replace(/\//g, "\\").toUpperCase();
      const cur = path.replace(/\//g, "\\").toUpperCase();
      if (!sel.startsWith(cur.replace(/\\$/, "") ) && sel !== cur && sel !== cur + "\\") {
        dstSelected.value = path.endsWith("\\") || path.endsWith("/") ? path : path + "\\";
      }
    } else {
      dstSelected.value = path.endsWith("\\") || path.endsWith("/") ? path : path + "\\";
    }
  } catch (e) { errorMsg.value = String(e); }
}

function openSrc(e: DirEntry) {
  if (e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) void loadSrc(e.path);
}
function openDst(e: DirEntry) {
  if (e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) void loadDst(e.path);
}

function srcUp() {
  if (!srcPath.value) return;
  if (isDriveRoot(srcPath.value)) void loadSrc("");
  else {
    const p = srcPath.value.replace(/[\\/]+$/, "");
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    void loadSrc(i > 0 ? p.slice(0, i + 1) : "");
  }
}
function dstUp() {
  if (!dstPath.value) return;
  if (isDriveRoot(dstPath.value)) void loadDst("");
  else {
    const p = dstPath.value.replace(/[\\/]+$/, "");
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    const parent = i > 0 ? p.slice(0, i + 1) : "";
    void loadDst(parent);
  }
}

function toggleSrc(path: string) {
  const next = new Set(srcSelected.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  srcSelected.value = next;
}
function selectSrcAll() {
  srcSelected.value = new Set(srcEntries.value.map((e) => e.path));
}
function clearSrc() { srcSelected.value = new Set(); }

function pickDst(e: DirEntry) {
  // single-select: directories (or drive root) only
  if (!(e.is_dir || e.is_backup_disk || isDriveRoot(e.path))) return;
  const drive = normDrive(e.path);
  if (![...backupRoots.value].some((r) => r === drive)) {
    errorMsg.value = "目标必须是受控盘或其子目录";
    return;
  }
  dstSelected.value = e.path.endsWith("\\") || e.path.endsWith("/") ? e.path : (e.is_dir || isDriveRoot(e.path) ? e.path + (e.path.includes("/") ? "/" : "\\") : e.path);
  // normalize trailing
  if (isDriveRoot(e.path)) dstSelected.value = normDrive(e.path);
}

async function doBackup() {
  errorMsg.value = ""; statusMsg.value = ""; lastBatch.value = null;
  const sources = Array.from(srcSelected.value);
  const dest = dstSelected.value.trim();
  if (!sources.length) { errorMsg.value = "请在左侧勾选备份源"; return; }
  if (!dest) { errorMsg.value = "请在右侧单选备份目标（受控盘或子目录）"; return; }
  const drive = normDrive(dest);
  if (![...backupRoots.value].some((r) => r === drive)) {
    errorMsg.value = "目标必须位于已标记的受控盘"; return;
  }
  busy.value = true;
  try {
    statusMsg.value = "正在启动备份…";
    const start = await invoke<JobStart>("start_backup", { sources, dest, batchName: batchName.value.trim() || null });
    statusMsg.value = `备份任务 ${start.job_id} 已开始`;
  } catch (e) {
    errorMsg.value = String(e); statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  await refreshBackupRoots();
  await loadSrc("");
  await loadDst("");
  const bind = async (ev: string, fn: (p: any) => void) => {
    unlisteners.push(await listen(ev, (e) => fn(e.payload)));
  };
  await bind("backup-job-progress", (p: JobProgress) => { progress.value = p; });
  await bind("backup-job-finished", (p: BackupJobFinished) => {
    progress.value = null;
    statusMsg.value = p.message || (p.ok ? "备份完成" : "备份结束");
    if (p.batch) lastBatch.value = p.batch;
  });
  await bind("dir-counts-update", (p: DirCountUpdate) => {
    if (p.job_id !== dirCountsJobId.value) return;
    applyDirCount(p.path, p.controlled_count, p.total_files);
  });
});
onUnmounted(() => { for (const u of unlisteners) try { u(); } catch { /* */ } });
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>高级备份 <span class="sub">DataVault</span></h1>
        <p class="hint">左侧多选源（不含已标记受控盘）；右侧仅可单选受控盘或其子目录作为目标。</p>
      </div>
      <button class="btn primary" title="将左侧勾选复制到右侧所选目标" :disabled="!canBackup" @click="doBackup">开始备份</button>
    </header>

    <p v-if="errorMsg" class="banner error">{{ errorMsg }}</p>
    <p v-if="statusMsg" class="banner ok">{{ statusMsg }}</p>
    <div v-if="progress" class="banner progress">
      <div class="progress-meta">
        <span>{{ progress.phase }} · {{ progress.job_id }}</span>
        <span>{{ progress.current }} / {{ progress.total || "?" }}</span>
      </div>
      <div class="progress-track">
        <div class="progress-fill" :style="{ width: progress.total ? Math.min(100, 100 * progress.current / progress.total) + '%' : '15%' }"></div>
      </div>
      <div v-if="progress.rel_path || progress.message" class="progress-file">{{ progress.rel_path || progress.message }}</div>
    </div>

    <label class="field batch-name"><span>批次名称（可选，默认年月日时分秒）</span>
      <input v-model="batchName" type="text" placeholder="例如 项目A-全量" title="留空则使用本地时间 YYYYMMDDHHmmss 作为批次号" />
    </label>
    <div class="panes">
      <section class="panel pane">
        <h2>源（多选）</h2>
        <div class="toolbar">
          <button class="btn small" :disabled="!srcPath" @click="srcUp">上级</button>
          <button class="btn small" @click="loadSrc('')">盘符</button>
          <button class="btn small" @click="selectSrcAll">全选</button>
          <button class="btn small" @click="clearSrc">清空</button>
          <span class="muted">已选 {{ srcSelected.size }}</span>
        </div>
        <code class="path">{{ srcPath || "此电脑（盘符）" }}</code>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th>
                <th style="width:90px">大小</th><th style="width:60px">受控</th><th style="width:90px">数量</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in srcEntries" :key="'s-'+e.path" :class="{ selected: srcSelected.has(e.path) }" @dblclick="openSrc(e)">
                <td @click.stop><input type="checkbox" :checked="srcSelected.has(e.path)" @change="toggleSrc(e.path)" /></td>
                <td class="name" @click="(e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) ? openSrc(e) : toggleSrc(e.path)">
                  <span class="icon" aria-hidden="true">{{ (e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) ? "📁" : "📄" }}</span>{{ e.name || e.path }}
                </td>
                <td>{{ e.is_dir || e.is_backup_disk || isDriveRoot(e.path) ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir || e.is_backup_disk || isDriveRoot(e.path) ? "—" : formatSize(e.size) }}</td>
                <td>
                  <span v-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span>
                </td>
                <td>
                  <span v-if="e.is_dir && e.is_controlled" title="受控文件数/总文件数">{{ e.controlled_count != null && e.total_files != null ? e.controlled_count + '/' + e.total_files : '…' }}</span>
                </td>
              </tr>
              <tr v-if="!srcEntries.length"><td colspan="6" class="muted center">空目录或无法访问</td></tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="panel pane">
        <h2>目标（单选 · 仅受控盘）</h2>
        <div class="toolbar">
          <button class="btn small" :disabled="!dstPath" @click="dstUp">上级</button>
          <button class="btn small" @click="loadDst('')">受控盘符</button>
          <span class="muted">{{ dstSelected ? "已选 " + dstSelected : "未选目标" }}</span>
        </div>
        <code class="path">{{ dstPath || "受控盘列表" }}</code>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in dstEntries" :key="'d-'+e.path"
                :class="{ selected: dstSelected.replace(/[\\/]+$/, '').toUpperCase() === e.path.replace(/[\\/]+$/, '').toUpperCase() }"
                @dblclick="openDst(e)">
                <td @click.stop>
                  <input type="radio" name="dst" :checked="dstSelected.replace(/[\\/]+$/, '').toUpperCase() === e.path.replace(/[\\/]+$/, '').toUpperCase()" @change="pickDst(e)" />
                </td>
                <td class="name" @click="openDst(e)">
                  <span class="icon" aria-hidden="true">📁</span>{{ e.name || e.path }}
                </td>
                <td>文件夹</td>
              </tr>
              <tr v-if="!dstEntries.length && !dstPath">
                <td colspan="3" class="muted center">无可用受控盘。请先在主窗口「标记为受控」。</td>
              </tr>
              <tr v-if="!dstEntries.length && dstPath" class="selected dest-here-row">
                <td colspan="3" class="muted center dest-here">
                  <div>备份到此目录（当前为空）</div>
                  <strong class="dest-path">{{ dstSelected || dstPath }}</strong>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </div>

    <p v-if="lastBatch" class="muted small">最近批次：<strong>{{ lastBatch.id }}</strong>（{{ lastBatch.files.length }} 个文件）→ {{ lastBatch.destination_root }}</p>
  </div>
</template>

<style scoped>
.app { min-height:100vh; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:14px 16px 18px; box-sizing:border-box; display:flex; flex-direction:column; gap:10px; }
.header { display:flex; justify-content:space-between; align-items:center; gap:12px; }
h1 { margin:0; font-size:1.25rem; font-weight:700; }
h2 { margin:0 0 8px; font-size:0.95rem; color:#9db4ff; }
.sub { color:#7aa2ff; font-weight:500; font-size:0.95rem; }
.hint { margin:4px 0 0; color:#9aa7b8; font-size:0.85rem; }
.banner { padding:8px 12px; border-radius:8px; font-size:0.88rem; }
.banner.error { background:#3a1515; color:#ffb4b4; border:1px solid #7a2e2e; }
.banner.ok { background:#14301f; color:#b6f0c8; border:1px solid #2d6a45; }
.banner.progress { background:#152038; color:#c5d4ff; border:1px solid #2f5bff; }
.progress-meta { display:flex; justify-content:space-between; font-size:0.85rem; margin-bottom:6px; gap:8px; }
.progress-track { height:8px; background:#0f1419; border-radius:999px; overflow:hidden; }
.progress-fill { height:100%; background:linear-gradient(90deg,#2f5bff,#6d9bff); }
.progress-file { margin-top:6px; font-size:0.78rem; color:#9aa7b8; word-break:break-all; }
.field { display:flex; flex-direction:column; gap:4px; font-size:0.8rem; color:#9aa7b8; }
.field input { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.batch-name { max-width:420px; }
.panes { display:grid; grid-template-columns:1fr 1fr; gap:12px; min-height:0; flex:1; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; display:flex; flex-direction:column; min-height:0; }
.pane { max-height:calc(100vh - 200px); }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:6px; flex-wrap:wrap; }
.path { display:block; background:#0f1419; padding:6px 8px; border-radius:6px; border:1px solid #2a3442; font-size:0.8rem; margin-bottom:8px; word-break:break-all; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; min-height:180px; }
table { width:100%; border-collapse:collapse; font-size:0.88rem; }
th, td { padding:7px 9px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; }
.name { cursor:pointer; user-select:none; } .icon { margin-right:6px; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; }
.btn:disabled { opacity:0.45; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.small { padding:4px 8px; font-size:0.78rem; }
.muted { color:#9aa7b8; font-size:0.8rem; } .small { font-size:0.78rem; } .center { text-align:center; }
.badge { font-size:0.72rem; background:#2f5bff; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; margin-left:0; }
.badge.controlled { background:#1f6b45; margin-left:0; }
.dest-here { padding:14px 10px; line-height:1.5; }
.dest-here .dest-path { display:block; margin-top:6px; color:#9db4ff; word-break:break-all; font-size:0.85rem; }
.dest-here-row { background:#1e2a40; }
@media (max-width:900px) { .panes { grid-template-columns:1fr; } }
</style>
