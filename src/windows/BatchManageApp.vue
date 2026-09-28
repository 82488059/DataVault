<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface DirEntry {
  name: string; path: string; is_dir: boolean; size: number;
  is_backup_disk: boolean; is_controlled: boolean;
  controlled_count?: number | null; total_files?: number | null;
}
interface FileMeta {
  rel_path: string; src_path: string; dest_path: string;
  size: number; mtime: number; quick_md5?: string; error: string | null;
}
interface BackupBatch {
  id: string; created_at: string; sources: string[];
  destination_root: string; files: FileMeta[];
}
interface BatchRow {
  id: string; timeLabel: string; fileCount: number; drive: string; raw: BackupBatch;
}
interface JobStart { job_id: string; total: number; kind: string; }
interface JobProgress {
  job_id: string; phase: string; current: number; total: number;
  rel_path: string | null; message: string;
}
interface VerifyItem {
  rel_path: string; src_path: string; dest_path: string;
  src_hash: string | null; dest_hash: string | null; ok: boolean; message: string;
}
interface VerifyReport {
  batch_id: string; mode: string; items: VerifyItem[]; passed: number; failed: number;
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
interface VerifyJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  controlled: ControlledVerifyReport | null; batch: VerifyReport | null;
}

const rows = ref<BatchRow[]>([]);
const batchFilter = ref("");
const filteredRows = computed(() => {
  const q = batchFilter.value.trim().toLowerCase();
  if (!q) return rows.value;
  return rows.value.filter((r) => String(r.id).toLowerCase().includes(q));
});
const selected = ref<Set<string>>(new Set());
const busy = ref(false);
const errorMsg = ref("");
const statusMsg = ref("");
const progress = ref<JobProgress | null>(null);
const lastReport = ref<VerifyReport | null>(null);
const controlledReport = ref<ControlledVerifyReport | null>(null);
const unlisteners: UnlistenFn[] = [];

const dirPath = ref("");
const dirEntries = ref<DirEntry[]>([]);

interface DirCountUpdate {
  job_id: number; path: string; controlled_count: number; total_files: number;
}
const dirCountsJobId = ref(0);
function applyDirCount(path: string, controlled_count: number, total_files: number) {
  const list = dirEntries.value;
  const i = list.findIndex((e) => e.path === path);
  if (i < 0) return;
  const next = list.slice();
  next[i] = { ...next[i], controlled_count, total_files };
  dirEntries.value = next;
}
async function requestDirFileCounts(list: DirEntry[]) {
  const paths = list.filter((e) => e.is_dir && e.is_controlled).map((e) => e.path);
  if (!paths.length) { dirCountsJobId.value = 0; return; }
  try { dirCountsJobId.value = await invoke<number>("start_dir_file_counts", { paths }); } catch { /* ignore */ }
}

const dirSelected = ref<Set<string>>(new Set());

