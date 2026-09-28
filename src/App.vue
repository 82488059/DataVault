<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  is_backup_disk: boolean;
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

interface VerifyItem {
  rel_path: string;
  src_path: string;
  dest_path: string;
  src_hash: string | null;
  dest_hash: string | null;
  ok: boolean;
  message: string;
}

interface VerifyReport {
  batch_id: string;
  mode: string;
  items: VerifyItem[];
  passed: number;
  failed: number;
}

interface ControlledFile {
  rel_path: string;
  size: number;
  mtime: number;
  md5: string;
  fast_md5: string;
  sample_ratio: number;
  sample_chunk_mb: number;
  updated_at: string;
}

interface ControlledVerifyItem {
  rel_path: string;
  status: string;
  message: string;
  expected: string | null;
  actual: string | null;
  size_changed: boolean;
  mtime_changed: boolean;
}


interface JobStart {
  job_id: string;
  total: number;
  kind: string;
}

interface JobProgress {
  job_id: string;
  phase: string;
  current: number;
  total: number;
  rel_path: string | null;
  message: string;
}

interface JobFinished {
  job_id: string;
  kind: string;
  ok: boolean;
  cancelled: boolean;
  added: number;
  failed: number;
  total: number;
  message: string;
}

interface ControlledVerifyReport {
  drive_root: string;
  mode: string;
  items: ControlledVerifyItem[];
  passed: number;
  failed: number;
  missing: number;
  errors: number;
}

const currentPath = ref("");
const entries = ref<DirEntryInfo[]>([]);
const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);
const jobRunning = ref(false);
const jobProgress = ref<JobProgress | null>(null);
let jobUnlisteners: UnlistenFn[] = [];

const destPath = ref("");
const lastBatch = ref<BackupBatch | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const verifyReport = ref<VerifyReport | null>(null);

const controlledFiles = ref<ControlledFile[]>([]);
const controlledVerify = ref<ControlledVerifyReport | null>(null);
const selectedControlled = ref<Set<string>>(new Set());

const pathLabel = computed(() => currentPath.value || "此电脑（盘符）");

const currentDrive = computed(() => {
  const m = currentPath.value.match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
});

const backupDriveSet = ref<Set<string>>(new Set());
const currentIsBackup = ref(false);

function isDriveRootPath(p: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test((p || "").trim());
}

