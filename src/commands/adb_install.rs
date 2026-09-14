//! `adbx adb-install` 子命令：在主机上安装 Android SDK Platform-Tools。
//!
//! 该命令只处理本机文件和网络，不访问 Android 设备。adb 缺失时下载官方
//! Platform-Tools 压缩包，校验其中的 adb 后安装到用户目录，并把目录加入用户 PATH。

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use zip::ZipArchive;

use crate::adb;

const DOWNLOAD_CHUNK_SIZE: usize = 64 * 1024;
const NON_INTERACTIVE_REPORT_BYTES: u64 = 1024 * 1024;
const INTERACTIVE_REPORT_INTERVAL_MS: u64 = 100;
#[cfg(unix)]
const PATH_MARKER: &str = "# adbx managed platform-tools";

/// 安装或检查主机上的 adb。
///
/// PATH 中已有可运行的 adb 时只输出其版本和路径；没有 adb 时下载官方
/// Platform-Tools 并写入用户目录。安装失败会返回 Err，部分完成状态会在错误中说明。
pub fn run() -> Result<()> {
    if let Some(path) = adb::find_adb_on_path() {
        let version = adb::adb_version_at(&path).with_context(|| {
            format!(
                "PATH 中已有 adb，但无法运行：{}，未执行自动安装",
                path.display()
            )
        })?;
        println!("✓ 已有 adb，路径：{}，版本：{}", path.display(), version);
        return Ok(());
    }

    let tools_dir = managed_tools_dir()?;
    let adb_path = tools_dir.join(adb_file_name());
    if adb_path.is_file() {
        let version = adb::adb_version_at(&adb_path).with_context(|| {
            format!(
                "用户目录已有 adb，但无法运行：{}，未覆盖现有文件",
                adb_path.display()
            )
        })?;
        ensure_user_path(&tools_dir)?;
        println!(
            "✓ 已有 adb，路径：{}，版本：{}",
            adb_path.display(),
            version
        );
        print_path_reload_hint();
        return Ok(());
    }

    if tools_dir.exists() {
        anyhow::bail!(
            "adb 安装目录已存在但缺少 {}：{}；未删除或覆盖现有目录",
            adb_file_name(),
            tools_dir.display()
        );
    }

    let url = platform_tools_url()?;
    let parent = tools_dir
        .parent()
        .context("无法确定 adb 安装目录的父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("无法创建 adb 安装目录：{}", parent.display()))?;

    let temporary = tempfile::Builder::new()
        .prefix(".adbx-platform-tools-")
        .tempdir_in(parent)
        .context("无法创建 Platform-Tools 临时目录")?;
    let archive_path = temporary.path().join("platform-tools.zip");
    download(url, &archive_path)?;

    let extracted_root = temporary.path().join("extracted");
    fs::create_dir(&extracted_root).context("无法创建 Platform-Tools 解压目录")?;
    extract_archive(&archive_path, &extracted_root)?;

    let extracted_tools_dir = extracted_root.join("platform-tools");
    let extracted_adb_path = extracted_tools_dir.join(adb_file_name());
    if !extracted_tools_dir.is_dir() || !extracted_adb_path.is_file() {
        anyhow::bail!("Platform-Tools 压缩包缺少 {}", extracted_adb_path.display());
    }

    #[cfg(unix)]
    set_executable(&extracted_adb_path)?;

    let version =
        adb::adb_version_at(&extracted_adb_path).context("下载的 adb 无法运行，已取消安装")?;

    // 重新检查目标，避免命令运行期间另一个安装进程先写入目标目录。
    if tools_dir.exists() {
        anyhow::bail!(
            "adb 安装目录已被其他进程创建：{}；未覆盖现有目录",
            tools_dir.display()
        );
    }
    fs::rename(&extracted_tools_dir, &tools_dir)
        .with_context(|| format!("无法将 Platform-Tools 安装到 {}", tools_dir.display()))?;

    if let Err(err) = ensure_user_path(&tools_dir) {
        anyhow::bail!(
            "adb 已安装到 {}，但写入用户 PATH 失败：{}",
            tools_dir.display(),
            err
        );
    }

    println!("✓ adb 安装成功，版本：{version}");
    println!("  路径：{}", tools_dir.join(adb_file_name()).display());
    println!("  已加入用户 PATH");
    print_path_reload_hint();
    Ok(())
}

/// 根据当前编译目标选择 Google 官方 Platform-Tools 下载地址。
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn platform_tools_url() -> Result<&'static str> {
    Ok("https://dl.google.com/android/repository/platform-tools-latest-linux.zip")
}

