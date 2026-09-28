import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { emit } from "@tauri-apps/api/event";

export const EVT_BACKUP_CTX = "dv-backup-ctx";
export const EVT_VERIFY_CTX = "dv-verify-ctx";
export const LS_BACKUP = "dv-backup-ctx";
export const LS_VERIFY = "dv-verify-ctx";

export interface BackupContext {
  sources: string[];
  currentPath: string;
  destHint: string;
}

export interface VerifyContext {
  drive: string;
  /** Pre-selected controlled rel_paths (from vault.db). */
  relPaths: string[];
  /** Absolute file/dir paths to resolve against vault.db (uncontrolled skipped). */
  paths: string[];
  isBackupDisk: boolean;
}

export async function openOrFocusWindow(
  label: string,
  title: string,
  width: number,
  height: number,
): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    try {
      await existing.unminimize();
    } catch {
      /* ignore */
    }
    try {
      await existing.setFocus();
    } catch {
      /* ignore */
    }
    return existing;
  }
  const win = new WebviewWindow(label, {
    url: "/",
    title,
    width,
    height,
    minWidth: Math.min(560, width),
    minHeight: Math.min(420, height),
    center: true,
    resizable: true,
    focus: true,
  });
  await new Promise<void>((resolve, reject) => {
    win.once("tauri://created", () => resolve());
    win.once("tauri://error", (e) => reject(e));
  });
  return win;
}

export async function openBackupWindow(ctx: BackupContext): Promise<void> {
  localStorage.setItem(LS_BACKUP, JSON.stringify(ctx));
  await openOrFocusWindow("backup", "DataVault · 备份", 820, 640);
  await emit(EVT_BACKUP_CTX, ctx);
}

export async function openVerifyWindow(ctx: VerifyContext): Promise<void> {
  localStorage.setItem(LS_VERIFY, JSON.stringify(ctx));
  await openOrFocusWindow("verify", "DataVault · 校验", 1100, 720);
  await emit(EVT_VERIFY_CTX, ctx);
}

export function readBackupCtx(): BackupContext | null {
  try {
    const raw = localStorage.getItem(LS_BACKUP);
    if (!raw) return null;
    return JSON.parse(raw) as BackupContext;
  } catch {
    return null;
  }
}

export function readVerifyCtx(): VerifyContext | null {
  try {
    const raw = localStorage.getItem(LS_VERIFY);
    if (!raw) return null;
    return JSON.parse(raw) as VerifyContext;
  } catch {
    return null;
  }
}
