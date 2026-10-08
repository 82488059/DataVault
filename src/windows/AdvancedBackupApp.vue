<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useVirtualList } from "../virtualList";

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
interface DirCountUpdate {
  job_id: number; path: string; controlled_count: number; total_files: number;
}

const srcPath = ref("");
const dstPath = ref("");
const srcEntries = ref<DirEntry[]>([]);
const dstEntries = ref<DirEntry[]>([]);
const {
  windowed: srcWindowed, range: srcRange, slice: shownSrc,
  onScroll: onSrcScroll, setScroller: setSrcScroller,
} = useVirtualList(srcEntries, 36, 200, srcPath);
const {
  windowed: dstWindowed, range: dstRange, slice: shownDst,
  onScroll: onDstScroll, setScroller: setDstScroller,
} = useVirtualList(dstEntries, 36, 200, dstPath);
const srcSelected = ref<Set<string>>(new Set());
const dstSelected = ref<string>("");
const backupRoots = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const progress = ref<JobProgress | null>(null);
const lastBatch = ref<BackupBatch | null>(null);
const busy = ref(false);
const batchName = ref("");
const nameRegex = ref("");
/** include = 仅匹配；exclude = 排除匹配 */
const regexMode = ref<"include" | "exclude">("include");
const packAsTar = ref(false);
/** When browsing inside a controlled .tar */
const tarBrowse = ref<{ tarPath: string; prefix: string } | null>(null);
const tarBrowseDst = ref<{ tarPath: string; prefix: string } | null>(null);

const unlisteners: UnlistenFn[] = [];
const dirCountsJobId = ref(0);