/// 根据当前编译目标选择 Google 官方 Platform-Tools 下载地址。
#[cfg(any(
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn platform_tools_url() -> Result<&'static str> {
    Ok("https://dl.google.com/android/repository/platform-tools-latest-darwin.zip")
}

/// 根据当前编译目标选择 Google 官方 Platform-Tools 下载地址。
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn platform_tools_url() -> Result<&'static str> {
    Ok("https://dl.google.com/android/repository/platform-tools-latest-windows.zip")
}

/// 在暂未覆盖的平台上返回清晰错误。
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
fn platform_tools_url() -> Result<&'static str> {
    anyhow::bail!(
        "当前平台 {} / {} 暂不支持自动安装 adb",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// 返回当前平台的 adb 文件名。
fn adb_file_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}

/// 返回 adbx 管理的 Platform-Tools 用户目录。
fn managed_tools_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .context("无法确定 Windows 用户目录：LOCALAPPDATA 未设置")?;
        return Ok(PathBuf::from(local_app_data)
            .join("adbx")
            .join("platform-tools"));
    }

    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").context("无法确定用户目录：HOME 未设置")?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("adbx")
            .join("platform-tools"))
    }
}

/// 从官方 ZIP 下载文件，并按终端类型显示下载进度。
fn download(url: &str, destination: &Path) -> Result<()> {
    println!("正在下载 Android SDK Platform-Tools...");
    let response = ureq::get(url)
        .set("User-Agent", concat!("adbx/", env!("CARGO_PKG_VERSION")))
        .call()
        .with_context(|| format!("无法下载 Platform-Tools：{url}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);

    let mut reader = response.into_reader();
    let mut file = File::create(destination)
        .with_context(|| format!("无法创建下载文件：{}", destination.display()))?;
    let interactive = io::stdout().is_terminal();
    let mut last_reported = 0;
    let mut last_rendered_at = Instant::now();
    let mut downloaded = 0;
    let mut buffer = [0u8; DOWNLOAD_CHUNK_SIZE];

    render_progress(
        0,
        total,
        interactive,
        &mut last_reported,
        &mut last_rendered_at,
        true,
    )?;
    loop {
        let read = reader
            .read(&mut buffer)
            .context("读取 Platform-Tools 下载内容失败")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .context("写入 Platform-Tools 下载文件失败")?;
        downloaded += read as u64;
        render_progress(
            downloaded,
            total,
            interactive,
            &mut last_reported,
            &mut last_rendered_at,
            false,
        )?;
    }
    file.flush().context("刷新 Platform-Tools 下载文件失败")?;
    if downloaded != last_reported {
        render_progress(
            downloaded,
            total,
            interactive,
            &mut last_reported,
            &mut last_rendered_at,
            true,
        )?;
    }
    if interactive {
        println!();
    }
    Ok(())
}

/// 输出一次下载进度；交互终端单行刷新，其他输出环境按固定字节间隔换行。
fn render_progress(
    downloaded: u64,
    total: Option<u64>,
    interactive: bool,
    last_reported: &mut u64,
    last_rendered_at: &mut Instant,
    force: bool,
) -> Result<()> {
    if !force {
        let bytes_due = downloaded.saturating_sub(*last_reported) >= NON_INTERACTIVE_REPORT_BYTES;
        let time_due = interactive
            && last_rendered_at.elapsed() >= Duration::from_millis(INTERACTIVE_REPORT_INTERVAL_MS);
        if !(bytes_due || time_due) {
            return Ok(());
        }
    }

    let line = format_progress_line(downloaded, total);
    let mut stdout = io::stdout().lock();
    if interactive {
        write!(stdout, "\r{line}")?;
        stdout.flush()?;
    } else {
        writeln!(stdout, "{line}")?;
    }
    *last_reported = downloaded;
    *last_rendered_at = Instant::now();
    Ok(())
}

/// 格式化下载进度文本；服务端未提供总大小时只显示已下载大小。
fn format_progress_line(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => {
            let percent = downloaded as f64 / total as f64 * 100.0;
            format!(
                "下载进度：{} / {} ({percent:.1}%)",
                format_bytes(downloaded),
                format_bytes(total)
            )
        }
        _ => format!("下载进度：已下载 {}", format_bytes(downloaded)),
    }
}

/// 将字节数格式化为便于终端阅读的二进制单位。
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 安全解压 ZIP，拒绝路径穿越和符号链接条目。
fn extract_archive(archive_path: &Path, destination: &Path) -> Result<()> {
    let archive_file = File::open(archive_path)
        .with_context(|| format!("无法打开 Platform-Tools 压缩包：{}", archive_path.display()))?;
    let mut archive = ZipArchive::new(archive_file).context("Platform-Tools 压缩包格式无效")?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("无法读取 Platform-Tools 压缩包条目 {index}"))?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            anyhow::bail!(
                "Platform-Tools 压缩包包含不支持的符号链接：{}",
                entry.name()
            );
        }

        let relative_path = safe_archive_entry_path(entry.name())?;
        let output_path = destination.join(&relative_path);
        if entry.is_dir() {
            fs::create_dir_all(&output_path)
                .with_context(|| format!("无法创建解压目录：{}", output_path.display()))?;
            continue;
        }

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建解压目录：{}", parent.display()))?;
        }
        let mut output_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .with_context(|| format!("无法创建解压文件：{}", output_path.display()))?;
        io::copy(&mut entry, &mut output_file)
            .with_context(|| format!("无法写入解压文件：{}", output_path.display()))?;
    }
    Ok(())
}