function formatSize(n: number | null | undefined): string {
  if (n == null || typeof n !== "number" || !Number.isFinite(n) || n < 0) return "-";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

async function refreshBackupDrives() {
  try {
    const drives = await invoke<{ path: string; is_backup_disk: boolean }[]>("list_drives");
    const next = new Set<string>();
    for (const d of drives) {
      if (d.is_backup_disk) next.add(d.path.toUpperCase());
    }
    backupDriveSet.value = next;
    if (currentDrive.value) {
      currentIsBackup.value = next.has(currentDrive.value.toUpperCase());
    }
  } catch {
    /* ignore */
  }
}

async function refreshBatches() {
  try {
    batchIds.value = await invoke<string[]>("list_batches");
    if (!verifyBatchId.value && batchIds.value.length) {
      verifyBatchId.value = batchIds.value[0];
    }
  } catch {
    /* 首次可能尚无目录 */
  }
}

async function refreshControlled() {
  controlledFiles.value = [];
  selectedControlled.value = new Set();
  controlledVerify.value = null;
  if (!currentDrive.value || !currentIsBackup.value) return;
  try {
    controlledFiles.value = await invoke<ControlledFile[]>("list_controlled_files", {
      drive: currentDrive.value,
    });
  } catch (e) {
    // not a backup disk or empty
    controlledFiles.value = [];
  }
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
    if (!path) {
      currentIsBackup.value = false;
      controlledFiles.value = [];
    } else {
      const drive = path.match(/^([A-Za-z]:)/)?.[1]?.toUpperCase() + "\\";
      currentIsBackup.value = !!drive && backupDriveSet.value.has(drive);
      await refreshControlled();
    }
  } catch (e) {
    errorMsg.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function goRoot() {
  await loadDir("");
}

async function goUp() {
  if (!currentPath.value) return;
  const p = currentPath.value.replace(/[\\/]+$/, "");
  const m = p.match(/^([A-Za-z]:)(?:\\|$)/);
  if (m && (p === m[1] || p === m[1] + "\\")) {
    await goRoot();
    return;
  }
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (idx <= 2) {
    await loadDir(p.slice(0, 3));
  } else {
    await loadDir(p.slice(0, idx));
  }
}

function toggleSelect(path: string) {
  const next = new Set(selected.value);
  if (next.has(path)) next.delete(path);
  else next.add(path);
  selected.value = next;
}

function selectAllFiles() {
  const next = new Set<string>();
  for (const e of entries.value) next.add(e.path);
  selected.value = next;
}

async function openEntry(e: DirEntryInfo) {
  if (e.is_dir) await loadDir(e.path);
}

function useCurrentAsDest() {
  if (currentPath.value) destPath.value = currentPath.value;
}

async function doMarkBackupDisk() {
  errorMsg.value = "";
  statusMsg.value = "";
  // Prefer current drive; at root, use single selected drive
  let target = currentDrive.value;
  if (!currentPath.value) {
    if (selected.value.size !== 1) {
      errorMsg.value = "请在盘符列表中勾选一个盘，或先进入该盘";
      return;
    }
    target = Array.from(selected.value)[0];
  }
  if (!target) {
    errorMsg.value = "请先进入要标记的盘符";
    return;
  }
  busy.value = true;
  statusMsg.value = "正在标记受控盘…";
  try {
    await invoke("mark_backup_disk", { drive: target });
    statusMsg.value = `已标记受控盘：${target}`;
    await refreshBackupDrives();
    currentIsBackup.value = true;
    await refreshControlled();
    if (!currentPath.value) await loadDir("");
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

async function doAddControlled() {
  errorMsg.value = "";
  statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "请先将当前盘标记为受控";
    return;
  }
  if (selected.value.size === 0) {
    errorMsg.value = "请勾选要登记的文件或目录";
    return;
  }
  if (jobRunning.value) {
    errorMsg.value = "已有任务在进行中";
    return;
  }
  const drive = currentDrive.value.toUpperCase();
  for (const p of selected.value) {
    if (!p.toUpperCase().startsWith(drive.replace(/\$/, ""))) {
      errorMsg.value = "受控文件必须位于当前受控盘上";
      return;
    }
  }
  try {
    jobRunning.value = true;
    jobProgress.value = null;
    statusMsg.value = "已启动后台登记（计算 MD5 / FastMD5）…";
    const start = await invoke<JobStart>("start_add_controlled_files", {
      drive: currentDrive.value,
      paths: Array.from(selected.value),
    });
    statusMsg.value = `后台任务 ${start.job_id} 已开始`;
  } catch (e) {
    jobRunning.value = false;
    errorMsg.value = String(e);
    statusMsg.value = "";
  }
}

async function doIndexBackupDisk() {
  errorMsg.value = "";
  statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "请先进入已标记的受控盘";
    return;
  }
  if (jobRunning.value) {
    errorMsg.value = "已有任务在进行中";
    return;
  }
  try {
    jobRunning.value = true;
    jobProgress.value = null;
    statusMsg.value = "正在扫描受控盘并建立索引…";
    const start = await invoke<JobStart>("start_index_backup_disk", {
      drive: currentDrive.value,
    });
    statusMsg.value = `索引任务 ${start.job_id} 已开始（后台计算哈希）`;
  } catch (e) {
    jobRunning.value = false;
    errorMsg.value = String(e);
    statusMsg.value = "";
  }
}

async function doCancelJob() {
  try {
    await invoke<boolean>("cancel_controlled_job");
    statusMsg.value = "正在取消…";
  } catch (e) {
    errorMsg.value = String(e);
  }
}

function toggleControlled(rel: string) {
  const next = new Set(selectedControlled.value);
  if (next.has(rel)) next.delete(rel);
  else next.add(rel);
  selectedControlled.value = next;
}

async function doVerifyControlled(mode: "full" | "quick") {
  errorMsg.value = "";
  statusMsg.value = "";
  if (!currentIsBackup.value || !currentDrive.value) {
    errorMsg.value = "当前不是受控盘";
    return;
  }
  busy.value = true;
  statusMsg.value = mode === "full" ? "完整校验进行中…" : "快速校验（FastMD5）进行中…";
  try {
    const relPaths =
      selectedControlled.value.size > 0 ? Array.from(selectedControlled.value) : null;
    const cmd = mode === "full" ? "verify_controlled_full" : "verify_controlled_quick";
    controlledVerify.value = await invoke<ControlledVerifyReport>(cmd, {
      drive: currentDrive.value,
      relPaths,
    });
    const r = controlledVerify.value;
    statusMsg.value = `${mode === "full" ? "完整" : "快速"}校验完成：通过 ${r.passed}，失败 ${r.failed}，缺失 ${r.missing}，错误 ${r.errors}`;
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

async function doBackup() {
  errorMsg.value = "";
  statusMsg.value = "";
  if (selected.value.size === 0) {
    errorMsg.value = "请先勾选要备份的文件或文件夹";
    return;
  }
  if (!destPath.value.trim()) {
    errorMsg.value = "请填写备份目标目录";
    return;
  }
  busy.value = true;
  statusMsg.value = "正在备份…";
  try {
    const batch = await invoke<BackupBatch>("backup_paths", {
      sources: Array.from(selected.value),
      dest: destPath.value.trim(),
    });
    lastBatch.value = batch;
    verifyBatchId.value = batch.id;
    const ok = batch.files.filter((f) => !f.error).length;
    const bad = batch.files.filter((f) => f.error).length;
    statusMsg.value = `备份完成：批次 ${batch.id}，成功 ${ok}，失败 ${bad}`;
    await refreshBatches();
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

async function doVerify(mode: "full" | "quick") {
  errorMsg.value = "";
  statusMsg.value = "";
  if (!verifyBatchId.value) {
    errorMsg.value = "请选择要校验的批次";
    return;
  }
  busy.value = true;
  statusMsg.value = mode === "full" ? "完整校验进行中…" : "快速校验进行中…";
  try {
    verifyReport.value = await invoke<VerifyReport>("verify_backup", {
      batchId: verifyBatchId.value,
      mode,
    });
    statusMsg.value = `${mode === "full" ? "完整" : "快速"}校验完成：通过 ${verifyReport.value.passed}，失败 ${verifyReport.value.failed}`;
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  const u1 = await listen<JobProgress>("controlled-job-progress", (ev) => {
    jobProgress.value = ev.payload;
    jobRunning.value = true;
    statusMsg.value = ev.payload.message;
  });
  const u2 = await listen<JobFinished>("controlled-job-finished", async (ev) => {
    jobRunning.value = false;
    jobProgress.value = null;
    statusMsg.value = ev.payload.message;
    if (!ev.payload.ok && !ev.payload.cancelled) {
      errorMsg.value = ev.payload.message;
    }
    await refreshControlled();
  });
  jobUnlisteners = [u1, u2];
  await goRoot();
  await refreshBatches();
});

onUnmounted(() => {
  for (const u of jobUnlisteners) u();
  jobUnlisteners = [];
});
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>数据管理 <span class="sub">DataVault</span></h1>
        <p class="hint">
          受控盘 · 受控文件 · 完整 MD5 / FastMD5 校验
          <span v-if="currentIsBackup" class="badge backup">受控盘 {{ currentDrive }}</span>
        </p>
      </div>
      <div class="header-actions">
        <button class="btn ghost" :disabled="busy" @click="goRoot">盘符</button>
        <button class="btn ghost" :disabled="busy || !currentPath" @click="goUp">上级</button>
        <button class="btn ghost" :disabled="busy" @click="loadDir(currentPath)">刷新</button>
      </div>
    </header>

    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-if="statusMsg" class="banner ok">{{ statusMsg }}</div>
    <div v-if="jobRunning && jobProgress" class="banner progress">
      <div class="progress-meta">
        <span>{{ jobProgress.phase === "scanning" ? "扫描中" : "哈希中" }}</span>
        <span v-if="jobProgress.total">{{ jobProgress.current }} / {{ jobProgress.total }}</span>
      </div>
      <div class="progress-track">
        <div
          class="progress-fill"
          :style="{
            width: jobProgress.total
              ? Math.min(100, (100 * jobProgress.current) / jobProgress.total) + '%'
              : '15%',
          }"
        ></div>
      </div>
      <div v-if="jobProgress.rel_path" class="progress-file">{{ jobProgress.rel_path }}</div>
    </div>

    <div class="layout">
      <section class="panel explorer">
        <div class="pathbar">
          <span class="label">当前位置</span>
          <code>{{ pathLabel }}</code>
        </div>
        <div class="toolbar">
          <button class="btn small" @click="selectAllFiles">全选</button>
          <button class="btn small" @click="selected = new Set()">清空选择</button>
          <button class="btn small" @click="useCurrentAsDest">将当前目录设为目标</button>
          <button class="btn small primary-outline" :disabled="busy" @click="doMarkBackupDisk">
            标记为受控
          </button>
          <button
            class="btn small primary-outline"
            :disabled="busy || jobRunning || !currentIsBackup"
            @click="doAddControlled"
          >
            添加受控文件
          </button>
          <button
            class="btn small primary-outline"
            :disabled="jobRunning || !currentIsBackup"
            @click="doIndexBackupDisk"
            title="扫描当前受控盘上已有文件，计算 MD5/FastMD5 写入 vault.db"
          >
            建立备份索引
          </button>
          <button
            v-if="jobRunning"
            class="btn small"
            @click="doCancelJob"
          >
            取消任务
          </button>
          <span class="muted">已选 {{ selected.size }} 项</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th style="width: 36px"></th>
                <th>名称</th>
                <th style="width: 80px">类型</th>
                <th style="width: 100px">大小</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="e in entries"
                :key="e.path"
                :class="{ selected: selected.has(e.path) }"
                @dblclick="openEntry(e)"
              >
                <td>
                  <input
                    type="checkbox"
                    :checked="selected.has(e.path)"
                    @change="toggleSelect(e.path)"
                  />
                </td>
                <td class="name" @click="e.is_dir ? openEntry(e) : toggleSelect(e.path)">
                  <span class="icon">{{ e.is_dir || isDriveRootPath(e.path) ? "📁" : "📄" }}</span>
                  {{ e.name }}
                </td>
                <td>{{ e.is_dir || isDriveRootPath(e.path) ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir || isDriveRootPath(e.path) ? "—" : formatSize(e.size) }}</td>
              </tr>
              <tr v-if="!entries.length">
                <td colspan="4" class="muted center">空目录或无法访问</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <aside class="side">
        <section class="panel">
          <h2>受控文件 <span v-if="currentIsBackup" class="badge backup">受控盘</span></h2>
          <p v-if="!currentIsBackup" class="muted small">
            进入盘符后点击「标记为受控」，再勾选文件「添加受控文件」。
          </p>
          <template v-else>
            <p class="muted small">
              清单随盘保存在 {{ currentDrive }}.datavault\vault.db；完整 = 全文件 MD5；快速 =
              FastMD5（每 100MB 取前 10%）
            </p>
            <div class="row">
              <button class="btn primary" :disabled="busy" @click="doVerifyControlled('full')">
                完整校验
              </button>
              <button class="btn" :disabled="busy" @click="doVerifyControlled('quick')">
                快速校验
              </button>
            </div>
            <ul class="result-list controlled">
              <li v-for="f in controlledFiles" :key="f.rel_path">
                <label class="ctrl-row">
                  <input
                    type="checkbox"
                    :checked="selectedControlled.has(f.rel_path)"
                    @change="toggleControlled(f.rel_path)"
                  />
                  <div>
                    <div class="rel">{{ f.rel_path }}</div>
                    <div class="hash">MD5 {{ f.md5 }}</div>
                    <div class="hash">Fast {{ f.fast_md5 }}</div>
                  </div>
                </label>
              </li>
              <li v-if="!controlledFiles.length" class="muted center">暂无受控文件</li>
            </ul>
            <div v-if="controlledVerify" class="verify-summary">
              <p>
                {{ controlledVerify.mode === "full" ? "完整" : "快速" }}： 通过
                <strong class="pass">{{ controlledVerify.passed }}</strong> / 失败
                <strong class="fail">{{ controlledVerify.failed }}</strong> / 缺失
                {{ controlledVerify.missing }} / 错误 {{ controlledVerify.errors }}
              </p>
              <ul class="result-list">
                <li
                  v-for="(it, i) in controlledVerify.items"
                  :key="i"
                  :class="{
                    ok: it.status === 'pass',
                    bad: it.status !== 'pass',
                  }"
                >
                  <div class="rel">{{ it.rel_path }}</div>
                  <div class="msg">{{ it.status }} — {{ it.message }}</div>
                  <div v-if="it.expected" class="hash">期望 {{ it.expected }}</div>
                  <div v-if="it.actual" class="hash">实际 {{ it.actual }}</div>
                </li>
              </ul>
            </div>
          </template>
        </section>

        <section class="panel">
          <h2>备份（批次）</h2>
          <label class="field">
            <span>目标目录</span>
            <input v-model="destPath" type="text" placeholder="例如 E:\Backup\DataVault" />
          </label>
          <button class="btn primary" :disabled="busy" @click="doBackup">开始备份</button>
          <p v-if="lastBatch" class="muted small">
            最近批次：<strong>{{ lastBatch.id }}</strong>（{{ lastBatch.files.length }} 个文件）
          </p>
        </section>

        <section class="panel">
          <h2>批次校验</h2>
          <label class="field">
            <span>备份批次</span>
            <select v-model="verifyBatchId">
              <option disabled value="">请选择批次</option>
              <option v-for="id in batchIds" :key="id" :value="id">{{ id }}</option>
            </select>
          </label>
          <div class="row">
            <button class="btn primary" :disabled="busy" @click="doVerify('full')">完整校验</button>
            <button class="btn" :disabled="busy" @click="doVerify('quick')">快速校验</button>
          </div>
          <p class="muted small">批次快速校验仍为头/尾 64KB（旧路径）；受控文件请用上方 FastMD5。</p>
        </section>

        <section v-if="verifyReport" class="panel results">
          <h2>
            批次校验结果
            <span class="badge">{{ verifyReport.mode === "full" ? "完整" : "快速" }}</span>
          </h2>
          <p>
            通过 <strong class="pass">{{ verifyReport.passed }}</strong> / 失败
            <strong class="fail">{{ verifyReport.failed }}</strong>
          </p>
          <ul class="result-list">
            <li v-for="(it, i) in verifyReport.items" :key="i" :class="{ ok: it.ok, bad: !it.ok }">
              <div class="rel">{{ it.rel_path }}</div>
              <div class="msg">{{ it.message }}</div>
              <div v-if="it.src_hash" class="hash">源 {{ it.src_hash }}</div>
              <div v-if="it.dest_hash" class="hash">目标 {{ it.dest_hash }}</div>
            </li>
          </ul>
        </section>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.app {
  min-height: 100vh;
  background: #0f1419;
  color: #e7ecf3;
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
  padding: 16px 20px 24px;
  box-sizing: border-box;
}
.header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  margin-bottom: 12px;
}
h1 {
  margin: 0;
  font-size: 1.4rem;
  font-weight: 700;
}
.sub {
  color: #7aa2ff;
  font-weight: 500;
  font-size: 0.95rem;
}
.hint {
  margin: 4px 0 0;
  color: #9aa7b8;
  font-size: 0.85rem;
}
.header-actions {
  display: flex;
  gap: 8px;
}
.banner {
  padding: 8px 12px;
  border-radius: 8px;
  margin-bottom: 10px;
  font-size: 0.9rem;
}
.banner.error {
  background: #3a1515;
  color: #ffb4b4;
  border: 1px solid #7a2e2e;
}
.banner.progress {
  background: #152038;
  color: #c5d4ff;
  border: 1px solid #2f5bff;
}
.progress-meta {
  display: flex;
  justify-content: space-between;
  font-size: 0.85rem;
  margin-bottom: 6px;
}
.progress-track {
  height: 8px;
  background: #0f1419;
  border-radius: 999px;
  overflow: hidden;
}
.progress-fill {
  height: 100%;
  background: linear-gradient(90deg, #2f5bff, #6d9bff);
  transition: width 0.2s ease;
}
.progress-file {
  margin-top: 6px;
  font-size: 0.8rem;
  color: #9aa7b8;
  word-break: break-all;
}
.banner.ok {
  background: #14301f;
  color: #b6f0c8;
  border: 1px solid #2d6a45;
}
.layout {
  display: grid;
  grid-template-columns: 1fr 360px;
  gap: 14px;
  min-height: calc(100vh - 120px);
}
.panel {
  background: #171d25;
  border: 1px solid #2a3442;
  border-radius: 12px;
  padding: 12px;
}
.explorer {
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.pathbar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 8px;
}
.pathbar .label {
  color: #9aa7b8;
  font-size: 0.8rem;
}
.pathbar code {
  flex: 1;
  background: #0f1419;
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px solid #2a3442;
  font-size: 0.85rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.toolbar {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
  flex-wrap: wrap;
}
.table-wrap {
  flex: 1;
  overflow: auto;
  border: 1px solid #2a3442;
  border-radius: 8px;
}
table {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.9rem;
}
th,
td {
  padding: 8px 10px;
  text-align: left;
  border-bottom: 1px solid #243041;
}
th {
  background: #1c2430;
  color: #9aa7b8;
  font-weight: 600;
  position: sticky;
  top: 0;
}
tr.selected {
  background: #1e2a40;
}
tr:hover {
  background: #1a222e;
}
.name {
  cursor: pointer;
  user-select: none;
}
.icon {
  margin-right: 6px;
}
.side {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: calc(100vh - 120px);
  overflow: auto;
}
h2 {
  margin: 0 0 10px;
  font-size: 1rem;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 10px;
  font-size: 0.85rem;
  color: #9aa7b8;
}
input[type="text"],
select {
  background: #0f1419;
  border: 1px solid #2a3442;
  color: #e7ecf3;
  border-radius: 8px;
  padding: 8px 10px;
  font-size: 0.9rem;
}
.btn {
  background: #243044;
  color: #e7ecf3;
  border: 1px solid #3a4a63;
  border-radius: 8px;
  padding: 8px 12px;
  cursor: pointer;
  font-size: 0.88rem;
}
.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.btn.primary {
  background: #2f5bff;
  border-color: #2f5bff;
  width: 100%;
  font-weight: 600;
}
.btn.primary-outline {
  border-color: #2f5bff;
  color: #9db4ff;
}
.btn.ghost {
  background: transparent;
}
.btn.small {
  padding: 4px 8px;
  font-size: 0.8rem;
}
.row {
  display: flex;
  gap: 8px;
}
.row .btn {
  flex: 1;
  width: auto;
}
.muted {
  color: #9aa7b8;
}
.small {
  font-size: 0.8rem;
}
.center {
  text-align: center;
}
.badge {
  font-size: 0.75rem;
  background: #2f5bff;
  padding: 2px 8px;
  border-radius: 999px;
  margin-left: 6px;
}
.badge.backup {
  background: #1f6b45;
}
.pass {
  color: #6dffa0;
}
.fail {
  color: #ff8f8f;
}
.result-list {
  list-style: none;
  padding: 0;
  margin: 8px 0 0;
  max-height: 220px;
  overflow: auto;
}
.result-list.controlled {
  max-height: 180px;
}
.result-list li {
  padding: 8px;
  border-radius: 8px;
  margin-bottom: 6px;
  border: 1px solid #2a3442;
  font-size: 0.8rem;
}
.result-list li.ok {
  border-color: #2d6a45;
  background: #122018;
}
.result-list li.bad {
  border-color: #7a2e2e;
  background: #201212;
}
.ctrl-row {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  cursor: pointer;
}
.rel {
  font-weight: 600;
}
.hash {
  font-family: ui-monospace, Consolas, monospace;
  color: #9aa7b8;
  word-break: break-all;
}
.verify-summary {
  margin-top: 8px;
}
@media (max-width: 960px) {
  .layout {
    grid-template-columns: 1fr;
  }
}
</style>
