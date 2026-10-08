# 备份盘受控文件设计（已批准）

> 状态：**已批准**（用户确认）  
> 日期：2026-09-28  
> 范围：文档与后续 Rust/Vue 实现；**不含** MSI 打包  
> 相关：备份盘标记、`.datavault` 元数据、SQLite 受控文件清单、完整 MD5 / FastMD5 校验

## 1. 背景与目标

DataVault（数据管理）需要把特定外置/目标盘标记为「备份盘」，在盘根维护受控文件清单与校验指纹，以便：

1. 识别哪些盘是本应用管理的备份盘；
2. 登记需要长期管控的文件（受控文件）；
3. 用 **完整 MD5** 与 **FastMD5（分块抽样）** 对清单中的文件做完整性校验。

本轮只落地设计文档；实现阶段再改 Rust 后端与 Vue 前端。

## 2. 术语

| 术语 | 含义 |
|------|------|
| 备份盘 | 盘符根目录存在 `.datavault/`，且 `disk.json` 中 `is_backup_disk=true` 的卷 |
| 受控文件 | 已登记到该盘 `vault.db` 的 `controlled_files` 表中的文件 |
| 完整 MD5 | 对文件全部字节计算的标准 MD5（32 位小写十六进制） |
| FastMD5 | 按固定块大小切分，对每块前若干比例字节做抽样哈希后的汇总 MD5 |

## 3. 盘上布局

备份盘根目录：

```text
<drive_root>/
  .datavault/
    disk.json      # 盘级标记与应用标识
    vault.db       # SQLite：受控文件表
```

- 标记备份盘 = 创建 `.datavault/` 并写入合规的 `disk.json`（及空库或已迁移的 `vault.db`）。
- 检测备份盘 = 扫描盘符根，判断 `.datavault/disk.json` 是否存在且可读、且 `is_backup_disk === true`、`app === "DataVault"`。
- `.datavault` 目录本身不进入「受控文件」清单；路径一律相对盘根，使用统一规范化后的相对路径（实现时定一种并文档化）。

## 4. disk.json

### 4.1 Schema

