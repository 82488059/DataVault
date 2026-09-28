import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

export async function openOrFocusWindow(
  label: string,
  title: string,
  width: number,
  height: number,
): Promise<WebviewWindow> {
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    try { await existing.unminimize(); } catch { /* ignore */ }
    try { await existing.setFocus(); } catch { /* ignore */ }
    return existing;
  }
  const win = new WebviewWindow(label, {
    url: "/",
    title,
    width,
    height,
    minWidth: Math.min(720, width),
    minHeight: Math.min(520, height),
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

export async function openAdvancedVerifyWindow(): Promise<void> {
  await openOrFocusWindow("batches", "数据管理 · 高级校验", 960, 640);
}

export async function openAdvancedBackupWindow(): Promise<void> {
  await openOrFocusWindow("adv-backup", "数据管理 · 高级备份", 1100, 720);
}