# adbx 使用文档

这里的内容面向使用 adbx 的人：安装、连接设备、命令参数、操作限制和常见问题。

## 推荐阅读顺序

1. [快速上手](quickstart.md)：从安装 adb 到执行第一条 adbx 命令。
2. [命令一览](#命令)：选择要完成的操作。
3. [常见问题](troubleshooting.md)：处理 adb、设备选择和终端相关问题。

## 基本格式

~~~text
adbx [-s <serial>] <命令> [参数]
~~~

`-s` 可以指定设备 serial。serial 可通过 `adb devices` 查看；只有一台设备连接时通常可以省略。

adbx 是 adb 的超集。stop、clear、restart、uninstall、info、browse、doctor 和 `adb-install` 由 adbx 处理；其他命令直接交给 adb，因此 `adbx devices`、`adbx shell`、`adbx logcat` 和 `adbx install` 都可以直接使用。透传时 stdout、stderr 和退出码与 adb 保持一致，详见 [adb 透传](passthrough.md)。

## 包名匹配

stop、restart、clear、uninstall 和 info 都接受一个或多个关键词：

- 不区分大小写；
- 多个关键词是 AND 关系，包名必须同时包含所有关键词；
- 单个关键词刚好等于完整包名时，优先精确匹配；
- stop、restart、clear、uninstall 遇到多个候选会停止执行并列出候选；info 是只读命令，会展示全部候选；
- 零命中会报错并返回非零退出码。

不确定目标时，先运行 `adbx info <关键词>`。

## 命令

| 命令 | 说明 |
| --- | --- |
| [stop](stop.md) | 停止应用 |
| [restart](restart.md) | 重启应用 |
| [clear](clear.md) | 清除应用数据 |
| [uninstall](uninstall.md) | 卸载应用 |
| [info](info.md) | 查看版本信息 |
| [browse](browse.md) | 浏览、预览、编辑和拉取设备文件 |
| [doctor](doctor.md) | 检查 adb 环境 |
| [adb-install](adb-install.md) | adb 缺失时自动安装官方 Platform-Tools |
| [adb 透传](passthrough.md) | 使用所有未被 adbx 接管的 adb 命令 |

## 需要特别确认的操作

`clear` 会清除应用数据，`uninstall` 会删除应用及其数据。两者都可能删除登录状态、缓存和本地数据库，通常无法恢复。卸载默认要求输入确认；脚本中使用 `-y` 时请先确保关键词只命中目标应用。

`browse` 拉取文件时会写入本地目录。同名文件可能被覆盖，同名目录会合并；取消传输后本地可能留下未完成的文件。

## 遇到问题

先运行：

~~~bash
adbx doctor
adb devices
~~~

然后查看 [常见问题](troubleshooting.md)。如果命令本身的参数不确定，可以运行 `adbx --help` 或 `adbx <命令> --help`。
