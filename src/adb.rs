//! adb 命令调用封装。
//!
//! 统一处理设备选择（-s）、错误透传（带出 adb 的 stderr），
//! 供各子命令复用，避免散落各处的 Command 拼装。

use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// 执行一条 adb 命令，返回 stdout。
///
/// * `serial` - 目标设备 serial，会转发为 `adb -s <serial>`；传 `None` 时由 adb
///   自行选择唯一连接的设备（无设备或多设备时 adb 会报错并透传给调用方）
/// * `args` - adb 之后的参数，如 `["shell", "pm", "list", "packages"]`
///
/// 返回 stdout 内容（UTF-8 容错解码）。adb 不存在或退出码非 0 时返回 Err，
/// 错误信息附带 adb 的 stderr 原文。
///
/// # 示例
///
/// ```ignore
/// let out = run_adb(None, &["shell", "pm", "list", "packages"])?;
/// ```
pub fn run_adb(serial: Option<&str>, args: &[&str]) -> Result<String> {
    let (stdout, _) = run_adb_raw(serial, args)?;
    Ok(stdout)
}

/// 执行一条 adb 命令，返回 (stdout, stderr)。退出码非 0 时返回 Err。
///
/// 供需要检查 stderr 的调用方使用（如 [`uninstall`]：部分 Android 的
/// `pm uninstall` 失败时退出码仍为 0，只把报错打在 stderr），其余命令用 [`run_adb`] 即可。
/// 拼装 adb 命令并完成 `-s <serial>` 设备选择转发（OsStr 版，唯一实现）。
///
/// 透传路径的参数是用户原始 OsString（可能非 UTF-8），设备选择的唯一
/// 实现放在这里；str 版 [`adb_command`] 只是它的便捷封装。
fn adb_command_os(serial: Option<&OsStr>) -> Command {
    let mut cmd = Command::new("adb");
    if let Some(serial) = serial {
        cmd.arg("-s").arg(serial);
    }
    cmd
}

/// 拼装 adb 命令，供本文件各调用点复用，保证设备选择行为只有一份实现。
fn adb_command(serial: Option<&str>) -> Command {
    adb_command_os(serial.map(OsStr::new))
}

