<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
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

const drive = ref("");
const isBackupDisk = ref(false);
const controlledFiles = ref<ControlledFile[]>([]);
const selectedControlled = ref<Set<string>>(new Set());
const controlledVerify = ref<ControlledVerifyReport | null>(null);
const batchIds = ref<string[]>([]);
const verifyBatchId = ref("");
const verifyReport = ref<VerifyReport | null>(null);
const errorMsg = ref("");
const statusMsg = ref("");
const jobRunning = ref(false);
const jobProgress = ref<JobProgress | null>(null);
let unlisteners: UnlistenFn[] = [];

async function applyCtx(ctx: VerifyContext) {
  drive.value = ctx.drive || "";
  isBackupDisk.value = !!ctx.isBackupDisk;
  selectedControlled.value = new Set(ctx.relPaths || []);
  await refreshAll();
}

async function refreshAll() {
  controlledFiles.value = [];
  controlledVerify.value = null;
  try { batchIds.value = await invoke<string[]>("list_batches");
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
    try { controlledFiles.value = await invoke<ControlledFile[]>("list_controlled_files", { drive: drive.value }); }
    catch { controlledFiles.value = []; }
  }
}

function toggleControlled(rel: string) {
  const next = new Set(selectedControlled.value);
  if (next.has(rel)) next.delete(rel); else next.add(rel);
  selectedControlled.value = next;
}

async function doVerifyControlled(mode: "full" | "quick") {
  errorMsg.value = ""; statusMsg.value = "";
  if (!drive.value || !isBackupDisk.value) { errorMsg.value = "当前没有备份盘上下文"; return; }
  if (jobRunning.value) { errorMsg.value = "已有任务在进行中"; return; }
  try {
    jobRunning.value = true; jobProgress.value = null; controlledVerify.value = null;
    statusMsg.value = mode === "full" ? "完整校验进行中…" : "快速校验（FastMD5）进行中…";
    const relPaths = selectedControlled.value.size > 0 ? Array.from(selectedControlled.value) : null;
    const start = await invoke<JobStart>("start_verify_controlled", {
      drive: drive.value, mode, relPaths,
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
        <h2>受控文件校验</h2>
        <p class="muted small">未勾选则校验全部受控文件。完整 = 全文 MD5；快速 = FastMD5。</p>
        <div class="row">
          <button class="btn primary" title="按库中完整 MD5 校验受控文件" :disabled="jobRunning || !isBackupDisk" @click="doVerifyControlled('full')">完整校验</button>
          <button class="btn" title="按库中 FastMD5 快速校验受控文件" :disabled="jobRunning || !isBackupDisk" @click="doVerifyControlled('quick')">快速校验</button>
          <button v-if="jobRunning" class="btn" title="取消正在进行的校验任务" @click="doCancel">取消</button>
        </div>
        <ul class="result-list controlled">
          <li v-for="f in controlledFiles" :key="f.rel_path">
            <label class="ctrl-row">
              <input type="checkbox" :checked="selectedControlled.has(f.rel_path)" @change="toggleControlled(f.rel_path)" />
              <div><div class="rel">{{ f.rel_path }}</div>
                <div class="hash">MD5 {{ f.md5 }}</div>
                <div class="hash">Fast {{ f.fast_md5 }}</div></div>
            </label>
          </li>
          <li v-if="!controlledFiles.length" class="muted center">暂无受控文件</li>
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
          <button class="btn primary" title="对所选批次做完整校验" :disabled="jobRunning" @click="doVerifyBatch('full')">完整校验</button>
          <button class="btn" title="对所选批次做快速校验" :disabled="jobRunning" @click="doVerifyBatch('quick')">快速校验</button>
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
.layout { display:grid; grid-template-columns:1fr 1fr; gap:12px; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; max-height:calc(100vh - 140px); overflow:auto; }
h2 { margin:0 0 8px; font-size:0.95rem; }
.field { display:flex; flex-direction:column; gap:4px; margin-bottom:10px; font-size:0.8rem; color:#9aa7b8; }
select { background:#0f1419; border:1px solid #2a3442; color:#e7ecf3; border-radius:8px; padding:8px 10px; }
.row { display:flex; gap:8px; flex-wrap:wrap; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; flex:1; }
.btn:disabled { opacity:0.5; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.ghost { background:transparent; flex:0; width:auto; }
.muted { color:#9aa7b8; } .small { font-size:0.78rem; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.badge { font-size:0.72rem; background:#1f6b45; padding:2px 8px; border-radius:999px; margin-left:6px; }
.result-list { list-style:none; padding:0; margin:8px 0 0; max-height:280px; overflow:auto; }
.result-list.controlled { max-height:200px; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.ctrl-row { display:flex; gap:8px; align-items:flex-start; cursor:pointer; }
.rel { font-weight:600; } .hash { font-family:ui-monospace,Consolas,monospace; color:#9aa7b8; word-break:break-all; }
.verify-summary { margin-top:10px; }
@media (max-width:800px) { .layout { grid-template-columns:1fr; } }
</style>
