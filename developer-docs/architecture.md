# 架构与边界

## 项目结构

adbx 是 Rust edition 2024 项目，同时提供库和二进制：

~~~
src/main.rs        命令行入口
src/lib.rs         库模块导出
src/commands/      每个 adbx 命令一个模块
src/adb.rs         adb 调用边界
src/matching.rs    包名匹配纯函数
~~~

调用方向保持单向：

~~~
main.rs
  → commands/<name>.rs
    → adb.rs
~~~

命令模块不直接创建 adb 进程。这样设备选择、错误格式和调用参数集中在一个边界内，命令逻辑也能在没有设备时测试。

## 命令注册和分发

新增命令需要同时完成：

1. 在 src/main.rs 的 Command enum 中定义 clap 参数和帮助文字。
2. 在 src/commands/mod.rs 注册模块。
3. 在 src/commands/<name>.rs 提供命令入口；设备命令调用 src/adb.rs，主机环境命令只处理本机 I/O。
4. 在 docs/ 增加面向使用者的说明；若实现机制有变化，再更新本目录。

根参数 -s/--serial 是全局参数，clap 声明 `env = "ANDROID_SERIAL"`，因此优先级为：显式 `-s` > 终端绑定（`adbx use` 写入的环境变量）> 自动选择。doctor 不使用 serial；透传命令在路由阶段捕获 serial，并统一转换为 adb 可识别的 -s <serial>。

自有命令在分发前通过 clap 的 `ValueSource` 区分参数来源：空环境变量视为未绑定，显式空 serial 报错，避免脚本的空变量触发自动设备选择；透传保留 adb 原生行为。

设备类命令在分发处（main.rs 的 run）统一调 `commands::device::auto_serial`：显式值原样透传；缺省时解析 `adb devices -l` 并自动选择唯一可用设备，零台或多台可用时报错并列出清单。doctor、adb-install 与终端集成命令（use/unuse/current/init/__resolve-device）不经过它，保持无设备可用。设备枚举的 I/O 与解析在 adb.rs（list_devices + parse_devices_output），选择与匹配语义（唯一可用、serial 唯一片段、状态校验）在 commands/device.rs 的纯函数中。

终端集成（commands/init.rs 输出 zsh/bash/powershell 包装函数）：Shell 函数拦截 `use`/`unuse` 修改当前终端的 `ANDROID_SERIAL`；设备解析由隐藏命令 `__resolve-device` 完成并向 stdout 输出一行 serial，脚本只捕获输出、不拼接 Shell 代码。透传不经 clap，adb 子进程继承 `ANDROID_SERIAL` 后行为与 adbx 一致。

包装函数在捕获输出或修改绑定前扫描 `use`/`unuse` 的帮助旗标，遇到 `--` 停止扫描；帮助请求交给公开命令，避免帮助正文被保存为 serial。Bash/zsh 使用 `${1:-}` 兼容 `set -u` 下的无参数调用。`tests/device_session.rs` 通过独立子进程、模拟 adb 和生成的脚本验证这些行为，不修改测试进程的环境变量，也不访问真实设备。

该集成测试在 Unix 上运行 Bash，在 macOS 上额外运行系统自带的 zsh。PowerShell 行为测试需要 PATH 中的 `pwsh`，默认忽略，具备环境时执行 `cargo test --test device_session powershell_help -- --ignored`。

adb-install 是主机环境命令，不访问设备。它在 PATH 和 adbx 用户目录中检查 adb，缺失时下载并校验官方 Platform-Tools，再按 Bash、zsh 或 fish 的实际启动规则写入用户级 PATH；Windows 写入注册表后广播 `WM_SETTINGCHANGE/Environment`。下载进度通过分块读取实时输出。

## adb 调用边界

src/adb.rs 提供两类执行方式：

- 捕获式调用：命令读取 stdout，非零退出码带上 adb 错误返回。
- 透传式调用：直接继承 stdio，在 Unix 上替换当前进程，保证交互、信号和退出码与 adb 一致。

应用管理命令通过 list_packages 获取设备包名，再由 matching::match_packages 完成精确优先、大小写不敏感和多关键词 AND 匹配。设备 I/O 不进入匹配函数。

## 错误与输出

命令入口返回 anyhow::Result。main 统一把错误打印为 ✗ 开头并以退出码 1 结束；成功动作在命令模块打印 ✓ 结果。多候选和零候选属于命令层可解释错误，不应在 adb 层伪装成设备错误。

uninstall 额外检查部分 Android 版本中 pm uninstall 退出码与输出不一致的情况；info 对缺失版本字段降级为“未知”。

## 透传路由

src/commands/passthrough.rs 在 clap 解析前分类参数：

~~~
扫描 -s/--serial
  ├─ 已知 adbx 子命令或 adbx 自身旗标 → 交给 clap
  ├─ 未知 token 或 adb 旗标 → 原样转发
  └─ 空参数或缺少 serial 值 → 交给 clap 报错
~~~

serial 之后的 OsString 参数不再二次解释，因此能保留非 UTF-8 参数及 adb 自己的旗标。

## 测试分工

- matching.rs、命令解析和 browse 的路径/排序/筛选逻辑使用纯函数单测。
- adb.rs 的正常调用、错误处理和解析逻辑可用单测覆盖。
- 标记拉取、真实目录创建和设备文件操作需要真机或模拟器；相关测试显式标记为 ignored，不能把普通 cargo test 的通过当成设备验收。
- 修改 docs 只需做链接和命令内容核对；涉及源码时运行 cargo build、cargo clippy --all-targets -- -D warnings 和 cargo test。