```json
{
  "is_backup_disk": true,
  "app": "DataVault",
  "created_at": "2026-09-28T04:00:00+08:00",
  "updated_at": "2026-09-28T04:00:00+08:00"
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `is_backup_disk` | boolean | 必须为 `true` 才视为备份盘 |
| `app` | string | 固定 `"DataVault"`，避免与其它工具的同名目录冲突 |
| `created_at` | string | ISO-8601 带时区 |
| `updated_at` | string | ISO-8601 带时区；标记/刷新时更新 |

### 4.2 行为

- **标记为备份盘**：若不存在则创建目录与文件；若已存在且 `app` 不匹配则拒绝并提示；若已是本应用备份盘则刷新 `updated_at`。
- **取消标记**：本设计阶段不强制；若后续支持，须明确是否保留 `vault.db`。
- 编码：UTF-8；缩进可选。

## 5. vault.db（SQLite）

### 5.1 表：`controlled_files`

| 列 | 类型 | 约束 | 说明 |
|------|------|------|------|
| `rel_path` | TEXT | PRIMARY KEY | 相对盘根的路径，规范化后唯一 |
| `size` | INTEGER | NOT NULL | 字节大小 |
| `mtime` | INTEGER / TEXT | NOT NULL | 文件修改时间（实现选用 epoch 秒或 ISO；文档与代码一致即可） |
| `md5` | TEXT | NOT NULL | 完整文件 MD5，32 位小写 hex |
| `fast_md5` | TEXT | NOT NULL | FastMD5，32 位小写 hex |
| `sample_ratio` | REAL | NOT NULL DEFAULT 0.10 | 每块抽样比例（默认 10%） |
| `sample_chunk_mb` | INTEGER | NOT NULL DEFAULT 100 | 分块大小（默认 100 MB） |
| `updated_at` | TEXT | NOT NULL | 本行登记/重算时间，ISO-8601 |

建议索引：无额外必须索引（PK 已覆盖按路径查询）；若按 `updated_at` 列表可再加索引。

### 5.2 建表示意

```sql
CREATE TABLE IF NOT EXISTS controlled_files (
  rel_path TEXT PRIMARY KEY,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  md5 TEXT NOT NULL,
  fast_md5 TEXT NOT NULL,
  sample_ratio REAL NOT NULL DEFAULT 0.10,
  sample_chunk_mb INTEGER NOT NULL DEFAULT 100,
  updated_at TEXT NOT NULL
);
```

> `mtime` 类型以实现为准；上表以 INTEGER（Unix 秒）为例。

### 5.3 库文件位置

路径：`<drive_root>/.datavault/vault.db`  
打开方式：Rust 侧 `rusqlite`（或等价）只读写该盘数据库；不把受控清单迁到应用数据目录（清单随盘走）。

## 6. 哈希算法

### 6.1 完整 MD5

对文件全部字节流计算标准 MD5，输出 32 位小写十六进制。用于「完整校验」。

### 6.2 FastMD5（分块抽样）

参数（写入元数据，默认值如下）：

- `sample_chunk_mb` = **100**（块大小推荐按 **100 × 1024²** 字节）
- `sample_ratio` = **0.10**（每块取前 10%）

算法：

1. 设块大小 `C = sample_chunk_mb × 1024 × 1024`，比例 `R = sample_ratio`。
2. 将文件按 `C` 切分为若干完整块，**最后不足一块的剩余部分也作为一块**（remainder chunk）。
3. 对每一块：只读取该块内前 `floor(block_len × R)` 字节（若为 0 则该块不贡献字节）。
4. 按块顺序将各块抽样字节馈入同一个 MD5 上下文（等价于拼接后再 MD5）。
5. 输出 32 位小写 hex，存入 `fast_md5`；同时把所用的 `sample_ratio`、`sample_chunk_mb` 存入该行。

伪代码：

```text
ctx = MD5()
offset = 0
while offset < size:
  block_len = min(C, size - offset)
  sample_len = floor(block_len * R)
  if sample_len > 0:
    ctx.update(read(offset, sample_len))
  offset += block_len