function normDrive(p: string): string {
  const m = (p || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
}
async function refreshBackupRoots() {
  const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
  const next = new Set<string>();
  for (const d of drives) {
    if (d.is_backup_disk) next.add(normDrive(d.path) || d.path.toUpperCase());
  }
  backupRoots.value = next;
}
function isControlledDrive(p: string): boolean {
  const d = normDrive(p);
  return !!d && backupRoots.value.has(d);
}
function isDriveRoot(p: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test((p || "").trim());
}
function isFolderEntry(e: DirEntry): boolean {
  return e.is_dir || e.is_backup_disk || isDriveRoot(e.path);
}
function formatSize(n: number | null | undefined): string {
  if (n == null || typeof n !== "number" || !Number.isFinite(n) || n < 0) return "-";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
function pathKey(p: string): string {
  return (p || "").replace(/\//g, "\\").replace(/[\\/]+$/, "").toUpperCase();
}
function isDstSelected(p: string): boolean {
  return !!dstSelected.value && pathKey(dstSelected.value) === pathKey(p);
}

function applyDirCount(path: string, controlled_count: number, total_files: number) {
  const si = srcEntries.value.findIndex((e) => e.path === path);
  if (si >= 0) {
    const next = srcEntries.value.slice();
    next[si] = { ...next[si], controlled_count, total_files };
    srcEntries.value = next;
  }
  const di = dstEntries.value.findIndex((e) => e.path === path);
  if (di >= 0) {
    const next = dstEntries.value.slice();
    next[di] = { ...next[di], controlled_count, total_files };
    dstEntries.value = next;
  }
}
async function requestDirFileCounts(list: DirEntry[]) {
  const paths = list.filter((e) => e.is_dir && e.is_controlled).map((e) => e.path);
  if (!paths.length) { dirCountsJobId.value = 0; return; }
  try { dirCountsJobId.value = await invoke<number>("start_dir_file_counts", { paths }); } catch { /* ignore */ }
}


function isControlledTar(e: DirEntry): boolean {
  return !e.is_dir && !!e.is_controlled && e.name.toLowerCase().endsWith(".tar");
}
function isOpenable(e: DirEntry): boolean {
  return isFolderEntry(e) || isControlledTar(e);
}

async function loadTarViewDst() {
  if (!tarBrowseDst.value) return;
  const { tarPath, prefix } = tarBrowseDst.value;
  const raw = await invoke<{ name: string; tar_path: string; rel_path: string; is_dir: boolean; size: number }[]>(
    "list_tar_entries",
    { path: tarPath, prefix: prefix || null },
  );
  dstEntries.value = raw.map((t) => ({
    name: t.name,
    path: `${t.tar_path}::${t.rel_path}`,
    is_dir: t.is_dir,
    size: t.size,
    is_backup_disk: false,
    is_controlled: true,
    controlled_count: null,
    total_files: null,
  }));
  dstPath.value = prefix ? `${tarPath} :: ${prefix}` : `${tarPath} :: /`;
}

async function loadTarView() {
  if (!tarBrowse.value) return;
  const { tarPath, prefix } = tarBrowse.value;
  const raw = await invoke<{ name: string; tar_path: string; rel_path: string; is_dir: boolean; size: number }[]>(
    "list_tar_entries",
    { path: tarPath, prefix: prefix || null },
  );
  srcEntries.value = raw.map((t) => ({
    name: t.name,
    path: `${t.tar_path}::${t.rel_path}`,
    is_dir: t.is_dir,
    size: t.size,
    is_backup_disk: false,
    is_controlled: true,
    controlled_count: null,
    total_files: null,
  }));
  srcPath.value = prefix ? `${tarPath} :: ${prefix}` : `${tarPath} :: /`;
  srcSelected.value = new Set();
}

const sameDriveConflict = computed(() => {
  if (!dstSelected.value || srcSelected.value.size === 0) return false;
  const dd = normDrive(dstSelected.value);
  if (!dd) return false;
  for (const s of srcSelected.value) {
    if (normDrive(s) === dd) return true;
  }
  return false;
});
const canBackup = computed(
  () => srcSelected.value.size > 0 && !!dstSelected.value && !busy.value && !sameDriveConflict.value,
);

async function loadSrc(path: string) {
  errorMsg.value = "";
  tarBrowse.value = null;
  try {
    if (!path) {
      srcEntries.value = await invoke<DirEntry[]>("list_dir", { path: "" });
    } else {
      const raw = await invoke<DirEntry[]>("list_dir", { path });
      srcEntries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
    }
    srcPath.value = path;
    srcSelected.value = new Set();
    void requestDirFileCounts(srcEntries.value);
  } catch (e) { errorMsg.value = String(e); }
}

async function loadDst(path: string) {
  errorMsg.value = "";
  tarBrowseDst.value = null;
  try {
    if (!path) {
      dstEntries.value = await invoke<DirEntry[]>("list_dir", { path: "" });
      dstPath.value = "";
      dstSelected.value = "";
      void requestDirFileCounts(dstEntries.value);
      return;
    }
    const raw = await invoke<DirEntry[]>("list_dir", { path });
    dstEntries.value = raw.filter((e) => e.name.toLowerCase() !== ".datavault");
    dstPath.value = path;
    if (dstSelected.value) {
      const sel = pathKey(dstSelected.value);
      const cur = pathKey(path);
      if (sel !== cur && !sel.startsWith(cur + "\\")) {
        dstSelected.value = path.endsWith("\\") || path.endsWith("/") ? path : path + "\\";
      }
    } else {
      dstSelected.value = path.endsWith("\\") || path.endsWith("/") ? path : path + "\\";
    }
    void requestDirFileCounts(dstEntries.value);
  } catch (e) { errorMsg.value = String(e); }
}

function openSrc(e: DirEntry) {
  if (tarBrowse.value && e.is_dir) {
    const rel = e.path.includes("::") ? e.path.split("::").slice(1).join("::") : e.path;
    tarBrowse.value = { tarPath: tarBrowse.value.tarPath, prefix: rel.replace(/\/+$/, "") };
    void loadTarView().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (isControlledTar(e)) {
    tarBrowse.value = { tarPath: e.path, prefix: "" };
    void loadTarView().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (isFolderEntry(e)) void loadSrc(e.path);
}
function openDst(e: DirEntry) {
  if (tarBrowseDst.value && e.is_dir) {
    const rel = e.path.includes("::") ? e.path.split("::").slice(1).join("::") : e.path;
    tarBrowseDst.value = { tarPath: tarBrowseDst.value.tarPath, prefix: rel.replace(/\/+$/, "") };
    void loadTarViewDst().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (isControlledTar(e)) {
    tarBrowseDst.value = { tarPath: e.path, prefix: "" };
    void loadTarViewDst().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (isFolderEntry(e)) void loadDst(e.path);
}

function srcUp() {
  if (tarBrowse.value) {
    const pref = tarBrowse.value.prefix.replace(/\/+$/, "");
    if (!pref) {
      const tarPath = tarBrowse.value.tarPath;
      const parent = tarPath.replace(/[\\/]+$/, "");
      const i = Math.max(parent.lastIndexOf("\\"), parent.lastIndexOf("/"));
      tarBrowse.value = null;
      void loadSrc(i > 0 ? parent.slice(0, i + 1) : "");
      return;
    }
    const i = pref.lastIndexOf("/");
    tarBrowse.value = { tarPath: tarBrowse.value.tarPath, prefix: i >= 0 ? pref.slice(0, i) : "" };
    void loadTarView().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (!srcPath.value) return;
  if (isDriveRoot(srcPath.value)) void loadSrc("");
  else {
    const p = srcPath.value.replace(/[\\/]+$/, "");
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    void loadSrc(i > 0 ? p.slice(0, i + 1) : "");
  }
}
function dstUp() {
  if (tarBrowseDst.value) {
    const pref = tarBrowseDst.value.prefix.replace(/\/+$/, "");
    if (!pref) {
      const tarPath = tarBrowseDst.value.tarPath;
      const parent = tarPath.replace(/[\\/]+$/, "");
      const i = Math.max(parent.lastIndexOf("\\"), parent.lastIndexOf("/"));
      tarBrowseDst.value = null;
      void loadDst(i > 0 ? parent.slice(0, i + 1) : "");
      return;
    }
    const i = pref.lastIndexOf("/");
    tarBrowseDst.value = { tarPath: tarBrowseDst.value.tarPath, prefix: i >= 0 ? pref.slice(0, i) : "" };
    void loadTarViewDst().catch((err) => { errorMsg.value = String(err); });
    return;
  }
  if (!dstPath.value) return;
  if (isDriveRoot(dstPath.value)) void loadDst("");
  else {
    const p = dstPath.value.replace(/[\\/]+$/, "");
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    void loadDst(i > 0 ? p.slice(0, i + 1) : "");
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
  if (!isFolderEntry(e)) return;
  if (isDriveRoot(e.path)) dstSelected.value = normDrive(e.path);
  else if (e.path.endsWith("\\") || e.path.endsWith("/")) dstSelected.value = e.path;
  else dstSelected.value = e.path + (e.path.includes("/") ? "/" : "\\");
}

async function doBackup() {
  errorMsg.value = ""; statusMsg.value = ""; lastBatch.value = null;
  const sources = Array.from(srcSelected.value);
  const dest = dstSelected.value.trim();
  if (!sources.length) { errorMsg.value = "请在左侧勾选备份源"; return; }
  if (!dest) { errorMsg.value = "请在右侧单选备份目标文件夹"; return; }
  const dd = normDrive(dest);
  for (const s of sources) {
    if (dd && normDrive(s) === dd) {
      errorMsg.value = "源与目标不能在同一盘符，请选择不同盘符";
      return;
    }
  }
  try { await refreshBackupRoots(); } catch { /* ignore */ }
  if (dd && !isControlledDrive(dest)) {
    const ok = window.confirm(
      `目标盘「${dd}」尚未标记为受控。\n\n确认标记为 DataVault 受控盘并继续备份？\n将在该盘根目录创建 .datavault 元数据目录。`
    );
    if (!ok) { statusMsg.value = "已取消"; return; }
    busy.value = true;
    try {
      statusMsg.value = "正在标记受控盘…";
      await invoke("mark_backup_disk", { drive: dd });
      await refreshBackupRoots();
      statusMsg.value = `已标记受控盘：${dd}`;
    } catch (e) {
      errorMsg.value = String(e); statusMsg.value = ""; return;
    } finally {
      busy.value = false;
    }
  }
  busy.value = true;
  try {
    statusMsg.value = "正在启动备份…";
    const start = await invoke<JobStart>("start_backup", {
      sources,
      dest,
      batchName: batchName.value.trim() || null,
      nameRegex: nameRegex.value.trim() || null,
      regexExclude: regexMode.value === "exclude",
      packAsTar: packAsTar.value,
    });
    statusMsg.value = `备份任务 ${start.job_id} 已开始`;
  } catch (e) {
    errorMsg.value = String(e); statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  try { await refreshBackupRoots(); } catch { /* ignore */ }
  await loadSrc("");
  await loadDst("");
  const bind = async (ev: string, fn: (p: any) => void) => {
    unlisteners.push(await listen(ev, (e) => fn(e.payload)));
  };
  await bind("backup-job-progress", (p: JobProgress) => { progress.value = p; });
  await bind("backup-job-finished", (p: BackupJobFinished) => {
    progress.value = null;
    statusMsg.value = "";
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
        <p class="hint">左侧多选源；右侧浏览全部文件/文件夹，仅可单选文件夹作为目标。源与目标不可同一盘符。</p>
      </div>
      <button class="btn primary" title="将左侧勾选复制到右侧所选目标文件夹" :disabled="!canBackup" @click="doBackup">开始备份</button>
    </header>

    <p v-if="errorMsg" class="banner error">{{ errorMsg }}</p>
    <p v-if="sameDriveConflict" class="banner error">源与目标在同一盘符，请更换目标或取消同盘源项。</p>
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

        <label class="field batch-name"><span>批次名称（可选；默认本地时间戳）</span>
      <input v-model="batchName" type="text" placeholder="例如 项目A-全量" title="留空则使用本地时间 YYYYMMDDHHmmss 作为批次号" />
    </label>
    <div class="adv-opts">
      <label class="field regex-field"><span>源文件名正则（可选；匹配文件名非路径）</span>
        <div class="regex-row">
          <input v-model="nameRegex" type="text" placeholder="例如 \.pdf$ 或 /\.exe$/i" title="Rust 正则，或 JS 风格 /pattern/flags（i=忽略大小写）；匹配文件名非路径" />
          <select v-model="regexMode" title="匹配=仅备份匹配项；排除=跳过匹配项">
            <option value="include">匹配</option>
            <option value="exclude">排除</option>
          </select>
        </div>
      </label>
      <label class="pack-tar" title="将选中文件打成单个 .tar；目标为受控盘时自动登记受控元数据">
        <input type="checkbox" v-model="packAsTar" />
        <span>打包为 tar</span>
      </label>
    </div>
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
        <div class="table-wrap" :class="{ windowed: srcWindowed }" :ref="setSrcScroller" @scroll="onSrcScroll">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th>
                <th style="width:90px">大小</th><th style="width:60px">受控</th><th style="width:90px">数量</th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="srcRange.padTop" class="vpad" aria-hidden="true"><td colspan="5" :style="{ height: srcRange.padTop + 'px', padding: 0, border: 'none' }"></td></tr>
              <tr v-for="e in shownSrc" :key="'s-'+e.path" :class="{ selected: srcSelected.has(e.path) }" @dblclick="openSrc(e)">
                <td @click.stop><input type="checkbox" :checked="srcSelected.has(e.path)" @change="toggleSrc(e.path)" /></td>
                <td class="name" @click="isOpenable(e) ? openSrc(e) : toggleSrc(e.path)">
                  <span class="icon" aria-hidden="true">{{ isFolderEntry(e) || isControlledTar(e) ? (isControlledTar(e) ? "📦" : "📁") : "📄" }}</span>{{ e.name || e.path }}
                </td>
                <td>{{ isFolderEntry(e) ? "—" : formatSize(e.size) }}</td>
                <td>
                  <span v-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span>
                </td>
                <td>
                  <span v-if="e.is_dir && e.is_controlled" title="受控文件数/总文件数">{{ e.controlled_count != null && e.total_files != null ? e.controlled_count + '/' + e.total_files : '…' }}</span>
                </td>
              </tr>
              <tr v-if="srcRange.padBottom" class="vpad" aria-hidden="true"><td colspan="5" :style="{ height: srcRange.padBottom + 'px', padding: 0, border: 'none' }"></td></tr>
              <tr v-if="!srcEntries.length"><td colspan="5" class="muted center">空目录或无法访问</td></tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="panel pane">
        <h2>目标（单选文件夹）</h2>
        <div class="toolbar">
          <button class="btn small" :disabled="!dstPath" @click="dstUp">上级</button>
          <button class="btn small" @click="loadDst('')">盘符</button>
          <span class="muted">{{ dstSelected ? "已选 " + dstSelected : "未选目标" }}</span>
        </div>
        <code class="path">{{ dstPath || "此电脑（盘符）" }}</code>
        <div class="table-wrap" :class="{ windowed: dstWindowed }" :ref="setDstScroller" @scroll="onDstScroll">
          <table>
            <thead>
              <tr>
                <th style="width:36px"></th><th>名称</th>
                <th style="width:90px">大小</th><th style="width:60px">受控</th><th style="width:90px">数量</th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="dstRange.padTop" class="vpad" aria-hidden="true"><td colspan="5" :style="{ height: dstRange.padTop + 'px', padding: 0, border: 'none' }"></td></tr>
              <tr v-for="e in shownDst" :key="'d-'+e.path"
                :class="{ selected: isDstSelected(e.path), disabled: !isFolderEntry(e) }"
                @dblclick="openDst(e)">
                <td @click.stop>
                  <input type="radio" name="dst" :disabled="!isFolderEntry(e)"
                    :checked="isDstSelected(e.path)" @change="pickDst(e)" />
                </td>
                <td class="name" @click="isOpenable(e) ? openDst(e) : undefined">
                  <span class="icon" aria-hidden="true">{{ isFolderEntry(e) || isControlledTar(e) ? (isControlledTar(e) ? "📦" : "📁") : "📄" }}</span>{{ e.name || e.path }}
                </td>
                <td>{{ isFolderEntry(e) ? "—" : formatSize(e.size) }}</td>
                <td>
                  <span v-if="e.is_controlled" class="badge controlled" title="已在 vault.db 登记">受控</span>
                </td>
                <td>
                  <span v-if="e.is_dir && e.is_controlled" title="受控文件数/总文件数">{{ e.controlled_count != null && e.total_files != null ? e.controlled_count + '/' + e.total_files : '…' }}</span>
                </td>
              </tr>
              <tr v-if="dstRange.padBottom" class="vpad" aria-hidden="true"><td colspan="5" :style="{ height: dstRange.padBottom + 'px', padding: 0, border: 'none' }"></td></tr>
              <tr v-if="!dstEntries.length && !dstPath">
                <td colspan="5" class="muted center">无可用盘符</td>
              </tr>
              <tr v-if="!dstEntries.length && dstPath" class="selected dest-here-row">
                <td colspan="5" class="muted center dest-here">
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
.adv-opts { display:flex; flex-wrap:wrap; gap:12px 20px; align-items:flex-end; }
.regex-field { flex:1; min-width:240px; max-width:520px; }
.regex-row { display:flex; gap:8px; }
.regex-row input { flex:1; background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.regex-row select { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.pack-tar { display:flex; align-items:center; gap:8px; font-size:0.85rem; color:#c5d4ff; cursor:pointer; padding-bottom:8px; user-select:none; }
.panes { display:grid; grid-template-columns:minmax(0, 1fr) minmax(0, 1fr); gap:12px; min-height:0; flex:1; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; display:flex; flex-direction:column; min-height:0; }
.pane { max-height:calc(100vh - 200px); min-width:0; }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:6px; flex-wrap:wrap; }
.path { display:block; background:#0f1419; padding:6px 8px; border-radius:6px; border:1px solid #2a3442; font-size:0.8rem; margin-bottom:8px; word-break:break-all; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; min-height:180px; }
table { width:100%; border-collapse:collapse; font-size:0.88rem; table-layout:fixed; }
th, td { padding:7px 9px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; }
tr.disabled { opacity:0.75; }
.name { cursor:pointer; user-select:none; word-break:break-all; overflow-wrap:anywhere; white-space:normal; } .icon { margin-right:6px; }
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