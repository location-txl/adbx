# adb 透传

adbx 接管自己的命令；其他调用会原样交给 adb。这样可以一直使用 adbx 作为入口，不必在 adbx 和 adb 之间切换。

## 可以直接使用的命令

以下命令会透传给 adb：

~~~bash
adbx devices
adbx shell ls /sdcard
adbx logcat -d
adbx install app.apk
~~~

透传命令的标准输出、错误输出、交互终端、Ctrl-C 和退出码都保持 adb 的行为。

## 指定设备

下面四种写法都会把 serial 转换为 adb 使用的 -s <serial>：

~~~bash
adbx -s emulator-5554 shell ls /sdcard
adbx -semulator-5554 shell ls /sdcard
adbx --serial emulator-5554 shell ls /sdcard
adbx --serial=emulator-5554 shell ls /sdcard
~~~

serial 后面的参数不再由 adbx 解释，会原样交给 adb。例如：

~~~bash
adbx shell ls -l
adbx shell --serial x
~~~

第二条命令中的 --serial 是 shell 参数，不会被 adbx 当作设备选择参数。

## 路由规则

- 已知的 adbx 命令（stop、clear、restart、uninstall、info、browse、doctor、adb-install，以及 help）由 adbx 处理。
- -s、--serial、--help、--version 等 adbx 自身参数由 adbx 处理。
- 其他第一个有效参数按 adb 调用透传，例如 shell、devices、-d。
- 空参数、只有 -s 或 -s 缺少值时，由 adbx 显示参数错误。

## 示例

~~~bash
adbx -d shell echo hi
adbx shell false
echo $?
~~~

上例中的退出码由 adb 返回。未知命令也会看到 adb 自身的错误：

~~~text
adbx badcmd
adb: unknown command badcmd
~~~
