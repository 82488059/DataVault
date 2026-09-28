# DataVault（数据管理）

基于 **Tauri 2 + Vue 3 + TypeScript** 的桌面端数据备份与校验工具。

远程仓库：<https://git.xssl.online/82488059/DataVault.git>

## 功能概览

- 资源管理器式浏览本机盘符 / 文件夹 / 文件
- 将选中项备份到指定目标目录，并写入批次元数据
- **完整校验**：整文件 MD5
- **快速校验**：抽样 MD5（头 64KB + 尾 64KB + 文件大小）

更多说明见 [`docs/概览.md`](docs/概览.md)。

## 环境要求

- Node.js + npm
- Rust（`rustc` / `cargo`），建议将 `%USERPROFILE%\.cargo\bin` 加入 PATH
- Windows：WebView2、MSVC 构建工具（Visual Studio Build Tools）

## 安装与运行

```powershell
cd F:\Gitee\DataVault
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH
npm install
npm run tauri dev
```

生产构建：

```powershell
npm run tauri build
```

## MD5 方案说明

### 完整 MD5

对文件全部字节计算标准 MD5，输出 32 位小写十六进制。用于「完整校验」。

### 抽样 / 快速 MD5

1. 若 `size ≤ 64KiB`：哈希整个文件  
2. 若 `64KiB < size ≤ 128KiB`：哈希「前 64KiB + 剩余尾部」  
3. 若 `size > 128KiB`：哈希「前 64KiB + 后 64KiB」  
4. 再将文件大小的**十进制 ASCII**追加进哈希输入  

用于「快速校验」，可较快发现头尾内容或长度变化。

## 元数据

- 应用数据目录下：`metadata/batches/batch-*.json`
- 备份目标下旁路：`.datavault/batch-*.json`

## 文档

- [概览](docs/概览.md)
- [需求说明](docs/需求说明.md)
- [架构设计](docs/架构设计.md)
- [开发计划](docs/开发计划.md)