return hex(ctx.digest())
```

空文件：`md5` 与 `fast_md5` 均为空输入的 MD5（`d41d8cd98f00b204e9800998ecf8427e`）。

## 7. 功能与 UI

### 7.1 检测备份盘

- 列出盘符时（或进入盘根时）探测 `.datavault/disk.json`。
- UI 上对备份盘给予明确标识（徽章/标签：「备份盘」）。

### 7.2 标记为备份盘

- 用户在某盘根选择「标记为备份盘」。
- 创建 `.datavault/`、`disk.json`、`vault.db`（若不存在）。
- 失败时给出可读错误（无写权限、只读介质、`app` 冲突等）。

### 7.3 添加受控文件

- 在已标记的备份盘上，用户选择文件（可多选；目录策略：实现阶段可先支持「仅文件」或「展开目录下文件」，须在实现任务中写清）。
- 对每个文件：计算 `size`/`mtime`/`md5`/`fast_md5`，upsert 到 `controlled_files`。
- 路径存 `rel_path`（相对该盘根）。
- 进度与取消：大文件哈希应可显示进度（实现阶段）。

### 7.4 校验

| 模式 | 行为 |
|------|------|
| 完整校验 | 对清单中每个文件重算完整 MD5，与库中 `md5` 比较；可同时核对 `size`/`mtime` 是否变化并提示 |
| 快速校验 | 用库中该行的 `sample_ratio`/`sample_chunk_mb` 重算 FastMD5，与 `fast_md5` 比较 |

结果展示：通过 / 失败 / 缺失（文件不在盘上）/ 错误；支持对单个或全部受控文件执行。

## 8. 命令面（实现指引，非本轮代码）

建议 Tauri 命令（名称可微调）：

| 命令 | 作用 |
|------|------|
| `detect_backup_disks` / 扩展 `list_drives` | 返回是否备份盘 |
| `mark_backup_disk` | 写入 `disk.json` + 初始化 `vault.db` |
| `add_controlled_files` | 计算哈希并 upsert |
| `list_controlled_files` | 列出清单 |
| `verify_controlled_full` | 完整 MD5 校验 |
| `verify_controlled_quick` | FastMD5 校验 |

前端：在现有浏览/备份/校验 UI 上增加备份盘标识、标记入口、受控文件管理与校验入口。

## 9. 非目标（本轮与近期）

- **不做** MSI / 安装包打包工作。
- 本轮 **不改业务代码**，仅文档。
- 不强制云同步、多盘清单合并、加密 vault.db。
- 不在本设计中规定旧 `metadata/batches/*.json` 的迁移脚本。

## 10. 实现分期（文档对齐用）

1. **文档（本提交）**：本规格 + 需求/架构/概览/计划同步。
2. **后端**：SQLite、`disk.json`、完整 MD5、FastMD5、命令。
3. **前端**：检测/标记/添加受控/完整与快速校验 UI。
4. **（明确延后）** 安装包 / MSI。

## 11. 风险与约定

- 外置盘拔出时所有对该盘的操作应失败并提示，避免写到错误卷。
- `rel_path` 规范化（大小写、分隔符、禁止 `..`）须在实现时固定，防止重复登记。
- FastMD5 为抽样算法，不能替代完整 MD5 的密码学强度结论；UI 文案应区分「快速」与「完整」。
- 默认 `sample_ratio=0.10`、`sample_chunk_mb=100` 写入每一行，以便日后改默认值时旧文件仍按登记参数校验。

## 12. 批准记录

- 用户批准要点：盘根 `.datavault/`；`disk.json` 字段；`vault.db` / `controlled_files`；FastMD5 按 100MB 块取前 10%（含余块）；完整 MD5；UI 检测/标记/添加/双模式校验；本轮仅文档，后续 Rust/Vue，不做 MSI。

## 增量行为（2026-09-28 晚 · UI / 任务面板）

在既有批准设计与先前增量之上补充：

1. **产品文案「受控」**：界面优先使用「受控盘」「标记为受控」「受控」徽章；磁盘元数据字段仍为 `is_backup_disk` / `mark_backup_disk`，与本节 schema 兼容。
2. **列表列**：浏览与高级窗统一为 **名称 / 大小 / 受控 / 数量**；目录「数量」= 受控文件数/总文件数（异步计数）；去掉「类型」「标记」列。
3. **建立备份索引**：单一入口，合并原「添加受控文件」；行为仍为 upsert `controlled_files` 且跳过已有 `rel_path`。
4. **多任务状态面板**：主窗下方「状态」展示并行 Job；中文任务标签；进度含当前相对路径；取消在任务展开区。
5. **校验四态对齐**：受控与**批次**校验均使用 `pass` / `fail` / `missing` / `error`，汇总 `passed` / `failed` / `missing` / `errors`；UI 可点击筛选。
6. **高级窗口**：高级校验（目录+批次）、高级备份（双栏）；不改变盘上 `disk.json` / `vault.db` 布局。
7. **FastMD5**：仍以本节 100MB×10% 为准；README 中旧「头/尾 64KB」描述已废弃，仅作历史对照。
8. **高级备份与归档**：源文件名正则（匹配/排除；支持 JS `/pattern/flags`）；可选打包 `{批次ID}.tar` 并在受控盘自动 upsert 归档行；浏览可展开已受控 `.tar`。不改变 `disk.json` / `controlled_files` 列结构。
9. **UI 细节**：高级备份左右等宽；列表名称换行。
