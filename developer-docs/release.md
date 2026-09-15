# 构建与发布

## 本地验证

提交前按改动范围运行：

~~~bash
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
~~~

Release 构建：

~~~bash
cargo build --release
~~~

这些命令验证源码和构建配置，不替代真实设备上的 adb、TUI 和安装脚本验收。

## Platform-Tools 下载

`adb-install` 使用 ureq 2：Linux 启用 rustls，macOS/Windows 显式配置系统 TLS。
关闭默认 features，避免非 Linux 目标引入 rustls；代理由命令按环境变量优先级显式配置。
连接超时为 30 秒，单次读写超时为 60 秒，不限制整个下载的总时长。

未使用 minreq 3.0.0，因为它把单次 TCP 短读当作 CONNECT 响应结束，合法分片会失败，
响应头恰为 256 字节时还可能挂起。`tests/adb_install_proxy.rs` 使用本地代理覆盖这两个边界，
检查客户端收到完整响应后发送 TLS 握手，并检查错误输出不含代理凭据；不访问外部网络。
切换客户端时保留这些回归测试。使用 ureq 会保留 URL 解析依赖，不宣称实现 minreq 的体积收益。

## GitHub Actions 流程

.github/workflows/release.yml 在推送 v* Tag 后执行：

1. 在 Ubuntu 上运行 cargo test --locked 和 cargo clippy --all-targets --locked -- -D warnings。
2. 按矩阵安装目标 toolchain，校验 Tag 与 Cargo.toml、Cargo.lock 的版本一致。
3. 为每个平台构建 release 二进制并打包。
4. 检查得到五个资产后创建或更新 GitHub Release。

当前构建矩阵：

| 平台 | Rust target | 资产格式 |
| --- | --- | --- |
| Windows x64 | x86_64-pc-windows-msvc | zip |
| Windows ARM64 | aarch64-pc-windows-msvc | zip |
| Linux x64 | x86_64-unknown-linux-gnu | tar.gz |
| macOS Intel | x86_64-apple-darwin | tar.gz |
| macOS Apple Silicon | aarch64-apple-darwin | tar.gz |

版本 Tag 必须以 v 开头并符合 Cargo 可接受的语义化版本。工作流会在临时构建环境同步根包版本，不会回写提交。

## 安装脚本

install.sh 面向 macOS/Linux，install.ps1 面向 Windows。脚本会根据平台和架构选择 Release 资产，在替换最终文件前先下载、解压并校验临时文件。

- Shell 默认目录为 ~/.local/bin，可用 --version 和 --install-dir 覆盖。
- PowerShell 默认目录为 %LOCALAPPDATA%\\adbx\\bin，可用 -Version 和 -InstallDir 覆盖。
- 使用 latest 时脚本从 GitHub Release 的重定向结果解析稳定版本；指定版本时校验 Tag 格式，避免把任意字符串拼进下载地址。

修改发布资产命名、版本规则或脚本参数时，要同时检查 workflow、两个安装脚本和 README 的安装说明。
