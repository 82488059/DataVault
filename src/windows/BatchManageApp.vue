<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
interface VerifyJobFinished {
  job_id: string; kind: string; ok: boolean; cancelled: boolean; message: string;
  controlled: unknown; batch: VerifyReport | null;
}

const rows = ref<BatchRow[]>([]);
const selected = ref<Set<string>>(new Set());
const busy = ref(false);
const errorMsg = ref("");
const statusMsg = ref("");
const progress = ref<JobProgress | null>(null);
const lastReport = ref<VerifyReport | null>(null);
const unlisteners: UnlistenFn[] = [];

function driveOf(dest: string): string {
  const m = (dest || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : (dest || "—");
}

/** Prefer YYYYMMDDHHmmss id → readable local time; else created_at. */
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

async function refresh() {
  errorMsg.value = "";
  try {
    const ids = await invoke<string[]>("list_batches");
    const next: BatchRow[] = [];
    for (const id of ids) {
      try {
        const b = await invoke<BackupBatch>("load_batch", { batchId: id });
        next.push({
          id: b.id,
          timeLabel: timeOf(b),
          fileCount: b.files?.length ?? 0,
          drive: driveOf(b.destination_root),
          raw: b,
        });
      } catch {
        /* skip broken */
      }
    }
    rows.value = next;
    const keep = new Set<string>();
    for (const id of selected.value) if (next.some((r) => r.id === id)) keep.add(id);
    selected.value = keep;
  } catch (e) {
    errorMsg.value = String(e);
  }
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
  } catch (e) {
    errorMsg.value = String(e); statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  await refresh();
  const bind = async (ev: string, fn: (p: any) => void) => {
    unlisteners.push(await listen(ev, (e) => fn(e.payload)));
  };
  await bind("verify-job-progress", (p: JobProgress) => {
    if (p.job_id?.startsWith("batch-") || true) progress.value = p;
  });
  await bind("verify-job-finished", (p: VerifyJobFinished) => {
    progress.value = null;
    if (p.batch) lastReport.value = p.batch;
    statusMsg.value = p.message || (p.ok ? "校验完成" : "校验结束");
    void refresh();
  });
});

onUnmounted(() => { for (const u of unlisteners) try { u(); } catch { /* */ } });
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>高级校验 <span class="sub">DataVault</span></h1>
        <p class="hint">勾选批次后执行快速/完整校验。批次号为本地年月日时分秒。</p>
      </div>
      <div class="row">
        <button class="btn small" title="刷新批次列表" @click="refresh">刷新</button>
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

    <section class="panel">
      <div class="toolbar">
        <button class="btn small" @click="toggleAll">{{ selected.size === rows.length && rows.length ? "清空选择" : "全选" }}</button>
        <button class="btn small primary" title="对勾选批次做快速校验" :disabled="!hasSelection || busy" @click="doVerify('quick')">快速校验</button>
        <button class="btn small" title="对勾选批次做完整校验" :disabled="!hasSelection || busy" @click="doVerify('full')">完整校验</button>
        <span class="muted">已选 {{ selected.size }} / {{ rows.length }}</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th style="width:36px"></th>
              <th>批次号</th>
              <th style="width:180px">时间</th>
              <th style="width:80px">文件数</th>
              <th style="width:90px">盘符</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id" :class="{ selected: selected.has(r.id) }" @click="toggle(r.id)">
              <td @click.stop>
                <input type="checkbox" :checked="selected.has(r.id)" @change="toggle(r.id)" />
              </td>
              <td class="mono">{{ r.id }}</td>
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

    <section v-if="lastReport" class="panel results">
      <h2>最近校验结果</h2>
      <p>批次 {{ lastReport.batch_id }} · {{ lastReport.mode === "full" ? "完整" : "快速" }}：通过
        <strong class="pass">{{ lastReport.passed }}</strong> / 失败
        <strong class="fail">{{ lastReport.failed }}</strong></p>
      <ul class="result-list">
        <li v-for="(it, i) in lastReport.items" :key="i" :class="{ ok: it.ok, bad: !it.ok }">
          <div class="rel">{{ it.rel_path }}</div>
          <div class="msg">{{ it.message }}</div>
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.app { min-height:100vh; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:14px 16px 18px; box-sizing:border-box; display:flex; flex-direction:column; gap:10px; }
.header { display:flex; justify-content:space-between; align-items:flex-start; gap:12px; }
h1 { margin:0; font-size:1.25rem; font-weight:700; }
h2 { margin:0 0 8px; font-size:1rem; color:#9db4ff; }
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
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:8px; flex-wrap:wrap; }
.table-wrap { overflow:auto; border:1px solid #2a3442; border-radius:8px; max-height:calc(100vh - 280px); }
table { width:100%; border-collapse:collapse; font-size:0.88rem; }
th, td { padding:7px 9px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; cursor:pointer; }
.mono { font-family:ui-monospace,Consolas,monospace; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.85rem; }
.btn:disabled { opacity:0.45; cursor:not-allowed; }
.btn.primary { background:#2f5bff; border-color:#2f5bff; font-weight:600; }
.btn.small { padding:4px 8px; font-size:0.78rem; }
.muted { color:#9aa7b8; } .center { text-align:center; }
.pass { color:#6dffa0; } .fail { color:#ff8f8f; }
.results { max-height:220px; overflow:auto; }
.result-list { list-style:none; margin:0; padding:0; }
.result-list li { padding:8px; border-radius:8px; margin-bottom:6px; border:1px solid #2a3442; font-size:0.78rem; }
.result-list li.ok { border-color:#2d6a45; background:#122018; }
.result-list li.bad { border-color:#7a2e2e; background:#201212; }
.rel { font-weight:600; }
.row { display:flex; gap:8px; }
</style>