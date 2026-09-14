# adb-install：自动安装 adb

`adb-install` 检查本机是否已有可运行的 adb。缺失时下载 Google 官方 Android SDK Platform-Tools，显示下载进度，安装到用户目录，并把安装目录加入用户级 PATH。

## 用法

~~~text
adbx adb-install
~~~

命令不需要设备，也没有额外参数。全局 `-s` 参数会被忽略。

## 已有 adb

如果当前 PATH 中已有可运行的 adb，命令不会下载或覆盖文件：

~~~text
✓ 已有 adb，路径：/path/to/adb，版本：1.0.41
~~~

如果 PATH 中找到了 adb 文件但无法运行，命令会报错并保留原文件。

## 自动安装

命令根据当前平台下载官方 Platform-Tools：

- Linux x64：`platform-tools-latest-linux.zip`
- macOS Intel/Apple Silicon：`platform-tools-latest-darwin.zip`
- Windows x64/ARM64：`platform-tools-latest-windows.zip`（Google 未发布 ARM64 版，ARM64 上由 Windows 11 系统模拟层运行 x64 adb）

下载时会显示进度。交互终端使用单行刷新；输出被重定向或运行在 CI 中时，每隔一段大小输出一行。服务端没有提供总大小时，会持续显示已下载大小。

默认安装目录：

- macOS/Linux：`~/.local/share/adbx/platform-tools`
- Windows：`%LOCALAPPDATA%\adbx\platform-tools`

Unix 会按当前 shell 的启动规则写入 PATH：Bash 同时覆盖登录 shell 使用的 `.bash_profile`、`.bash_login` 或 `.profile`，以及非登录 shell 使用的 `.bashrc`；zsh 使用 `${ZDOTDIR:-$HOME}/.zshrc`；fish 使用 `${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish`。未识别的 shell 不会伪造配置成功，命令会返回非零退出码并打印需要手动加入 PATH 的目录。

安装成功后需要重新打开终端，或者按命令输出执行 `source` 加载 shell 配置。Windows 会广播用户环境变量变更，但已有终端宿主或 IDE 可能仍缓存旧 PATH，需要完全退出后重新打开。之后可以使用 `adb devices`，也可以继续使用 `adbx devices`。

## 失败处理

网络失败、压缩包损坏、平台不支持、下载的 adb 无法运行或 PATH 写入失败时会返回非零退出码。临时文件会被清理，已有安装目录不会被删除或覆盖。

也可以手动从 [Android SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools) 安装 adb。安装后运行 `adbx doctor` 检查 PATH 是否生效。
