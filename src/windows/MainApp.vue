<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { openBackupWindow, openVerifyWindow } from "../bridge";

interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  is_backup_disk: boolean;
}

const currentPath = ref("");
const entries = ref<DirEntryInfo[]>([]);
const selected = ref<Set<string>>(new Set());
const errorMsg = ref("");
const statusMsg = ref("");
const busy = ref(false);
const backupDriveSet = ref<Set<string>>(new Set());
const currentIsBackup = ref(false);

const pathLabel = computed(() => currentPath.value || "此电脑（盘符）");
const currentDrive = computed(() => {
  const m = currentPath.value.match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
});

function formatSize(n: number): string {
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
  } catch { /* ignore */ }
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
async function openEntry(e: DirEntryInfo) { if (e.is_dir) await loadDir(e.path); }

async function doMarkBackupDisk() {
  errorMsg.value = ""; statusMsg.value = "";
  let target = currentDrive.value;
  if (!currentPath.value) {
    if (selected.value.size !== 1) { errorMsg.value = "请在盘符列表中勾选一个盘，或先进入该盘"; return; }
    target = Array.from(selected.value)[0];
  }
  if (!target) { errorMsg.value = "请先进入要标记的盘符"; return; }
  busy.value = true; statusMsg.value = "正在标记备份盘…";
  try {
    await invoke("mark_backup_disk", { drive: target });
    statusMsg.value = `已标记备份盘：${target}`;
    await refreshBackupDrives(); currentIsBackup.value = true;
    if (!currentPath.value) await loadDir("");
  } catch (e) { errorMsg.value = String(e); statusMsg.value = ""; }
  finally { busy.value = false; }
}

async function openBackup() {
  errorMsg.value = "";
  try {
    await openBackupWindow({
      sources: Array.from(selected.value),
      currentPath: currentPath.value,
      destHint: currentIsBackup.value ? currentPath.value : "",
    });
  } catch (e) { errorMsg.value = "打开备份窗口失败: " + String(e); }
}

async function openVerify() {
  errorMsg.value = "";
  try {
    await openVerifyWindow({ drive: currentDrive.value, relPaths: [], isBackupDisk: currentIsBackup.value });
  } catch (e) { errorMsg.value = "打开校验窗口失败: " + String(e); }
}

onMounted(() => { void goRoot(); });
</script><template>
  <div class="app">
    <header class="header">
      <div>
        <h1>数据管理 <span class="sub">DataVault</span></h1>
        <p class="hint">文件浏览 <span v-if="currentIsBackup" class="badge backup">备份盘 {{ currentDrive }}</span></p>
      </div>
      <div class="header-actions">
        <button class="btn ghost" title="返回盘符列表" :disabled="busy" @click="goRoot">盘符</button>
        <button class="btn ghost" title="返回上一级目录" :disabled="busy || !currentPath" @click="goUp">上级</button>
        <button class="btn ghost" title="刷新当前目录" :disabled="busy" @click="loadDir(currentPath)">刷新</button>
        <button class="btn primary-outline" title="打开独立备份窗口：批次备份、添加受控文件、建立备份索引" :disabled="busy" @click="openBackup">备份</button>
        <button class="btn primary-outline" title="打开独立校验窗口：完整/快速校验与结果" :disabled="busy" @click="openVerify">校验</button>
      </div>
    </header>
    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-if="statusMsg" class="banner ok">{{ statusMsg }}</div>
    <section class="panel explorer">
      <div class="pathbar"><span class="label">当前位置</span><code>{{ pathLabel }}</code></div>
      <div class="toolbar">
        <button class="btn small" title="全选当前列表中的所有项" @click="selectAllFiles">全选</button>
        <button class="btn small" title="清空当前勾选" @click="selected = new Set()">清空选择</button>
        <button class="btn small primary-outline" title="将当前盘标记为 DataVault 备份盘（写入 .datavault）" :disabled="busy" @click="doMarkBackupDisk">标记为备份盘</button>
        <span class="muted">已选 {{ selected.size }} 项</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th style="width:36px"></th><th>名称</th><th style="width:80px">类型</th><th style="width:100px">大小</th><th style="width:90px">标记</th></tr></thead>
          <tbody>
            <tr v-for="e in entries" :key="e.path" :class="{ selected: selected.has(e.path) }" @dblclick="openEntry(e)">
              <td><input type="checkbox" :checked="selected.has(e.path)" @change="toggleSelect(e.path)" /></td>
              <td class="name" @click="e.is_dir ? openEntry(e) : toggleSelect(e.path)"><span class="icon">{{ e.is_dir ? "📁" : "📄" }}</span>{{ e.name }}</td>
              <td>{{ e.is_dir ? "文件夹" : "文件" }}</td>
              <td>{{ e.is_dir ? "—" : formatSize(e.size) }}</td>
              <td><span v-if="e.is_backup_disk" class="badge backup">备份盘</span></td>
            </tr>
            <tr v-if="!entries.length"><td colspan="5" class="muted center">空目录或无法访问</td></tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>﻿<style scoped>
