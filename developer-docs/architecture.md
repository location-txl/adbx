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

根参数 -s/--serial 是全局参数。doctor 不使用 serial；透传命令在路由阶段捕获 serial，并统一转换为 adb 可识别的 -s <serial>。

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
