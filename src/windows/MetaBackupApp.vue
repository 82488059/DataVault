<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";

interface DriveInfo {
  path: string;
  is_backup_disk: boolean;
}
interface MetaExportResult {
  zip_path: string;
  entry_count: number;
  source_drive: string;
}
interface MetaRestoreResult {
  drive_root: string;
  backup_path: string | null;
  entry_count: number;
  message: string;
}
interface MetaVerifyReport {
  ok: boolean;
  message: string;
  entry_count: number;
  source_drive: string | null;
  exported_at: string | null;
  failures: string[];
}

const drives = ref<DriveInfo[]>([]);
const selectedDrive = ref("");
const busy = ref(false);
const errorMsg = ref("");
const statusMsg = ref("");
const verifySummary = ref("");
const lastZipPath = ref("");

const hasDrive = computed(() => !!selectedDrive.value);
const selectedInfo = computed(() =>
  drives.value.find((d) => normDrive(d.path) === selectedDrive.value) || null
);
const selectedIsControlled = computed(() => !!selectedInfo.value?.is_backup_disk);

function normDrive(p: string): string {
  const m = (p || "").match(/^([A-Za-z]:)/);
  return m ? m[1].toUpperCase() + "\\" : "";
}

function driveLabel(d: DriveInfo): string {
  const root = normDrive(d.path) || d.path;
  return d.is_backup_disk ? `${root}（受控）` : root;
}

async function refreshDrives() {
  errorMsg.value = "";
  try {
    const list = await invoke<DriveInfo[]>("list_drives");
    drives.value = list;
    if (selectedDrive.value) {
      const still = list.some((d) => normDrive(d.path) === selectedDrive.value);
      if (!still) selectedDrive.value = "";
    }
  } catch (e) {
    errorMsg.value = String(e);
  }
}

async function defaultExportName(): Promise<string> {
  const drive = selectedDrive.value;
  if (!drive) return "DataVault-meta.zip";
  try {
    return await invoke<string>("default_datavault_export_name", { drive });
  } catch {
    const letter = drive.replace(/:\\?$/, "").replace(/\\/g, "");
    const stamp = new Date().toISOString().replace(/[-:TZ.]/g, "").slice(0, 14);
    return `DataVault-meta-${letter}-${stamp}.zip`;
  }
}