.app { min-height:100vh; background:#0f1419; color:#e7ecf3; font-family:"Segoe UI","Microsoft YaHei",system-ui,sans-serif; padding:16px 20px 24px; box-sizing:border-box; display:flex; flex-direction:column; gap:10px; }
.header { display:flex; justify-content:space-between; align-items:flex-start; gap:12px; }
h1 { margin:0; font-size:1.4rem; font-weight:700; }
.sub { color:#7aa2ff; font-weight:500; font-size:0.95rem; }
.hint { margin:4px 0 0; color:#9aa7b8; font-size:0.85rem; }
.header-actions { display:flex; gap:8px; flex-wrap:wrap; }
.banner { padding:8px 12px; border-radius:8px; font-size:0.9rem; }
.banner.error { background:#3a1515; color:#ffb4b4; border:1px solid #7a2e2e; }
.banner.ok { background:#14301f; color:#b6f0c8; border:1px solid #2d6a45; }
.panel { background:#171d25; border:1px solid #2a3442; border-radius:12px; padding:12px; flex:1; display:flex; flex-direction:column; min-height:0; }
.pathbar { display:flex; align-items:center; gap:10px; margin-bottom:8px; }
.pathbar .label { color:#9aa7b8; font-size:0.8rem; }
.pathbar code { flex:1; background:#0f1419; padding:6px 10px; border-radius:6px; border:1px solid #2a3442; font-size:0.85rem; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.toolbar { display:flex; gap:8px; align-items:center; margin-bottom:8px; flex-wrap:wrap; }
.table-wrap { flex:1; overflow:auto; border:1px solid #2a3442; border-radius:8px; }
table { width:100%; border-collapse:collapse; font-size:0.9rem; }
th, td { padding:8px 10px; text-align:left; border-bottom:1px solid #243041; }
th { background:#1c2430; color:#9aa7b8; font-weight:600; position:sticky; top:0; }
tr.selected { background:#1e2a40; } tr:hover { background:#1a222e; }
.name { cursor:pointer; user-select:none; } .icon { margin-right:6px; }
.btn { background:#243044; color:#e7ecf3; border:1px solid #3a4a63; border-radius:8px; padding:8px 12px; cursor:pointer; font-size:0.88rem; }
.btn:disabled { opacity:0.5; cursor:not-allowed; }
.btn.primary-outline { border-color:#2f5bff; color:#9db4ff; }
.btn.ghost { background:transparent; } .btn.small { padding:4px 8px; font-size:0.8rem; }
.muted { color:#9aa7b8; } .center { text-align:center; }
.badge { font-size:0.75rem; background:#2f5bff; padding:2px 8px; border-radius:999px; margin-left:6px; }
.badge.backup { background:#1f6b45; }
</style>
