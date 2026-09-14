# 快速上手

## 1. 准备 adb

先安装 adbx。主机没有 adb 时运行：

~~~bash
adbx adb-install
~~~

命令会从官方 Platform-Tools 下载对应平台的压缩包，并在下载过程中显示进度。安装完成后重新打开终端，或按命令输出加载 shell 配置；Windows 下已有终端宿主可能需要完全退出后再打开。已有 adb 时命令会直接提示版本和路径，不会重复下载。

也可以手动安装 [Android SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools)，确保下面的命令能找到 adb：

~~~bash
adb version
~~~

## 2. 准备设备

在 Android 设备的开发者选项中打开 USB 调试，用 USB 连接设备，然后运行：

~~~bash
adb devices
~~~

第一次连接时，设备会询问是否允许这台电脑调试。选择允许，并再次运行 `adb devices`；列表中的状态应为 `device`。

如果连接了多台设备，记下要操作的 serial，后续用 `-s` 指定：

~~~bash
adbx -s emulator-5554 info setting
~~~

## 3. 检查 adbx

~~~bash
adbx doctor
~~~

看到 `✓ adb 已安装` 表示本机 adb 已能启动。doctor 只检查 adb，不检查具体设备；设备连接状态仍以 `adb devices` 为准。

## 4. 找到并查看应用

不需要先知道完整包名：

~~~bash
adbx info setting
~~~

`info` 会列出命中的完整包名、versionName 和 versionCode。关键词命中多个应用时，info 会全部展示；其他应用管理命令会要求你缩小关键词范围。

## 5. 执行应用操作

确认目标后，可以停止或重启：

~~~bash
adbx stop wanandroid
adbx restart location wanandroid
~~~

清除数据和卸载不可恢复。卸载默认会询问确认：

~~~bash
adbx clear wanandroid
adbx uninstall wanandroid
~~~

脚本场景可使用 `adbx uninstall -y <关键词>` 跳过确认，但应先用 `info` 验证命中结果。

## 6. 浏览设备文件

~~~bash
adbx browse
~~~

默认从 `/sdcard` 开始。选中文件或文件夹后按 Space 标记，按 Enter 拉取到当前目录；也可以指定起始路径和本地输出目录：

~~~bash
adbx browse /sdcard/DCIM -o ~/pull_out
~~~

浏览器需要真实的交互式终端。完整快捷键、预览、编辑和传输限制见 [browse 使用说明](browse.md)。

## 下一步

- [命令一览](README.md#命令)
- [adb 透传](passthrough.md)
- [常见问题](troubleshooting.md)
