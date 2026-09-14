# doctor：检查 adb 环境

检查本机是否安装 adb、是否能从 PATH 启动，并显示 adb 版本。doctor 不访问 Android 设备。

## 用法

~~~text
adbx doctor
~~~

没有参数。即使写了全局 -s，doctor 也不会使用它。

## 结果

adb 可用时：

~~~text
✓ adb 已安装，版本 1.0.41
~~~

adb 未安装或不在 PATH 中时：

~~~text
✗ 无法启动 adb，请确认已安装并在 PATH 中
~~~

没有 adb 时可以运行 `adbx adb-install` 自动下载官方 Platform-Tools。修复 PATH 后重新打开终端，再运行一次 doctor。设备连接问题请使用 adb devices 检查。

## 相关说明

- [快速上手](quickstart.md) 从安装 adb 开始介绍完整流程。
- [常见问题](troubleshooting.md) 说明 adb 找不到和设备未授权的处理方法。
