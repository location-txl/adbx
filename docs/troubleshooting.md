# 常见问题

## adb 找不到

如果看到“无法启动 adb，请确认已安装并在 PATH 中”，先安装 Android SDK Platform-Tools，再确认：

~~~bash
adb version
which adb      # macOS / Linux
where adb      # Windows
~~~

安装脚本完成后，如果终端仍找不到 adbx，重新打开终端，或把脚本提示的安装目录加入 PATH。

## 没有设备或设备未授权

运行：

~~~bash
adb devices
~~~

- 没有列表项：检查 USB 线、USB 调试开关和设备连接模式。
- 状态为 `unauthorized`：解锁设备并接受 USB 调试授权，再运行一次。
- 状态为 `offline`：重新插拔设备，必要时重启 adb 服务。

## 连接了多台设备

adb 无法在多台设备之间猜测目标。使用 `adb devices` 找到 serial，然后把 `-s <serial>` 放在 adbx 命令后：

~~~bash
adbx -s emulator-5554 stop wanandroid
adbx -s emulator-5554 browse
~~~

原生 adb 透传也支持同样的写法，例如 `adbx -s emulator-5554 shell ls /sdcard`。

## 关键词没有命中或命中太多

先用只读命令查看结果：

~~~bash
adbx info <关键词>
~~~

多个关键词会同时参与匹配，例如：

~~~bash
adbx info location wanandroid
~~~

然后把完整包名或更具体的关键词用于 stop、restart、clear 或 uninstall。

## browse 无法打开

`browse` 需要交互式终端。如果把输出重定向到文件、在不支持 TTY 的任务环境中运行，命令会直接退出。请在本地终端运行：

~~~bash
adbx browse /sdcard
~~~

起始路径必须是设备端绝对路径，例如 `/sdcard/DCIM`；相对路径会被拒绝。指定的本地输出目录不存在时会自动创建。

## browse 读取或保存失败

无权限目录、设备特殊文件和设备断开都可能导致读取或保存失败。回到可访问的目录后重试。预览和编辑单次最多读取 10 MiB；二进制或超大文件不支持文本编辑。

保存文本时，如果 adbx 发现远端文件在编辑期间发生变化，会要求明确选择是否覆盖。取消后可重新打开文件，确认内容后再保存。

## 文件拉取结果与预期不同

`browse` 会按设备端路径的最后一段名称写入输出目录。不同目录下的同名条目可能落到同一个本地路径；已有同名文件可能被覆盖，同名目录会合并。传输过程中按 q、Esc 或 Ctrl-C 取消时，未完成条目会保留标记以便重试，但本地可能留下半截文件。
