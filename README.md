# DataVault（数据管理）

基于 **Tauri 2 + Vue 3 + TypeScript** 的桌面端数据备份与校验工具。

远程仓库：<https://github.com/82488059/DataVault>

## 适用场景

- 拷贝到移动盘 / 备份盘后，用完整或快速校验确认有没有拷坏
- 将盘标记为受控并建立索引，弄清「哪些文件归我管」
- **移动硬盘长时间不通电存放后再接上使用**：介质可能已发生静默损坏（文件还在、大小也正常，但内容已坏），单靠打开文件很难发现；对照事先登记的校验值做完整性核对，按通过 / 失败 / 缺失 / 错误定位问题数据（归档盘、冷备盘重新启用时尤其有用）

## 功能概览

- 资源管理器式浏览本机盘符 / 文件夹 / 文件（列：**名称 / 大小 / 受控 / 数量**）；长文件名自动换行
- **标记为受控**（盘符根 `.datavault`）；**建立备份索引**（单一按钮，合并原「添加受控」）登记受控文件
- 批次备份到目标目录，并写入批次元数据（可选批次名称）
- **完整校验**：整文件 MD5；**快速校验**：FastMD5（默认每 100MB 块取前 10%）
- 主窗**多任务状态面板**；校验结果四态：通过 / 失败 / 缺失 / 错误
- 独立窗：**高级校验**、**高级备份**
  - 高级备份：左右等宽双栏；源文件名正则（匹配/排除；支持裸 Rust 正则或 JS 风格 `/pattern/flags`）；可选**打包为 tar**（受控盘自动登记归档）；可展开受控 `.tar`

更多说明见 [`docs/概览.md`](docs/概览.md)。

## 环境要求

- Node.js + npm
- Rust（`rustc` / `cargo`），建议将 `%USERPROFILE%\.cargo\bin` 加入 PATH
- Windows：WebView2、**Visual Studio 2022** MSVC（`vcvars64.bat`）；避免 PATH 上 VS2017 `link.exe` 抢先

## 安装与运行

```powershell
cd E:\Repos\DataVault
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH
npm install
npm run tauri dev
```

生产构建（强制 VS2022 工具链）：

```powershell
cmd /c `"D:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat`" && set PATH=%USERPROFILE%\.cargo\bin;%PATH% && npm run tauri build
```

### 打包回退顺序

1. 优先 `npm run tauri build`（`bundle.targets=all`：可用时产出 MSI / NSIS 等）
2. WiX/MSI 不可用时：`npm run tauri build -- --bundles nsis`
3. 再回退便携包：`npm run tauri build -- --bundles zip`
4. 仍失败则：`npm run tauri build -- --no-bundle`，交付 `src-tauri\target\release\datavault.exe`

成功打包时产物通常在 `src-tauri\target\release\bundle\`（`msi` / `nsis` / `zip`）。

## MD5 方案说明

### 完整 MD5

对文件全部字节计算标准 MD5，输出 32 位小写十六进制。用于「完整校验」。

### FastMD5（快速校验）

默认参数：`sample_chunk_mb=100`，`sample_ratio=0.10`。按块取前 10%（含余块）馈入同一 MD5。详见 [受控文件设计](docs/superpowers/specs/2026-09-28-backup-disk-controlled-files-design.md)。

> 脚手架早期「头/尾 64KiB + 文件大小 ASCII」方案**已废弃**，不再用于受控文件校验。

## 元数据

- 盘根：`.datavault/disk.json`、`.datavault/vault.db`
- 应用数据目录：`metadata/batches/<批次ID>.json`
- 备份目标旁路：`.datavault/<批次ID>.json`

## 文档

- [概览](docs/概览.md)
- [需求说明](docs/需求说明.md)
- [架构设计](docs/架构设计.md)
- [开发计划](docs/开发计划.md)
