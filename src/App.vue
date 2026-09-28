<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
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

const currentPath = ref("");
const entries = ref<DirEntryInfo[]>([]);
const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);

const destPath = ref("");
const lastBatch = ref<BackupBatch | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const verifyReport = ref<VerifyReport | null>(null);

const pathLabel = computed(() => currentPath.value || "此电脑（盘符）");

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
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

async function loadDir(path: string) {
  errorMsg.value = "";
  busy.value = true;
  try {
    entries.value = await invoke<DirEntryInfo[]>("list_dir", { path });
    currentPath.value = path;
    selected.value = new Set();
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
  await goRoot();
  await refreshBatches();
});
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>数据管理 <span class="sub">DataVault</span></h1>
        <p class="hint">资源管理器浏览 · 备份到目标盘 · 完整 / 快速 MD5 校验</p>
      </div>
      <div class="header-actions">
        <button class="btn ghost" :disabled="busy" @click="goRoot">盘符</button>
        <button class="btn ghost" :disabled="busy || !currentPath" @click="goUp">上级</button>
        <button class="btn ghost" :disabled="busy" @click="loadDir(currentPath)">刷新</button>
      </div>
    </header>

    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-if="statusMsg" class="banner ok">{{ statusMsg }}</div>

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
                  <span class="icon">{{ e.is_dir ? "📁" : "📄" }}</span>
                  {{ e.name }}
                </td>
                <td>{{ e.is_dir ? "文件夹" : "文件" }}</td>
                <td>{{ e.is_dir ? "—" : formatSize(e.size) }}</td>
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
          <h2>备份</h2>
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
          <h2>校验</h2>
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
          <p class="muted small">
            完整 = 整文件 MD5；快速 = 头/尾各 64KB + 文件大小
          </p>
        </section>

        <section v-if="verifyReport" class="panel results">
          <h2>
            校验结果
            <span class="badge">{{ verifyReport.mode === "full" ? "完整" : "快速" }}</span>
          </h2>
          <p>
            通过 <strong class="pass">{{ verifyReport.passed }}</strong> /
            失败 <strong class="fail">{{ verifyReport.failed }}</strong>
          </p>
          <ul class="result-list">
            <li v-for="(it, i) in verifyReport.items" :key="i" :class="{ ok: it.ok, bad: !it.ok }">
              <div class="rel">{{ it.rel_path }}</div>
              <div class="msg">{{ it.message }}</div>
              <div v-if="it.src_hash" class="hash">源 {{ it.src_hash }}</div>
              <div v-if="it.dest_hash" class="hash">备 {{ it.dest_hash }}</div>
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
.banner.ok {
  background: #14301f;
  color: #b6f0c8;
  border: 1px solid #2d6a45;
}
.layout {
  display: grid;
  grid-template-columns: 1fr 340px;
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
  max-height: 280px;
  overflow: auto;
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
.rel {
  font-weight: 600;
}
.hash {
  font-family: ui-monospace, Consolas, monospace;
  color: #9aa7b8;
  word-break: break-all;
}
@media (max-width: 960px) {
  .layout {
    grid-template-columns: 1fr;
  }
}
</style>