function driveOf(dest: string): string {
  const m = (dest || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : (dest || "—");
}
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
function timeOf(b: BackupBatch): string {
  const id = (b.id || "").trim();
  if (/^\d{14}$/.test(id)) {
    return `${id.slice(0, 4)}-${id.slice(4, 6)}-${id.slice(6, 8)} ${id.slice(8, 10)}:${id.slice(10, 12)}:${id.slice(12, 14)}`;
  }
  const c = b.created_at || "";
  if (/^\d+$/.test(c)) {
    const d = new Date(Number(c) * 1000);
    if (!Number.isNaN(d.getTime())) {
      const p = (n: number) => String(n).padStart(2, "0");
      return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
    }
  }
  return c || "—";
}

const hasSelection = computed(() => selected.value.size > 0);
const hasDirSelection = computed(() => dirSelected.value.size > 0);
const dirPathLabel = computed(() => dirPath.value || "受控盘列表");

async function refresh() {
  errorMsg.value = "";
  try {
    const ids = await invoke<string[]>("list_batches");
    const next: BatchRow[] = [];
    for (const id of ids) {
      try {
        const b = await invoke<BackupBatch>("load_batch", { batchId: id });
        next.push({
          id: b.id, timeLabel: timeOf(b), fileCount: b.files?.length ?? 0,
          drive: driveOf(b.destination_root), raw: b,
        });
      } catch { /* skip */ }
    }
    rows.value = next;
    const keep = new Set<string>();
    for (const id of selected.value) if (next.some((r) => r.id === id)) keep.add(id);
    selected.value = keep;
  } catch (e) { errorMsg.value = String(e); }
}

function toggle(id: string) {
  const next = new Set(selected.value);
  if (next.has(id)) next.delete(id); else next.add(id);
  selected.value = next;
}
function toggleAll() {
  if (selected.value.size === rows.value.length) selected.value = new Set();
  else selected.value = new Set(rows.value.map((r) => r.id));
}

async function loadDir(path: string) {
  errorMsg.value = "";
  try {
    if (!path) {
      const drives = await invoke<DirEntry[]>("list_dir", { path: "" });
      // Only already-marked backup disks
      dirEntries.value = drives.filter((d) => d.is_backup_disk);
      void requestDirFileCounts(dirEntries.value);
    } else {
      const raw = await invoke<DirEntry[]>("list_dir", { path });
      // Only controlled files / dirs containing controlled paths
      dirEntries.value = raw.filter(
        (e) => e.name.toLowerCase() !== ".datavault" && e.is_controlled,
      );
      void requestDirFileCounts(dirEntries.value);
    }
    dirPath.value = path;
    dirSelected.value = new Set();
  } catch (e) { errorMsg.value = String(e); }
}

function openDirEntry(e: DirEntry) {
  if (e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) void loadDir(e.path);
}
function dirUp() {
  if (!dirPath.value) return;
  if (isDriveRoot(dirPath.value)) void loadDir("");
  else {
    const p = dirPath.value.replace(/[\\/]+$/, "");
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    void loadDir(i > 0 ? p.slice(0, i + 1) : "");
  }
}
function toggleDir(path: string) {
  const next = new Set(dirSelected.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  dirSelected.value = next;
}
function selectDirAll() { dirSelected.value = new Set(dirEntries.value.map((e) => e.path)); }
function clearDirSel() { dirSelected.value = new Set(); }

function resolveDirDrive(): string {
  if (dirPath.value) return normDrive(dirPath.value);
  const drives = new Set<string>();
  for (const p of dirSelected.value) {
    const d = normDrive(p);
    if (d) drives.add(d);
  }
  if (drives.size === 1) return Array.from(drives)[0];
  return "";
}

async function doVerifyDir(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = ""; controlledReport.value = null;
  const sel = Array.from(dirSelected.value);
  if (!sel.length) { errorMsg.value = "请先在目录列表勾选要校验的目录或文件"; return; }
  const drive = resolveDirDrive();
  if (!drive) { errorMsg.value = "请勾选同一受控盘下的路径，或先进入该受控盘"; return; }
  for (const p of sel) {
    if (normDrive(p) !== drive) { errorMsg.value = "勾选的路径须属于同一受控盘"; return; }
  }
  busy.value = true;
  try {
    statusMsg.value = mode === "full" ? "按目录完整校验进行中…" : "按目录快速校验进行中…";
    const start = await invoke<JobStart>("start_verify_controlled", {
      drive, mode, relPaths: null, paths: sel,
    });
    statusMsg.value = `目录校验 ${start.job_id} 已开始（可与批次校验并行）`;
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
  finally { busy.value = false; }
}

async function doVerify(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = ""; lastReport.value = null;
  const ids = Array.from(selected.value);
  if (!ids.length) { errorMsg.value = "请先勾选要校验的批次"; return; }
  busy.value = true;
  try {
    for (const batchId of ids) {
      statusMsg.value = mode === "full"
        ? `完整校验批次 ${batchId}…`
        : `快速校验批次 ${batchId}…`;
      const start = await invoke<JobStart>("start_verify_backup", { batchId, mode });
      statusMsg.value = `批次校验 ${start.job_id} 已开始（可并行）`;
    }
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
  finally { busy.value = false; }
}

onMounted(async () => {
  await refresh();
  await loadDir("");
  const bind = async (ev: string, fn: (p: any) => void) => {
    unlisteners.push(await listen(ev, (e) => fn(e.payload)));
  };
  await bind("verify-job-progress", (p: JobProgress) => { progress.value = p; });
  await bind("verify-job-finished", (p: VerifyJobFinished) => {
    progress.value = null;
    if (p.batch) lastReport.value = p.batch;
    if (p.controlled) controlledReport.value = p.controlled;
    statusMsg.value = p.message || (p.ok ? "校验完成" : "校验结束");
    void refresh();
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
        <h1>高级校验 <span class="sub">DataVault</span></h1>
        <p class="hint">按目录校验受控文件，或勾选备份批次做快/完整校验。仅显示已标记受控盘及受控路径。</p>
      </div>
      <div class="row">
        <button class="btn small" title="刷新批次列表" @click="refresh">刷新批次</button>
      </div>
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
      <div v-if="progress.rel_path" class="progress-file">{{ progress.rel_path }}</div>
      <div v-if="progress.message" class="progress-file">{{ progress.message }}</div>
    </div>

    <div class="panes">
      <section class="panel pane">
        <h2>按目录校验（受控）</h2>
        <p class="muted small">仅列出受控盘及含受控文件的目录/文件。勾选后快速/完整校验对应受控项。</p>
        <div class="toolbar">
          <button class="btn small" :disabled="!dirPath" @click="dirUp">上级</button>
          <button class="btn small" @click="loadDir('')">受控盘符</button>
          <button class="btn small" @click="selectDirAll">全选</button>
          <button class="btn small" @click="clearDirSel">清空</button>
          <button class="btn small primary" title="对勾选路径下受控文件快速校验" :disabled="!hasDirSelection || busy" @click="doVerifyDir('quick')">快速校验</button>
          <button class="btn small" title="对勾选路径下受控文件完整校验" :disabled="!hasDirSelection || busy" @click="doVerifyDir('full')">完整校验</button>
          <span class="muted">已选 {{ dirSelected.size }}</span>
        </div>
        <code class="path">{{ dirPathLabel }}</code>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th><th style="width:70px">类型</th>
                <th style="width:90px">大小</th><th style="width:60px">受控</th><th style="width:90px">数量</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in dirEntries" :key="'d-'+e.path" :class="{ selected: dirSelected.has(e.path) }" @dblclick="openDirEntry(e)">
                <td @click.stop><input type="checkbox" :checked="dirSelected.has(e.path)" @change="toggleDir(e.path)" /></td>
                <td class="name" @click="(e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) ? openDirEntry(e) : toggleDir(e.path)">
                  <span class="icon" aria-hidden="true">{{ (e.is_dir || e.is_backup_disk || isDriveRoot(e.path)) ? "📁" : "📄" }}</span>{{ e.name || e.path }}
                </td>
                <td>{{ e.is_dir || e.is_backup_disk || isDriveRoot(e.path) ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir || e.is_backup_disk ? "—" : formatSize(e.size) }}</td>
                <td>
                  <span v-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span>
                </td>
                <td>
                  <span v-if="e.is_dir && e.is_controlled" title="受控文件数/总文件数">{{ e.controlled_count != null && e.total_files != null ? e.controlled_count + '/' + e.total_files : '…' }}</span>
                </td>
              </tr>
              <tr v-if="!dirEntries.length">
                <td colspan="6" class="muted center">{{ dirPath ? "此目录下无受控项" : "无受控盘。请先在主窗口「标记为受控」。" }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="panel pane">
        <h2>按批次校验</h2>
        <p class="muted small">勾选备份批次后执行快/完整校验。批次号为名称或本地年月日时分秒。</p>
        <label class="field"><span>过滤批次</span>
          <input type="text" v-model="batchFilter" placeholder="输入关键词过滤批次号" title="输入时过滤下方批次列表" />
        </label>
        <div class="toolbar">
          <button class="btn small" @click="toggleAll">{{ selected.size === filteredRows.length && filteredRows.length ? "清空选择" : "全选" }}</button>
          <button class="btn small primary" title="对勾选批次做快速校验" :disabled="!hasSelection || busy" @click="doVerify('quick')">快速校验</button>
          <button class="btn small" title="对勾选批次做完整校验" :disabled="!hasSelection || busy" @click="doVerify('full')">完整校验</button>
          <span class="muted">已选 {{ selected.size }} / {{ filteredRows.length }}</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th>
                <th>批次号</th>
                <th style="width:160px">时间</th>
                <th style="width:70px">文件数</th>
                <th style="width:80px">盘符</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="r in filteredRows" :key="r.id" :class="{ selected: selected.has(r.id) }">
                <td @click.stop>
                  <input type="checkbox" :checked="selected.has(r.id)" @change="toggle(r.id)" />
                </td>
                <td class="mono name" @click="toggle(r.id)">{{ r.id }}</td>
                <td>{{ r.timeLabel }}</td>
                <td>{{ r.fileCount }}</td>
                <td>{{ r.drive }}</td>
              </tr>
              <tr v-if="!rows.length">
                <td colspan="5" class="muted center">暂无备份批次。请在主窗口完成备份后再查看。</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </div>

    <section v-if="controlledReport" class="panel results">
      <h2>目录校验结果</h2>
      <p>受控 · {{ controlledReport.drive_root }} · {{ controlledReport.mode === "full" ? "完整" : "快速" }}：通过
        <strong class="pass">{{ controlledReport.passed }}</strong> / 失败
        <strong class="fail">{{ controlledReport.failed }}</strong> / 缺失
        {{ controlledReport.missing }} / 错误 {{ controlledReport.errors }}</p>
      <ul class="result-list">
        <li v-for="(it, i) in controlledReport.items" :key="'c-'+i" :class="{ ok: it.status === 'pass', bad: it.status !== 'pass' }">
          <div class="rel">{{ it.rel_path }}</div>
          <div class="msg">{{ it.status }} · {{ it.message }}</div>
        </li>
      </ul>
    </section>

    <section v-if="lastReport" class="panel results">
      <h2>批次校验结果</h2>
      <p>批次 {{ lastReport.batch_id }} · {{ lastReport.mode === "full" ? "完整" : "快速" }}：通过
        <strong class="pass">{{ lastReport.passed }}</strong> / 失败
        <strong class="fail">{{ lastReport.failed }}</strong></p>
      <ul class="result-list">
        <li v-for="(it, i) in lastReport.items" :key="'b-'+i" :class="{ ok: it.ok, bad: !it.ok }">
          <div class="rel">{{ it.rel_path }}</div>
          <div class="msg">{{ it.message }}</div>
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.app { min-height:100vh; height:100vh; overflow:hidden; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:14px 16px 18px; box-sizing:border-box; display:flex; flex-direction:column; gap:10px; }
.header { display:flex; justify-content:space-between; align-items:flex-start; gap:12px; flex-shrink:0; }
h1 { margin:0; font-size:1.25rem; font-weight:700; }
h2 { margin:0 0 6px; font-size:0.95rem; color:#9db4ff; }
.sub { color:#7aa2ff; font-weight:500; font-size:0.95rem; }
.hint { margin:4px 0 0; color:#9aa7b8; font-size:0.85rem; }
.banner { padding:8px 12px; border-radius:8px; font-size:0.88rem; flex-shrink:0; }
.banner.error { background:#3a1515; color:#ffb4b4; border:1px solid #7a2e2e; }
.banner.ok { background:#14301f; color:#b6f0c8; border:1px solid #2d6a45; }
.banner.progress { background:#152038; color:#c5d4ff; border:1px solid #2f5bff; }
.progress-meta { display:flex; justify-content:space-between; font-size:0.85rem; margin-bottom:6px; gap:8px; }
.progress-track { height:8px; background:#0f1419; border-radius:999px; overflow:hidden; }
.progress-fill { height:100%; background:linear-gradient(90deg,#2f5bff,#6d9bff); }
.progress-file { margin-top:6px; font-size:0.78rem; color:#9aa7b8; word-break:break-all; }
.panes { display:grid; grid-template-columns:1fr 1fr; gap:12px; min-height:0; flex:1; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; }
.pane { display:flex; flex-direction:column; min-height:0; overflow:hidden; }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:6px; flex-wrap:wrap; }
.path { display:block; background:#0f1419; padding:6px 8px; border-radius:6px; border:1px solid #2a3442; font-size:0.8rem; margin-bottom:8px; word-break:break-all; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; min-height:120px; }
table { width:100%; border-collapse:collapse; font-size:0.88rem; }
th, td { padding:7px 9px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; }
.name { cursor:pointer; user-select:none; } .icon { margin-right:6px; }
.mono { font-family:ui-monospace,Consolas,monospace; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; }
.btn:disabled { opacity:0.45; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.small { padding:4px 8px; font-size:0.78rem; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.badge { font-size:0.72rem; background:#2f5bff; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; margin-left:0; }
.badge.controlled { background:#1f6b45; margin-left:0; }
.results { flex-shrink:0; max-height:min(220px, 24vh); overflow:hidden; display:flex; flex-direction:column; min-height:0; }
.results .verify-summary, .results .result-block { flex:1 1 auto; min-height:0; display:flex; flex-direction:column; overflow:hidden; }
.results .result-list { flex:1 1 auto; min-height:0; max-height:140px; overflow:auto; }
.result-list { list-style:none; margin:0; padding:0; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.rel { font-weight:600; }
.row { display:flex; gap:8px; }
@media (max-width:960px) { .panes { grid-template-columns:1fr; } .app { height:auto; overflow:auto; } }
</style>