fn run_adb_raw(serial: Option<&str>, args: &[&str]) -> Result<(String, String)> {
    let (stdout, stderr) = run_adb_raw_bytes(serial, args)?;
    Ok((
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

/// 执行 adb 并保留 stdout/stderr 原始字节，供文件预览等二进制场景使用。
fn run_adb_raw_bytes(serial: Option<&str>, args: &[&str]) -> Result<(Vec<u8>, Vec<u8>)> {
    let output = adb_command(serial)
        .args(args)
        .output()
        .context("无法启动 adb，请确认已安装并在 PATH 中")?;
    if !output.status.success() {
        anyhow::bail!(
            "adb {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok((output.stdout, output.stderr))
}

/// 原样透传执行 adb：以 adb 子进程替换本进程，成功时本函数不返回。
///
/// 供未知子命令透传（passthrough）使用。与 [`run_adb`] 的捕获式执行不同，
/// 透传要求 stdio 直接继承终端——`adb shell` 交互、`logcat` 流式输出、
/// 颜色/TTY 全部正常；Ctrl+C 等信号直达 adb；退出码原样返回给调用脚本
/// （`adb shell false` 的非零码不会被吞掉）。Unix 下用 exec 进程替换实现
/// （adbx 进程被 adb 替换，零额外开销），其他平台退化为 status + exit。
/// `serial` 为路由层捕获归一化的值，以 `-s <serial>` 前置；`args` 为用户
/// 原始参数（OsString，不二次解释）。adb 不存在时返回 Err（提示安装）。
pub fn exec_adb(serial: Option<&OsStr>, args: &[OsString]) -> Result<()> {
    let mut cmd = adb_command_os(serial);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec 成功后本进程即 adb，永不返回；仅启动失败（如 adb 缺失）时返回 Err
        let err = cmd.exec();
        anyhow::bail!("无法启动 adb，请确认已安装并在 PATH 中：{err}");
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .context("无法启动 adb，请确认已安装并在 PATH 中")?;
        // 退出码原样透传给调用方，信号终止（无码）按 1 处理
        std::process::exit(status.code().unwrap_or(1))
    }
}

/// 从 `adb version` 输出中解析版本号。
///
/// adb 首行输出形如 `Android Debug Bridge version 1.0.41`，
/// 取 `version ` 之后的部分；无法识别时返回 None。
fn parse_adb_version(output: &str) -> Option<String> {
    let first_line = output.lines().next()?.trim();
    first_line
        .strip_prefix("Android Debug Bridge version ")
        .map(str::to_owned)
}

/// 获取本机 adb 版本号（`adb version`）。
///
/// 与设备无关，不使用 serial。adb 不存在时返回 Err（提示安装）；
/// 版本行格式异常时退回首行原文，不视为错误。
pub fn adb_version() -> Result<String> {
    let out = run_adb(None, &["version"])?;
    Ok(parse_adb_version(&out)
        .unwrap_or_else(|| out.lines().next().unwrap_or_default().trim().to_owned()))
}

/// 获取设备上已安装的全部包名。
///
/// 基于 `pm list packages`，已去掉每行的 `package:` 前缀。
/// 返回包名列表（顺序与设备输出一致，通常为字典序）。
pub fn list_packages(serial: Option<&str>) -> Result<Vec<String>> {
    let out = run_adb(serial, &["shell", "pm", "list", "packages"])?;
    Ok(out
        .lines()
        // 过滤掉空行和异常输出，只保留 package: 前缀的行
        .filter_map(|line| line.strip_prefix("package:"))
        .map(str::to_owned)
        .collect())
}

/// 强制停止指定包名的应用（`am force-stop`）。
///
/// * `package` - 完整包名；应用未在运行时该命令也会成功，调用方无需预检查
///
/// 失败（包不存在、设备断开等）时返回 Err 并透传 adb 的 stderr。
pub fn force_stop(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(serial, &["shell", "am", "force-stop", package])?;
    Ok(())
}

/// 启动指定包名的应用入口 Activity。
///
/// 通过 `monkey -p <pkg> -c android.intent.category.LAUNCHER 1` 拉起，
/// 一次调用即可，无需先解析入口 Activity 名。
///
/// * `package` - 完整包名；应用须有 launcher Activity（纯服务类应用没有），
///   否则 monkey 以非 0 退出，错误透传给调用方
pub fn launch_app(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(
        serial,
        &[
            "shell",
            "monkey",
            "-p",
            package,
            "-c",
            "android.intent.category.LAUNCHER",
            "1",
        ],
    )?;
    Ok(())
}

/// 获取指定包名的 dumpsys 信息（`dumpsys package <pkg>`）。
///
/// * `package` - 完整包名；包不存在时输出可能为空，由调用方解析降级
///
/// 返回 dumpsys 原文，版本字段解析由调用方（info 命令）完成。
/// 设备断开等失败场景返回 Err 并透传 adb 的 stderr。
pub fn dump_package(serial: Option<&str>, package: &str) -> Result<String> {
    run_adb(serial, &["shell", "dumpsys", "package", package])
}

/// 清除指定包名的应用数据（`pm clear`）。
///
/// * `package` - 完整包名
///
/// 该操作会同时强制停止应用。失败（包不存在、设备断开等）时返回 Err 并透传 adb 的 stderr。
pub fn clear_data(serial: Option<&str>, package: &str) -> Result<()> {
    run_adb(serial, &["shell", "pm", "clear", package])?;
    Ok(())
}

/// 卸载指定包名的应用（`pm uninstall`）。
///
/// * `package` - 完整包名
///
/// **该操作不可恢复**：应用及其全部数据会被删除。
/// 除退出码外还需校验输出：部分 Android 的 pm uninstall 失败时退出码仍为 0
/// （实测 Android 7.1：stderr 打 Java 异常、stdout 为空），只把报错打在输出里。
/// 校验不通过时返回 Err 并透传 pm 的报错原文。
pub fn uninstall(serial: Option<&str>, package: &str) -> Result<()> {
    let (stdout, stderr) = run_adb_raw(serial, &["shell", "pm", "uninstall", package])?;
    if !is_uninstall_success(&stdout, &stderr) {
        // 优先报 stderr（异常栈），stdout 无内容时兜底（Failure 场景）
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        anyhow::bail!("卸载 {package} 失败：{detail}");
    }
    Ok(())
}

/// 判断 pm uninstall 的输出是否表示成功（纯函数，可单测）。
///
/// 成功时 stdout 为 `Success` 且 stderr 为空。失败时要么 stderr 非空
/// （Java 异常），要么 stdout 含 `Failure`，二者任一出现即视为失败。
fn is_uninstall_success(stdout: &str, stderr: &str) -> bool {
    stderr.trim().is_empty() && !stdout.contains("Failure")
}

/// 设备端 shell 参数转义：整体单引号包裹，内部单引号转为 `'\''`（纯函数，可单测）。
///
/// adb 会把 `shell` 子命令的参数用空格拼接后交给设备端 shell 解释，
/// 路径含空格、`$`、`*` 等字符时必须转义；`pull`/`push` 的路径不经 shell，无需转义。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 列出设备端目录内容（`adb shell ls -l <path>/`）。
///
/// * `path` - 设备端路径；内部已做 shell 转义，含空格等特殊字符安全
///
/// 强制追加尾部 `/`：路径本身是指向目录的符号链接（如 `/sdcard`）时，
/// 不加斜杠 ls 会列出链接自身而非目录内容（真机实测）。
/// 返回 `ls -l` 原始输出（toybox 格式，Android 6+），行解析由调用方
/// （browse 命令的 `parse_ls`）完成，本函数只管 I/O。
/// 无权限、路径不存在等失败返回 Err 并透传 stderr。
pub fn shell_list_dir(serial: Option<&str>, path: &str) -> Result<String> {
    let path = format!("{}/", path.trim_end_matches('/'));
    run_adb(serial, &["shell", "ls", "-l", &shell_quote(&path)])
}

/// 在设备端创建目录（`adb shell mkdir <path>`，单层，不加 -p）。
///
/// * `path` - 设备端绝对路径；内部已做 shell 转义，含空格等特殊字符安全
///
/// 除退出码外还校验 stderr：mkdir 成功时 stderr 恒空，失败只向 stderr 输出
/// 报错（仿 [`uninstall`]，兜住部分设备 adb shell 不回传退出码的情形）。
/// 目标已存在、无权限等失败返回 Err 并透传设备端报错原文。
pub fn shell_mkdir(serial: Option<&str>, path: &str) -> Result<()> {
    let (_, stderr) = run_adb_raw(serial, &["shell", "mkdir", &shell_quote(path)])?;
    if !stderr.trim().is_empty() {
        anyhow::bail!("mkdir {path} 失败：{}", stderr.trim());
    }
    Ok(())
}

/// 读取设备端文件的前 `max_bytes + 1` 个字节（`adb exec-out`，保留二进制内容）。
///
/// * `path` - 设备端文件路径；通过设备端 shell 单引号转义，含空格和特殊字符安全
/// * `max_bytes` - 调用方允许预览的最大字节数；多读一个字节用于判断文件是否超限
///
/// 返回的字节可直接交给文本解码器或图片解码器。文件不存在、目标不是普通文件、
/// 设备断开或 adb 不可用时返回 Err；文件超过上限时由调用方根据返回长度决定是否拒绝展示。
pub fn read_file(serial: Option<&str>, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let read_limit = max_bytes.saturating_add(1);
    let command = read_file_command(path, max_bytes);
    let args = ["exec-out", "sh", "-c", command.as_str()];
    let (mut stdout, _) = run_adb_raw_bytes(serial, &args)?;
    // dd 按固定块读取，最后一个块可能略大于限制；在本地截断，避免额外依赖设备端命令。
    stdout.truncate(read_limit);
    Ok(stdout)
}

/// 生成 Android 常见 `dd` 语法的读取命令；按块读取避免 `head -c` 在旧版 toybox 上不兼容。
/// `test -f` 会在打开目标前拒绝目录、FIFO、TTY 等非普通文件，避免同步读取永久等待。
/// `exec-out` 是原始字节流，必须丢弃 dd 的统计输出，避免污染预览内容。
fn read_file_command(path: &str, max_bytes: usize) -> String {
    const BLOCK_SIZE: usize = 4096;
    let blocks = max_bytes.saturating_add(1).div_ceil(BLOCK_SIZE);
    let quoted_path = shell_quote(path);
    format!(
        "test -f {quoted_path} || exit 1; dd if={quoted_path} bs={BLOCK_SIZE} count={blocks} 2>/dev/null"
    )
}

/// 解析 `du -s` 多路径输出（纯函数，可单测）。
///
/// toybox 行格式 `KB数\t路径`（路径与入参一致，可含中文/空格）；按路径回填到
/// 与 `paths` 等长的字节数组（KB × 1024）。设备上已不存在的路径没有对应行，
/// 保持 0；du 的报错行、格式异常行直接跳过。
fn parse_du_lines(output: &str, paths: &[String]) -> Vec<u64> {
    let mut sizes = vec![0u64; paths.len()];
    for line in output.lines() {
        let Some((kb, path)) = line.split_once('\t') else {
            continue;
        };
        let Ok(kb) = kb.trim().parse::<u64>() else {
            continue;
        };
        if let Some(i) = paths.iter().position(|p| p == path) {
            sizes[i] = kb.saturating_mul(1024);
        }
    }
    sizes
}

/// 批量查询设备端路径总大小（`adb shell du -s`，一次往返，KB 换算为字节）。
///
/// 仅用于进度展示：任一路径不存在/无权限只会让 du 整体退出非 0，stdout 里
/// 其余路径的大小行仍然有效，因此忽略退出码、只取 stdout 解析（缺行保持 0，
/// 0 表示大小未知，调用方跳过百分比）；设备断开、adb 缺失时 stdout 为空，
/// 同样返回全 0，不向上报错。
pub fn du_totals(serial: Option<&str>, paths: &[String]) -> Vec<u64> {
    if paths.is_empty() {
        return Vec::new();
    }
    let quoted: Vec<String> = paths.iter().map(|p| shell_quote(p)).collect();
    let mut args: Vec<&str> = vec!["shell", "du", "-s"];
    args.extend(quoted.iter().map(String::as_str));
    let out = adb_command(serial)
        .args(&args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    parse_du_lines(&out, paths)
}

/// 拉取设备端文件或目录到本地（`adb pull`，目录递归），并按本地落盘字节回报进度。
///
/// * `remote` - 设备端路径（adb 协议直接传输，不经设备端 shell，无需转义）
/// * `local` - 本地目标目录，原样传给 adb（接受非 UTF-8 路径）；文件落在
///   `local/<名字>`，目录落在 `local/<目录名>/`，进度即轮询该落点的递归大小
/// * `total_bytes` - 预期总字节（[`du_totals`] 的产物）；传 0 表示大小未知，不回报进度
/// * `should_abort` - 中止检查，每轮轮询（约 150ms 一次）前调用；返回 true 即
///   杀掉 adb pull 子进程并返回 `Ok(false)`。不含事件读取逻辑，由调用方注入，
///   本模块不依赖终端事件库
/// * `on_progress` - 进度回调（0-100，仅在值变化时触发；成功收尾定格 100）
///
/// 返回 `Ok(true)` 表示完整拉取完成；`Ok(false)` 表示调用方主动取消（取消时
/// 本地可能残留半截文件，由调用方负责提示）；失败返回 Err 并透传 adb 的 stderr。
/// adb 1.0.41（platform-tools 35，pty 下实测）已不再输出传输百分比，
/// 进度改为每 150ms 轮询本地落点的递归大小除以 `total_bytes`，是近似值
/// （du 按磁盘块统计略偏大、KB 粒度），可能提前到 100 或收尾停在 99，仅作展示。
/// 本地同名文件会被覆盖、同名目录合并。
pub fn pull_with_progress(
    serial: Option<&str>,
    remote: &str,
    local: &Path,
    total_bytes: u64,
    should_abort: impl Fn() -> bool,
    mut on_progress: impl FnMut(u8),
) -> Result<bool> {
    let mut child = adb_command(serial)
        .arg("pull")
        .arg(remote)
        .arg(local)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("无法启动 adb，请确认已安装并在 PATH 中")?;

    // 两个管道各自交给独立线程持续排空（见 spawn_drain 注释），
    // 否则输出超过管道缓冲时 adb 阻塞在写端、try_wait 永远等不到退出
    let out_pipe = child.stdout.take().map(spawn_drain);
    let err_pipe = child.stderr.take().map(spawn_drain);
    // 进度观测的本地落点：adb pull 的落盘位置即 local/<remote 的最后一段名字>
    let watch = local.join(remote.rsplit('/').next().unwrap_or(remote));

    let mut last: Option<u8> = None;
    loop {
        // 中止检查放在最前：取消优先于进度回报与退出判定
        if should_abort() {
            // 杀掉 adb pull 并回收子进程；本地残留半截文件由调用方提示
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        if let Some(status) = child.try_wait().context("等待 adb pull 结束失败")? {
            let out = out_pipe.map(join_drain).unwrap_or_default();
            let err = err_pipe.map(join_drain).unwrap_or_default();
            if !status.success() {
                // stderr 为空时兜底 stdout（个别失败形态只打 stdout）
                let detail = if err.trim().is_empty() { out } else { err };
                anyhow::bail!("adb pull 失败：{}", detail.trim());
            }
            if total_bytes > 0 && last != Some(100) {
                on_progress(100);
            }
            return Ok(true);
        }
        // total_bytes 为 0（大小未知）时 checked_div 得 None，跳过进度回报
        if let Some(pct) = (tree_size(&watch).min(total_bytes) * 100)
            .checked_div(total_bytes)
            .map(|v| v as u8)
            && last != Some(pct)
        {
            last = Some(pct);
            on_progress(pct);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// 在独立线程读空一个子进程管道并容错解码为字符串（读失败按空处理）。
///
/// 必须与轮询等待并行：adb pull 目录时对每个特殊文件/失效符号链接都会立即
/// 向 stderr 写一行警告，若等到子进程退出后才读管道，输出累计超过管道缓冲
/// （macOS 默认 16KB）会让 adb 阻塞在写端永不退出，轮询循环随之死锁。
fn spawn_drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// 等 reader 线程结束，取回管道全文（线程意外中止按空处理）。
fn join_drain(handle: std::thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

/// 递归统计本地路径的逻辑大小（纯本地 I/O，不涉设备）：
/// 文件取字节长度，目录递归求和；不跟随符号链接，读取失败的子项按 0 计。
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return 0;
    };
    read_dir.flatten().map(|e| tree_size(&e.path())).sum()
}

#[cfg(test)]
mod tests {
    use super::{
        du_totals, is_uninstall_success, parse_adb_version, parse_du_lines, pull_with_progress,
        read_file_command, shell_quote, tree_size,
    };

    #[test]
    fn parses_standard_version_output() {
        let out = "Android Debug Bridge version 1.0.41\nVersion 35.0.0-11465562\n";
        assert_eq!(parse_adb_version(out).as_deref(), Some("1.0.41"));
    }

    #[test]
    fn shell_quote_wraps_plain_and_special_names() {
        // 普通名字与含空格/通配符的名字：整体单引号包裹即安全
        assert_eq!(shell_quote("DCIM"), "'DCIM'");
        assert_eq!(shell_quote("My Files"), "'My Files'");
        assert_eq!(shell_quote("a$(rm)*c"), "'a$(rm)*c'");
    }

    #[test]
    fn shell_quote_escapes_inner_single_quotes() {
        // 内部单引号：闭合引号 + 转义单引号 + 重新开引号，拼回 it's
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn read_file_uses_dd_for_android_compatibility() {
        let command = read_file_command("/sdcard/My Files/notes.txt", 2 * 1024 * 1024);
        assert_eq!(
            command,
            "test -f '/sdcard/My Files/notes.txt' || exit 1; dd if='/sdcard/My Files/notes.txt' bs=4096 count=513 2>/dev/null"
        );
    }

    #[test]
    fn returns_none_for_unrecognized_output() {
        assert_eq!(parse_adb_version(""), None);
        assert_eq!(parse_adb_version("unexpected format\n"), None);
    }

    #[test]
    fn uninstall_success_output_should_pass() {
        // 真机实测：成功时 stdout 为 Success，stderr 为空
        assert!(is_uninstall_success("Success\n", ""));
    }

    #[test]
    fn uninstall_exception_on_stderr_should_fail() {
        // 真机实测（Android 7.1，包不存在）：stderr 打 Java 异常栈、退出码 0、stdout 为空
        let stderr = "Exception occurred while dumping:\njava.lang.IllegalArgumentException: Unknown package: x\n";
        assert!(!is_uninstall_success("", stderr));
    }

    #[test]
    fn uninstall_failure_on_stdout_should_fail() {
        // 部分版本的 pm 把 Failure 打在 stdout（同样退出码 0）
        assert!(!is_uninstall_success(
            "Failure [DELETE_FAILED_INTERNAL_ERROR]\n",
            ""
        ));
    }

    /// 真机回归：不存在的包卸载失败时，uninstall 必须返回 Err（而非误报成功）。
    /// 仅验证失败判定路径，不会卸载任何真实应用。手动运行：
    /// `cargo test uninstall_failure_on_real_device -- --ignored`
    #[test]
    #[ignore = "需要连接一台 adb 设备"]
    fn uninstall_failure_on_real_device() {
        assert!(super::uninstall(None, "com.nonexistent.adbx.regression").is_err());
    }

    #[test]
    fn parses_du_lines_with_tab_paths() {
        // 真机 toybox du -s 多路径输出（\r\n 换行、路径可含中文），行序与入参无关
        let paths: Vec<String> = ["/sdcard/DCIM", "/sdcard/Download/微信.apk", "/gone"]
            .map(String::from)
            .into_iter()
            .collect();
        let out = "265192\t/sdcard/Download/微信.apk\r\n4\t/sdcard/DCIM\r\n";
        assert_eq!(
            parse_du_lines(out, &paths),
            vec![4 * 1024, 265192 * 1024, 0]
        );
    }

    #[test]
    fn parse_du_lines_skips_error_and_junk_lines() {
        // du 的报错行没有 \t、纯垃圾行数字不合法：跳过，不影响其余行回填
        let paths: Vec<String> = ["/a", "/b"].map(String::from).into_iter().collect();
        let out = "du: /gone: No such file or directory\r\n7\t/a\r\njunk\r\n8\t/b\r\n";
        assert_eq!(parse_du_lines(out, &paths), vec![7 * 1024, 8 * 1024]);
    }

    #[test]
    fn tree_size_sums_nested_files() {
        let dir = std::env::temp_dir().join(format!("adbx_tree_size_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 300]).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 700]).unwrap();
        assert_eq!(tree_size(&dir), 1000);
        assert_eq!(tree_size(&dir.join("sub/b.bin")), 700);
        assert_eq!(tree_size(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机回归：du_totals 查大小、pull_with_progress 全程进度单调不减且收尾 100。
    /// 设备端生成 10MB 临时文件拉回本机，结束自清理。手动运行：
    /// `cargo test pull_progress_on_real_device -- --ignored`
    #[test]
    #[ignore = "需要连接一台 adb 设备"]
    fn pull_progress_on_real_device() {
        let remote = "/data/local/tmp/adbx_pct_test.bin";
        super::run_adb(
            None,
            &[
                "shell",
                "dd",
                "if=/dev/zero",
                &format!("of={remote}"),
                "bs=1048576",
                "count=10",
            ],
        )
        .unwrap();

        let sizes = du_totals(None, &[remote.to_owned()]);
        // du 按磁盘块统计，允许小幅偏差
        assert!(
            sizes[0].abs_diff(10 * 1024 * 1024) < 64 * 1024,
            "du 结果 {sizes:?}"
        );

        let out_dir = std::env::temp_dir().join(format!("adbx_pct_out_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).unwrap();

        let mut percents = Vec::new();
        let watch = out_dir.join("adbx_pct_test.bin");
        let done = pull_with_progress(
            None,
            remote,
            &out_dir,
            sizes[0],
            || false,
            |p| percents.push(p),
        )
        .unwrap();
        assert!(done, "未取消时应报告完成");
        assert_eq!(percents.last(), Some(&100));
        assert!(
            percents.windows(2).all(|w| w[0] <= w[1]),
            "进度非单调：{percents:?}"
        );
        assert_eq!(std::fs::metadata(&watch).unwrap().len(), 10 * 1024 * 1024);

        super::run_adb(None, &["shell", "rm", remote]).unwrap();
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    /// 真机回归：拉取中途触发中止回调，应返回 Ok(false) 且子进程被杀（本地残留半截文件）。
    /// 设备端生成 100MB 临时文件，首次进度回报后置取消标记，结束自清理。
    /// 手动运行：`cargo test pull_cancel_on_real_device -- --ignored`
    #[test]
    #[ignore = "需要连接一台 adb 设备"]
    fn pull_cancel_on_real_device() {
        let remote = "/data/local/tmp/adbx_cancel_test.bin";
        super::run_adb(
            None,
            &[
                "shell",
                "dd",
                "if=/dev/zero",
                &format!("of={remote}"),
                "bs=1048576",
                "count=100",
            ],
        )
        .unwrap();

        let out_dir = std::env::temp_dir().join(format!("adbx_cancel_out_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).unwrap();

        // 首次进度回报后置取消标记：确保 adb pull 已跑起来再杀
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let abort_flag = std::sync::Arc::clone(&flag);
        let mark_flag = std::sync::Arc::clone(&flag);
        let done = pull_with_progress(
            None,
            remote,
            &out_dir,
            100 * 1024 * 1024,
            // 进度首次回报置位后，下一轮轮询（≤150ms 后）即取消
            move || abort_flag.load(std::sync::atomic::Ordering::Relaxed),
            move |_| mark_flag.store(true, std::sync::atomic::Ordering::Relaxed),
        )
        .unwrap();
        assert!(!done, "触发中止回调后应返回 Ok(false)");

        super::run_adb(None, &["shell", "rm", remote]).unwrap();
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    /// 真机回归：mkdir 创建含空格/中文名目录成功、可被 ls 列出、重复创建报错
    /// （验证退出码与 stderr 双重校验）；单层语义（父目录不存在时不递归）。
    #[test]
    #[ignore = "需要连接一台 adb 设备"]
    fn mkdir_on_real_device() {
        let base = "/data/local/tmp/adbx_mkdir_test";
        let _ = super::run_adb(None, &["shell", "rm", "-rf", &shell_quote(base)]);
        // 父目录先建：不加 -p，单层 mkdir 不递归创建
        super::shell_mkdir(None, base).unwrap();
        let dir = format!("{base}/新建 目录");
        super::shell_mkdir(None, &dir).unwrap();
        assert!(super::shell_list_dir(None, base).unwrap().contains("新建 目录"));
        // 无 -p 的单层 mkdir：已存在必须报错而非静默成功
        assert!(super::shell_mkdir(None, &dir).is_err());
        super::run_adb(None, &["shell", "rm", "-rf", &shell_quote(base)]).unwrap();
    }
}