/// 将 ZIP 条目名称转换为安全的相对路径。
fn safe_archive_entry_path(name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if name.is_empty() || path.is_absolute() {
        anyhow::bail!("Platform-Tools 压缩包包含非法路径：{name:?}");
    }
    for component in path.components() {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir | Component::CurDir
        ) {
            anyhow::bail!("Platform-Tools 压缩包包含非法路径：{name:?}");
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
/// 为解压出的 adb 补充可执行权限，避免 ZIP 权限信息在解压过程中丢失。
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("无法设置 adb 可执行权限：{}", path.display()))
}

/// 确保 Platform-Tools 目录存在于当前用户的 PATH 中。
#[cfg(unix)]
fn ensure_user_path(directory: &Path) -> Result<()> {
    if path_contains_entry(std::env::var_os("PATH").as_deref(), directory) {
        return Ok(());
    }

    let directory_text = directory
        .to_str()
        .context("adb 安装路径不是有效的 UTF-8，无法写入 shell PATH")?;
    let (startup_files, shell) = match shell_startup_files() {
        Ok(config) => config,
        Err(err) => {
            let quoted_directory = shell_single_quote(directory_text)?;
            anyhow::bail!(
                "{err}；请手动将目录 {} 加入 PATH；POSIX shell 可执行：export PATH={quoted_directory}:\"$PATH\"",
                directory.display()
            );
        }
    };
    let path_line = shell_path_line(shell, directory_text)?;

    for startup_file in startup_files {
        let existing = match fs::read_to_string(&startup_file) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("无法读取 shell 配置文件：{}", startup_file.display())
                });
            }
        };

        // 只认完整的有效配置行。注释、残留标记或 platform-tools-old 等相似路径
        // 都不能证明 PATH 已经真的加入了当前安装目录。
        if has_valid_shell_path_entry(&existing, &path_line) {
            continue;
        }

        if let Some(parent) = startup_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 shell 配置目录：{}", parent.display()))?;
        }
        let prefix = if existing.is_empty() {
            String::new()
        } else if existing.ends_with('\n') {
            String::from("\n")
        } else {
            String::from("\n\n")
        };
        let addition = format!("{prefix}{PATH_MARKER}\n{path_line}\n");

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&startup_file)
            .with_context(|| format!("无法打开 shell 配置文件：{}", startup_file.display()))?;
        file.write_all(addition.as_bytes())
            .with_context(|| format!("无法写入 shell 配置文件：{}", startup_file.display()))?;
    }
    Ok(())
}

