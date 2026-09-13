# adbx

adb 扩展 CLI（Rust）。在 adb 之上包装常用操作的增强能力——核心是**包名模糊匹配**：不用先 `pm list packages` 查完整包名再复制粘贴，直接按关键词定位应用并执行动作。

```
$ adbx stop wanandroid
✓ 已停止 com.location.wanandroid
```

## 特性

- **模糊包名匹配**：关键词子串匹配、大小写不敏感、多关键词 AND 交集、完整包名精确匹配优先
- **安全兜底**：多命中时列出全部候选而不盲目执行，零命中明确报错；破坏性操作（卸载）默认二次确认
- **TUI 文件浏览**：键盘/鼠标浏览设备文件系统，跨目录多选、批量 `adb pull`，带实时筛选、排序、进度、纯文本编辑和右侧图片/文本预览
- **多设备支持**：全局 `-s/--serial` 转发为 `adb -s`，适用于多设备场景
- **单二进制、体积优先**：release 构建启用 fat LTO + `opt-level = "z"` + strip，无运行时依赖（仅要求系统装有 adb）

## 安装

### 从源码构建

需要 Rust 1.88+（edition 2024）与 [adb](https://developer.android.com/tools/adb)（须在 PATH 中）。

```bash
git clone <repo-url>
cd adbx
cargo build --release
# 产物在 target/release/adbx，按需复制到 PATH 目录
cp target/release/adbx /usr/local/bin/
```

构建后可用 `adbx doctor` 自检 adb 环境。

### 从 GitHub Release 下载

推送 `v*` 版本 Tag 后，GitHub Actions 会自动创建 [Release](https://github.com/location-txl/adbx/releases)，提供以下平台的压缩包：

| 平台 | 文件 |
|---|---|
| Windows x64 | `adbx-v<版本>-x86_64-pc-windows-msvc.zip` |
| Linux x64 | `adbx-v<版本>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Intel | `adbx-v<版本>-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `adbx-v<版本>-aarch64-apple-darwin.tar.gz` |

Unix 平台解压后，将 `adbx` 放入 PATH；Windows 平台解压后，将 `adbx.exe` 所在目录加入 PATH。运行 `adbx doctor` 可检查 adb 环境。

#### 一键安装

macOS/Linux：

```bash
curl -fsSL https://raw.githubusercontent.com/location-txl/adbx/main/install.sh | sh
```

Windows PowerShell：

```powershell
irm https://raw.githubusercontent.com/location-txl/adbx/main/install.ps1 | iex
```

安装脚本会自动识别当前平台和架构，默认安装到 `~/.local/bin`（macOS/Linux）或 `%LOCALAPPDATA%\adbx\bin`（Windows）。需要固定版本时，Shell 使用 `--version v0.1.0`，PowerShell 使用 `-Version v0.1.0`；也可以分别通过 `--install-dir` 和 `-InstallDir` 指定安装目录。

## 使用

```
adbx [-s <serial>] <命令> [参数]
```

### 命令一览

| 命令 | 作用 | 底层 adb 调用 |
|---|---|---|
| `stop <关键词>...` | 模糊匹配包名并强制停止应用 | `am force-stop` |
| `restart <关键词>...` | 模糊匹配包名并重启应用（停止后重新拉起） | `am force-stop` + `monkey` |
| `clear <关键词>...` | 模糊匹配包名并清除应用数据（**不可恢复**） | `pm clear` |
| `uninstall [-y] <关键词>...` | 模糊匹配包名并卸载应用，默认二次确认（**不可恢复**） | `pm uninstall` |
| `info <关键词>...` | 模糊匹配包名，展示完整包名与版本信息（versionName / versionCode） | `dumpsys package` |
| `browse [path] [-o <dir>]` | TUI 浏览设备文件系统，多选批量拉取、预览和编辑文本文件 | `ls` / `pull` / `exec-out` / `shell -T` |
| `doctor` | 检查 adb 是否已安装并显示版本 | `adb version` |

所有包名命令共享同一套匹配规则，多个关键词之间是 **AND 交集**关系：包名须同时包含所有关键词。

### 匹配规则

1. 取设备全量包名（`pm list packages`）。
2. **精确匹配优先**：只给一个关键词且恰好等于某个完整包名（大小写不敏感）时直接采用，不再做子串匹配——输入 `com.v1.chat` 不会误伤 `com.v1.chat.plugin`。
3. 否则做子串交集过滤：包名（小写化后）须包含每一个关键词。
4. 按命中数决定行为：

| 命中数 | 行为 | 退出码 |
|---|---|---|
| 唯一 / 精确 | 执行对应动作 | 0 |
| 多个 | 不执行，列出全部候选并提示换更精确的关键词（`info` 为只读命令，多命中时全部展示） | 1 |
| 零 | 报错 `没有包同时包含 ...` | 1 |

### 示例

```bash
# 唯一命中，直接停止
adbx stop wanandroid
# ✓ 已停止 com.location.wanandroid

# 多关键词缩小范围（AND 交集）
adbx stop location wanandroid
# ✓ 已停止 com.location.wanandroid

# 多命中时不执行，列出候选
adbx stop setting
# ✗ 多个包匹配 "setting"：
#     com.android.providers.settings
#     com.android.settings
#   请使用更精确的关键词重试

# 查看包版本信息
adbx info userdictionary
# ✓ com.android.providers.userdictionary
#     versionName: 7.1.2
#     versionCode: 25

# 跳过卸载二次确认（脚本场景）
adbx uninstall -y wanandroid

# 多设备时指定 serial
adbx -s 3GKL2NILLN stop wanandroid
```

### browse：TUI 文件浏览

```bash
adbx browse              # 从 /sdcard 开始，拉取到当前工作目录
adbx browse /sdcard/DCIM -o ~/pull_out
```

| 按键 | 行为 |
|---|---|
| ↑ / ↓ | 移动光标（列表自动滚动） |
| → / ←、⌫ | 进入文件夹 / 返回上一级（恢复光标位置） |
| Space | 标记 / 取消标记（跨目录累计，标记文件夹递归拉取） |
| / | 边输入边筛选当前目录条目 |
| S / O | 切换排序键（名称 → 大小 → 日期）/ 翻转方向 |
| v | 在右侧半屏预览选中的纯文本或 PNG/JPEG/GIF/BMP/WebP 图片；再次按 `v` 关闭 |
| e | 编辑选中的 UTF-8 纯文本文件；图片、二进制和特殊文件不可编辑 |
| Ctrl-S / Esc | 编辑器中保存 / 退出；有未保存修改时会先确认保存、丢弃或继续编辑 |
| 鼠标 | 列表单击选中、滚轮移动；编辑器单击定位光标、滚轮滚动 |
| PageUp / PageDown / Home / End | 滚动或定位右侧文本预览 |
| Enter | 批量拉取全部已标记条目，带逐项进度 |
| q / Esc | 退出，打印拉取摘要；拉取中按下则取消并中止 |

预览和编辑统一最多读取 10 MiB。编辑保存前会重新读取远端文件并与打开时快照比较；发现远端已修改时不会直接覆盖，需按 `o` 明确确认。拉取采用逐项阻塞 + 轮询本地落盘字节的方式回报近似进度；单项失败不中断整体，拉取中可随时取消，未完成项保留标记可重试。

## 开发

```bash
cargo build                               # 编译
cargo clippy --all-targets -- -D warnings # lint，必须零警告
cargo test                                # 全部测试（单测 + doctest）
```

### 架构

调用链分三层，单向依赖：

```
main.rs（clap 解析 + 子命令分发）
  → commands/<name>.rs（命令逻辑，一个命令一个模块）
    → adb.rs（唯一的 adb 调用出口）
```

- **`adb.rs` 是唯一的 adb 调用点**：`run_adb` 统一处理 `-s <serial>` 转发、退出码检查和 stderr 透传；命令模块不直接 `Command::new("adb")`。
- **可测性**：与设备无关的逻辑是纯函数（如 `matching.rs` 的 `match_packages`），可直接单测；设备 I/O 只存在于 `adb.rs`。
- **错误处理**：anyhow。命令实现返回 `Result`，main 统一打印 `✗ {err}` 并以退出码 1 结束；成功路径打印 `✓ ...`。

### 新增子命令

1. `main.rs` 的 `Command` enum 加变体；
2. `commands/mod.rs` 注册模块；
3. 实现 `commands/<name>.rs` 的 `pub fn run(serial: Option<&str>, ...)`；
4. 在 `docs/` 下新增对应文档。

## 文档

各命令的详细用法、行为细节与实现说明见 [`docs/`](docs/)：

- [browse](docs/browse.md) · [clear](docs/clear.md) · [doctor](docs/doctor.md) · [info](docs/info.md) · [restart](docs/restart.md) · [stop](docs/stop.md) · [uninstall](docs/uninstall.md)
