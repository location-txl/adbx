# 设备绑定（use / unuse）

连接多台设备时，`use` 可以把其中一台绑定到当前终端：之后这个终端里的 adbx 和 adb 命令都默认作用于它，其他终端不受影响。绑定基于 Android 官方的 `ANDROID_SERIAL` 环境变量，原生 `adb` 命令同样遵循。

~~~bash
adbx use emulator-5554    # 绑定设备到当前终端
adbx install app.apk      # 自动安装到 emulator-5554
adbx shell                # 自动进入 emulator-5554
adbx current              # 查看当前终端绑定的设备
adbx unuse                # 解除绑定
~~~

## 安装终端集成

`use` 和 `unuse` 需要修改当前 Shell 的环境变量，因此要先安装一次终端集成（和 Python venv 的 `activate` 同理）：

~~~bash
# zsh
echo 'eval "$(adbx init zsh)"' >> ~/.zshrc

# bash
echo 'eval "$(adbx init bash)"' >> ~/.bashrc
~~~

PowerShell 把集成函数写入 profile：

~~~powershell
adbx init powershell | Add-Content $PROFILE
~~~

安装后重新打开终端即可。集成脚本只定义一个 `adbx` 包装函数：`use`/`unuse` 在函数内修改环境变量，其余命令原样交给 adbx 本体；升级 adbx 后重新打开终端就能拿到新脚本。

没有安装集成时直接运行 `adbx use`，命令会输出上面的安装方法并以非零退出码结束，不会静默失败。

## 设备标识的写法

`use` 的参数可以是：

- 完整 serial，例如 `emulator-5554` 或 `3GKL2NILLN`；
- 唯一片段，例如模拟器端口号 `5554` 能命中 `emulator-5554`；
- 省略：自动选择唯一一台可用设备。

片段命中多台设备、没有命中、命中的设备未授权或离线时，命令会报错并列出当前设备清单，不会猜测目标。

## 设备选择的优先级

adbx 按以下顺序决定命令作用的设备：

1. 显式 `-s <serial>`，优先级最高；
2. 当前终端的 `ANDROID_SERIAL`（即 `adbx use` 绑定的设备）；
3. 只连接了一台可用设备时自动选择它；
4. 多台可用设备且未指定时报错并列出清单。

透传给 adb 的命令（`shell`、`install`、`logcat` 等）由 adb 按同样的规则处理 `-s` 和 `ANDROID_SERIAL`，两边行为一致。

## 多终端并行

绑定只存在于当前终端的环境变量中：同时打开两个终端，分别 `adbx use` 不同的设备，之后各自的命令互不影响。新开的终端没有绑定，从第 3 条规则开始生效。

~~~bash
# 终端 1
adbx use 5554
adbx shell getprop ro.build.product

# 终端 2
adbx use 5556
adbx logcat -d
~~~

## current 查看绑定

`adbx current` 显示当前终端绑定的 serial、连接状态和型号；未绑定时列出当前设备，方便直接挑选。绑定的设备已断开时会明确提示。

## 解除绑定

~~~bash
adbx unuse
~~~

未绑定时运行会直接提示，不报错。注意 `unuse` 只影响当前终端；其他终端的绑定保持不变。