/// 确保 Platform-Tools 目录存在于当前用户的 Windows PATH 中。
#[cfg(windows)]
fn ensure_user_path(directory: &Path) -> Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, REG_EXPAND_SZ, REG_SZ};
    use winreg::{RegKey, RegValue};

    let directory_text = directory
        .to_str()
        .context("adb 安装路径不是有效的 UTF-8，无法写入 Windows PATH")?;
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (environment, _) = current_user
        .create_subkey("Environment")
        .context("无法打开 Windows 用户环境变量")?;
    let current_value = match environment.get_raw_value("Path") {
        Ok(value) => Some(value),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).context("无法读取 Windows 用户 PATH"),
    };
    let existing = current_value
        .as_ref()
        .map(|value| decode_registry_string(&value.bytes))
        .unwrap_or_default();
    if path_contains_entry(Some(OsStr::new(&existing)), directory) {
        // 当前进程可能继承了旧环境；即使注册表已有条目，也重新广播一次，
        // 让用户从当前终端启动的后续进程有机会获得最新用户 PATH。
        broadcast_environment_change().context("Windows PATH 已存在，但广播环境变更失败")?;
        return Ok(());
    }

    let new_path = if existing.trim().is_empty() {
        directory_text.to_owned()
    } else {
        format!("{directory_text};{existing}")
    };
    let value_type = current_value
        .as_ref()
        .map(|value| value.vtype)
        .filter(|value| *value == REG_SZ || *value == REG_EXPAND_SZ)
        .unwrap_or(REG_EXPAND_SZ);
    let bytes: Vec<u8> = new_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    environment
        .set_raw_value(
            "Path",
            &RegValue {
                bytes,
                vtype: value_type,
            },
        )
        .context("无法写入 Windows 用户 PATH")?;
    broadcast_environment_change().context("Windows PATH 已写入，但广播环境变更失败")?;
    Ok(())
}

#[cfg(windows)]
/// 广播用户环境变量已变化，让 Explorer 等进程刷新后续创建进程的环境。
fn broadcast_environment_change() -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const HWND_BROADCAST: *mut c_void = 0xffffusize as *mut c_void;
    const WM_SETTINGCHANGE: u32 = 0x001a;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        #[link_name = "SendMessageTimeoutW"]
        fn send_message_timeout_w(
            window: *mut c_void,
            message: u32,
            w_param: usize,
            l_param: isize,
            flags: u32,
            timeout: u32,
            result: *mut usize,
        ) -> isize;
    }

    let environment: Vec<u16> = OsStr::new("Environment")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut result = 0;

    // SAFETY: HWND_BROADCAST、消息常量和 UTF-16 缓冲区均为本调用准备，缓冲区在
    // API 返回前保持有效；SendMessageTimeoutW 不会保存 l_param 指针。
    let sent = unsafe {
        send_message_timeout_w(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5_000,
            &mut result,
        )
    };
    if sent == 0 {
        anyhow::bail!("无法广播 Windows 环境变更通知");
    }
    Ok(())
}

/// 判断 PATH 字符串中是否已有指定目录，Windows 下忽略大小写和尾部分隔符。
fn path_contains_entry(path: Option<&OsStr>, target: &Path) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|entry| same_path(&entry, target))
}

/// 比较两个 PATH 条目。
fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        return left
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(right.to_string_lossy().trim_end_matches(['\\', '/']));
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// 返回当前用户 shell 应加载的 PATH 配置文件。
#[cfg(unix)]
fn shell_startup_files() -> Result<(Vec<PathBuf>, &'static str)> {
    let home = std::env::var_os("HOME").context("无法确定用户目录：HOME 未设置")?;
    let shell = std::env::var_os("SHELL").unwrap_or_default();
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    shell_startup_files_for(
        &shell,
        Path::new(&home),
        zdotdir.as_deref(),
        xdg_config_home.as_deref(),
    )
}

