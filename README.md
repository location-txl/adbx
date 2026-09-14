# adbx

adbx 是 adb 的增强命令行工具。你可以用更短的关键词管理 Android 应用、浏览和拉取设备文件，也可以继续使用所有原生 adb 命令。

例如，不必先查完整包名：

~~~bash
adbx stop wanandroid
~~~

输出：

~~~text
✓ 已停止 com.location.wanandroid
~~~

## 适合做什么

- 用关键词停止、重启、清除数据、卸载应用。
- 查看应用的完整包名和版本信息。
- 在终端里浏览设备文件，批量拉取、预览或编辑文本。
- 多台设备同时连接时，用 serial 指定目标。
- 用 `adbx shell`、`adbx logcat`、`adbx install` 等方式继续调用 adb。

## 安装

使用前需要先准备 adb，并把它加入 PATH。已有 adb 时可以直接使用；没有 adb 时，先安装 adbx，再运行 `adbx adb-install` 自动下载并配置官方 Platform-Tools。Android 官方说明见 [SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools)。

~~~bash
adbx adb-install
~~~

下载过程中会显示已下载大小和百分比。命令会把 Platform-Tools 安装到用户目录并写入用户级 PATH；完成后重新打开终端，或按命令输出加载 shell 配置。支持 Bash、zsh 和 fish 的常见启动配置；Windows 会通知环境变更，但已有终端宿主或 IDE 需要完全退出后再打开。

### macOS 或 Linux

~~~bash
curl -fsSL https://raw.githubusercontent.com/location-txl/adbx/main/install.sh | sh
~~~

默认安装到 `~/.local/bin`。如果该目录不在 PATH 中，安装脚本会给出当前 shell 的设置方式。

### Windows PowerShell

~~~powershell
irm https://raw.githubusercontent.com/location-txl/adbx/main/install.ps1 | iex
~~~

默认安装到 `%LOCALAPPDATA%\adbx\bin`。

### 下载 Release

也可以从 [GitHub Releases](https://github.com/location-txl/adbx/releases) 下载压缩包。当前提供 Windows x64/ARM64、Linux x64、macOS Intel 和 macOS Apple Silicon 版本。解压后把可执行文件所在目录加入 PATH。

### 从源码构建

需要 Rust stable（项目使用 edition 2024）：

~~~bash
git clone https://github.com/location-txl/adbx.git
cd adbx
cargo build --release
~~~

生成的文件在 `target/release/adbx`；Windows 下文件名为 `adbx.exe`。

## 第一次使用

1. 在手机上打开 USB 调试，用 USB 连接电脑。
2. 确认 adb 能看到设备：

   ~~~bash
   adb devices
   ~~~

   第一次连接时请在手机上允许 USB 调试授权。设备状态应为 `device`。
3. 检查本机环境：

   ~~~bash
   adbx doctor
   ~~~

4. 试着查看一个应用：

   ~~~bash
   adbx info setting
   ~~~

完整步骤和故障处理见 [快速上手](docs/quickstart.md) 与 [常见问题](docs/troubleshooting.md)。

## 命令速览

命令格式是：

~~~text
adbx [-s <serial>] <命令> [参数]
~~~

| 命令 | 用途 |
| --- | --- |
| `stop <关键词>...` | 强制停止应用 |
| `restart <关键词>...` | 停止后重新启动应用 |
| `clear <关键词>...` | 清除应用数据，不可恢复 |
| `uninstall [-y] <关键词>...` | 卸载应用，默认会确认 |
| `info <关键词>...` | 查看包名和版本信息 |
| `browse [路径] [-o <目录>]` | 浏览设备文件并批量拉取 |
| `doctor` | 检查 adb 是否可用 |
| `adb-install` | adb 缺失时自动安装官方 Platform-Tools |
| 其他 adb 命令 | 原样转发给 adb，例如 `shell`、`devices`、`logcat`、`install` |

各命令的完整参数和示例：

- [使用文档首页](docs/README.md)
- [stop](docs/stop.md) · [restart](docs/restart.md) · [clear](docs/clear.md)
- [uninstall](docs/uninstall.md) · [info](docs/info.md) · [doctor](docs/doctor.md) · [adb-install](docs/adb-install.md)
- [browse](docs/browse.md) · [adb 透传](docs/passthrough.md)

## 包名关键词

应用管理命令会从设备上的完整包名中查找匹配项：

- 关键词不区分大小写，多个关键词必须同时出现在包名中。例如 `location wanandroid` 会缩小到同时包含两者的包。
- 只有一个关键词且它刚好是完整包名时，优先使用这个精确匹配。
- 没有命中会报错；命中多个时会列出候选，不会替你猜测。

因此可以先用 `info` 找到目标，再用完整包名执行停止或其他操作。

## 常用示例

~~~bash
# 关键词匹配并停止
adbx stop wanandroid

# 用多个关键词缩小范围
adbx restart location wanandroid

# 指定设备
adbx -s 3GKL2NILLN info userdictionary

# 跳过卸载确认（仅在确认目标后使用）
adbx uninstall -y wanandroid

# 浏览并拉取设备文件
adbx browse /sdcard/DCIM -o ~/pull_out

# 继续使用原生 adb
adbx shell ls /sdcard
adbx logcat -d
~~~

清除数据和卸载会删除登录状态、缓存和应用数据；这些操作通常无法恢复，请在执行前确认包名。文件拉取时本地同名文件可能被覆盖，详见 [browse 使用说明](docs/browse.md)。

## 许可证

本项目采用 MIT License，详见 [LICENSE](LICENSE)。