async function doExport() {
  errorMsg.value = "";
  statusMsg.value = "";
  verifySummary.value = "";
  const drive = selectedDrive.value;
  if (!drive) {
    errorMsg.value = "请先选择盘符";
    return;
  }
  if (!selectedIsControlled.value) {
    errorMsg.value = "导出需要已标记的受控盘；请先在主窗口将盘符根「标记为受控」";
    return;
  }
  const defaultName = await defaultExportName();
  const zipPath = await save({
    title: "导出元数据（另存为）",
    defaultPath: defaultName,
    filters: [{ name: "Zip 归档", extensions: ["zip"] }],
  });
  if (!zipPath) {
    statusMsg.value = "已取消导出";
    return;
  }
  busy.value = true;
  statusMsg.value = "正在导出元数据…";
  try {
    const res = await invoke<MetaExportResult>("export_datavault_metadata", {
      drive,
      zipPath,
    });
    lastZipPath.value = res.zip_path;
    statusMsg.value = `已导出：${res.zip_path}（${res.entry_count} 个文件）`;
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

async function pickZip(title: string): Promise<string | null> {
  const picked = await open({
    title,
    multiple: false,
    filters: [{ name: "Zip 归档", extensions: ["zip"] }],
  });
  if (!picked) return null;
  return typeof picked === "string" ? picked : picked[0] ?? null;
}

async function doVerify() {
  errorMsg.value = "";
  statusMsg.value = "";
  verifySummary.value = "";
  if (!selectedDrive.value) {
    errorMsg.value = "请先选择盘符";
    return;
  }
  const zipPath = await pickZip("选择要校验的元数据备份 zip");
  if (!zipPath) {
    statusMsg.value = "已取消校验";
    return;
  }
  lastZipPath.value = zipPath;
  busy.value = true;
  statusMsg.value = "正在校验元数据备份…";
  try {
    const res = await invoke<MetaVerifyReport>("verify_datavault_backup_zip", {
      zipPath,
    });
    verifySummary.value = res.ok
      ? res.message
      : res.message +
        (res.failures?.length ? "\n" + res.failures.slice(0, 8).join("\n") : "");
    statusMsg.value = res.message;
    if (!res.ok) errorMsg.value = res.message;
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

async function doRestore() {
  errorMsg.value = "";
  statusMsg.value = "";
  verifySummary.value = "";
  const drive = selectedDrive.value;
  if (!drive) {
    errorMsg.value = "请先选择盘符";
    return;
  }
  const zipPath = await pickZip("选择要恢复到所选盘符的元数据备份 zip");
  if (!zipPath) {
    statusMsg.value = "已取消恢复";
    return;
  }
  lastZipPath.value = zipPath;
  const ok = window.confirm(
    `确认从 zip 恢复元数据到「${drive}」？\n\n将覆盖该盘根 .datavault/；若已存在会先自动备份为 .datavault.bak-<时间戳>/。\n\n导出的是清单与标记，不是盘上用户数据文件的拷贝。`
  );
  if (!ok) {
    statusMsg.value = "已取消恢复";
    return;
  }
  busy.value = true;
  statusMsg.value = "正在校验并恢复元数据…";
  try {
    const res = await invoke<MetaRestoreResult>("restore_datavault_metadata", {
      drive,
      zipPath,
    });
    statusMsg.value = res.message;
    await refreshDrives();
  } catch (e) {
    errorMsg.value = String(e);
    statusMsg.value = "";
  } finally {
    busy.value = false;
  }
}

onMounted(() => {
  void refreshDrives();
});
</script>

<template>
  <div class="app">
    <header class="header">
      <div>
        <h1>元数据备份 <span class="sub">DataVault</span></h1>
        <p class="hint">先选择盘符，再导出该盘 .datavault，或将 zip 恢复到该盘；校验用「打开文件」选 zip</p>
      </div>
      <button class="btn small" type="button" title="刷新盘符列表" :disabled="busy" @click="refreshDrives">刷新盘符</button>
    </header>

    <div v-if="errorMsg" class="banner error">{{ errorMsg }}</div>
    <div v-else-if="statusMsg" class="banner ok">{{ statusMsg }}</div>

    <section class="panel">
      <h2>1. 选择盘符</h2>
      <p class="muted small">导出/恢复/校验按钮在未选盘符前禁用。导出需受控盘；恢复可写入任意所选盘根。</p>
      <div v-if="!drives.length" class="muted center empty">无可用盘符</div>
      <ul class="drive-list">
        <li v-for="d in drives" :key="d.path">
          <label class="drive-item" :class="{ active: selectedDrive === normDrive(d.path) }">
            <input
              type="radio"
              name="meta-drive"
              :value="normDrive(d.path)"
              v-model="selectedDrive"
              :disabled="busy"
            />
            <span class="drive-name">{{ driveLabel(d) }}</span>
            <span v-if="d.is_backup_disk" class="badge backup">受控</span>
          </label>
        </li>
      </ul>
      <p v-if="selectedDrive" class="muted small">
        当前：<strong>{{ selectedDrive }}</strong>
        <span v-if="!selectedIsControlled">（非受控 — 可恢复导入；导出需先标记为受控）</span>
      </p>
    </section>

    <section class="panel">
      <h2>2. 导出 / 校验 / 恢复</h2>
      <p class="muted small" title="导出的是 .datavault 清单与标记，不是盘上用户数据文件的拷贝">
        导出：系统「另存为」预填默认文件名。校验/恢复：系统「打开文件」选择 zip。恢复目标为上方所选盘符根。
      </p>
      <div class="row">
        <button
          class="btn primary"
          title="将所选受控盘 .datavault 另存为 zip"
          :disabled="!hasDrive || busy"
          @click="doExport"
        >导出元数据…</button>
        <button
          class="btn"
          title="打开 zip，仅校验 manifest（不写盘）"
          :disabled="!hasDrive || busy"
          @click="doVerify"
        >校验元数据备份…</button>
        <button
          class="btn primary-outline"
          title="打开 zip，校验后恢复到所选盘符根；已有 .datavault 会先备份"
          :disabled="!hasDrive || busy"
          @click="doRestore"
        >恢复元数据…</button>
      </div>
      <p v-if="lastZipPath" class="muted small path">最近 zip：{{ lastZipPath }}</p>
      <p v-if="verifySummary" class="muted small verify" style="white-space:pre-wrap">{{ verifySummary }}</p>
    </section>
  </div>
</template>

<style scoped>
.app {
  min-height: 100vh;
  background: #0f1419;
  color: #e7ecf3;
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
  padding: 14px 16px 18px;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.header { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; }
h1 { margin: 0; font-size: 1.25rem; font-weight: 700; }
h2 { margin: 0 0 8px; font-size: 0.95rem; color: #9db4ff; }
.sub { color: #7aa2ff; font-weight: 500; font-size: 0.95rem; }
.hint { margin: 4px 0 0; color: #9aa7b8; font-size: 0.85rem; }
.banner { padding: 8px 12px; border-radius: 8px; font-size: 0.88rem; }
.banner.error { background: #3a1515; color: #ffb4b4; border: 1px solid #7a2e2e; }
.banner.ok { background: #14301f; color: #b6f0c8; border: 1px solid #2d6a45; }
.panel {
  background: #171d25;
  border: 1px solid #2a3442;
  border-radius: 12px;
  padding: 12px 14px;
}
.drive-list { list-style: none; margin: 8px 0 0; padding: 0; display: flex; flex-direction: column; gap: 6px; max-height: 220px; overflow: auto; }
.drive-item {
  display: flex; align-items: center; gap: 10px;
  padding: 8px 10px; border-radius: 8px; border: 1px solid #2a3442;
  background: #0f1419; cursor: pointer; user-select: none;
}
.drive-item.active { border-color: #2f5bff; background: #1e2a40; }
.drive-name { flex: 1; font-size: 0.9rem; }
.row { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
.btn {
  background: #243044; color: #e7ecf3; border: 1px solid #3a4a63;
  border-radius: 8px; padding: 8px 12px; cursor: pointer; font-size: 0.85rem;
}
.btn:disabled { opacity: 0.45; cursor: not-allowed; }
.btn.primary { background: #2f5bff; border-color: #2f5bff; font-weight: 600; }
.btn.primary-outline { background: transparent; border-color: #2f5bff; color: #9db4ff; }
.btn.small { padding: 4px 8px; font-size: 0.78rem; }
.muted { color: #9aa7b8; font-size: 0.8rem; }
.small { font-size: 0.78rem; }
.center { text-align: center; }
.empty { padding: 12px; }
.path, .verify { margin-top: 8px; word-break: break-all; }
.badge { font-size: 0.72rem; padding: 2px 8px; border-radius: 999px; }
.badge.backup { background: #1f6b45; color: #e7ecf3; }
</style>