#[cfg(unix)]
/// 根据 shell 启动规则选择会被加载的配置文件。
fn shell_startup_files_for(
    shell: &OsStr,
    home: &Path,
    zdotdir: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> Result<(Vec<PathBuf>, &'static str)> {
    match shell_kind(shell) {
        Some("bash") => {
            // Bash login shell 只读取三者中第一个存在的文件，非 login shell 读取
            // .bashrc；两类都写入，避免终端启动方式改变后 adb 再次失效。
            let login_file = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .map(|name| home.join(name))
                .find(|path| path.is_file())
                .unwrap_or_else(|| home.join(".bash_profile"));
            let bashrc = home.join(".bashrc");
            let mut files = vec![login_file];
            if !files.contains(&bashrc) {
                files.push(bashrc);
            }
            Ok((files, "bash"))
        }
        Some("zsh") => {
            let config_dir = zdotdir
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(home);
            Ok((vec![config_dir.join(".zshrc")], "zsh"))
        }
        Some("fish") => {
            let config_home = xdg_config_home
                .filter(|path| !path.as_os_str().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            Ok((vec![config_home.join("fish").join("config.fish")], "fish"))
        }
        _ => anyhow::bail!(
            "当前 shell {:?} 暂不支持自动配置 PATH",
            shell.to_string_lossy()
        ),
    }
}

#[cfg(unix)]
/// 识别可以自动写入 PATH 的 shell。
fn shell_kind(shell: &OsStr) -> Option<&'static str> {
    match Path::new(shell).file_name().and_then(OsStr::to_str) {
        Some("bash") => Some("bash"),
        Some("zsh") => Some("zsh"),
        Some("fish") => Some("fish"),
        _ => None,
    }
}

#[cfg(unix)]
/// 生成对应 shell 的完整 PATH 配置行。
fn shell_path_line(shell: &str, directory: &str) -> Result<String> {
    match shell {
        "bash" | "zsh" => Ok(format!(
            "export PATH={}:\"$PATH\"",
            shell_single_quote(directory)?
        )),
        "fish" => Ok(format!(
            "set -gx PATH {} $PATH",
            fish_single_quote(directory)?
        )),
        _ => anyhow::bail!("不支持为 shell {shell:?} 生成 PATH 配置"),
    }
}

#[cfg(unix)]
/// 判断配置文件中是否已经存在本工具生成的完整 PATH 命令。
fn has_valid_shell_path_entry(content: &str, path_line: &str) -> bool {
    content.lines().any(|line| line.trim() == path_line)
}

#[cfg(unix)]
/// 为 fish 配置生成单引号字符串。
fn fish_single_quote(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|character| character == '\0' || character == '\n' || character == '\r')
    {
        anyhow::bail!("PATH 包含 shell 不支持的控制字符");
    }
    Ok(format!(
        "'{}'",
        value.replace('\\', r"\\").replace('\'', r"\'")
    ))
}

#[cfg(unix)]
/// 为 POSIX shell 配置生成安全的单引号字符串。
fn shell_single_quote(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|character| character == '\0' || character == '\n' || character == '\r')
    {
        anyhow::bail!("PATH 包含 shell 不支持的控制字符");
    }
    Ok(format!("'{}'", value.replace('\'', r"'\''")))
}

/// 输出 PATH 生效提示。
#[cfg(unix)]
fn print_path_reload_hint() {
    if let Ok((paths, _)) = shell_startup_files() {
        if paths.len() == 1 {
            println!(
                "  请执行：source \"{}\"，或重新打开终端使 PATH 生效",
                paths[0].display()
            );
        } else {
            println!("  已覆盖 Bash 登录和非登录 shell 配置：");
            for path in &paths {
                println!("    {}", path.display());
            }
            println!("  请重新打开终端使 PATH 生效");
        }
    } else {
        println!("  请重新打开终端使 PATH 生效");
    }
}

/// 输出 PATH 生效提示。
#[cfg(windows)]
fn print_path_reload_hint() {
    println!("  已广播 Windows 环境变更；请完全退出并重新打开终端宿主或 IDE，使 PATH 生效");
}

#[cfg(windows)]
/// 解码 Windows 注册表中的 UTF-16 字符串值。
fn decode_registry_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, format_progress_line, path_contains_entry, safe_archive_entry_path};

    #[cfg(unix)]
    use super::{
        PATH_MARKER, has_valid_shell_path_entry, shell_kind, shell_path_line, shell_single_quote,
        shell_startup_files_for,
    };

    #[test]
    fn formats_download_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn formats_progress_with_and_without_total() {
        assert_eq!(
            format_progress_line(512 * 1024, Some(1024 * 1024)),
            "下载进度：512.0 KiB / 1.0 MiB (50.0%)"
        );
        assert_eq!(
            format_progress_line(512 * 1024, None),
            "下载进度：已下载 512.0 KiB"
        );
    }

    #[test]
    fn rejects_unsafe_archive_paths() {
        for path in ["", "../adb", "/tmp/adb", "platform-tools/../adb"] {
            assert!(
                safe_archive_entry_path(path).is_err(),
                "应拒绝路径 {path:?}"
            );
        }
    }

    #[test]
    fn accepts_platform_tools_archive_path() {
        assert_eq!(
            safe_archive_entry_path("platform-tools/adb").unwrap(),
            std::path::PathBuf::from("platform-tools/adb")
        );
    }

    #[cfg(unix)]
    #[test]
    fn detects_supported_shells() {
        use std::ffi::OsStr;

        assert_eq!(shell_kind(OsStr::new("/bin/zsh")), Some("zsh"));
        assert_eq!(shell_kind(OsStr::new("/bin/bash")), Some("bash"));
        assert_eq!(shell_kind(OsStr::new("/usr/bin/fish")), Some("fish"));
        assert_eq!(shell_kind(OsStr::new("/bin/nu")), None);
    }

    #[cfg(unix)]
    #[test]
    fn selects_shell_files_loaded_by_each_supported_shell() {
        use std::ffi::OsStr;
        use std::fs;

        let home = tempfile::tempdir().unwrap();
        let zsh_dotdir = home.path().join("zsh");
        let xdg_config_home = home.path().join("xdg");
        let (zsh_files, _) =
            shell_startup_files_for(OsStr::new("/bin/zsh"), home.path(), Some(&zsh_dotdir), None)
                .unwrap();
        assert_eq!(zsh_files, vec![zsh_dotdir.join(".zshrc")]);

        let (fish_files, _) = shell_startup_files_for(
            OsStr::new("/usr/bin/fish"),
            home.path(),
            None,
            Some(&xdg_config_home),
        )
        .unwrap();
        assert_eq!(
            fish_files,
            vec![xdg_config_home.join("fish").join("config.fish")]
        );

        fs::write(home.path().join(".bash_login"), "").unwrap();
        let (bash_files, _) =
            shell_startup_files_for(OsStr::new("/bin/bash"), home.path(), None, None).unwrap();
        assert_eq!(
            bash_files,
            vec![home.path().join(".bash_login"), home.path().join(".bashrc")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_accepts_complete_path_configuration_lines() {
        let path_line = shell_path_line("bash", "/tmp/platform-tools").unwrap();
        assert!(has_valid_shell_path_entry(
            &format!("{PATH_MARKER}\n{path_line}\n"),
            &path_line
        ));
        assert!(!has_valid_shell_path_entry(
            &format!("{PATH_MARKER}\n"),
            &path_line
        ));
        assert!(!has_valid_shell_path_entry(
            &format!("# {path_line}\n"),
            &path_line
        ));
        assert!(!has_valid_shell_path_entry(
            "export PATH='/tmp/platform-tools-old':\"$PATH\"\n",
            &path_line
        ));
    }

    #[test]
    fn detects_complete_path_entries_without_prefix_collisions() {
        let target = std::path::PathBuf::from("/tmp/platform-tools");
        let joined =
            std::env::join_paths([std::path::PathBuf::from("/tmp/other"), target.clone()]).unwrap();
        assert!(path_contains_entry(Some(&joined), &target));

        let similar =
            std::env::join_paths([std::path::PathBuf::from("/tmp/platform-tools-old")]).unwrap();
        assert!(!path_contains_entry(Some(&similar), &target));
    }

    #[cfg(unix)]
    #[test]
    fn quotes_shell_path_without_expanding_content() {
        assert_eq!(
            shell_single_quote("/tmp/a $HOME/it's").unwrap(),
            "'/tmp/a $HOME/it'\\''s'"
        );
    }
}
