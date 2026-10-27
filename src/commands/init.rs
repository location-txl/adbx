//! init 子命令：输出终端集成脚本，供 `eval "$(adbx init zsh)"` 等方式加载。
//!
//! 集成脚本在 Shell 里定义 `adbx` 函数：拦截 `use`/`unuse` 修改当前终端
//! 的 `ANDROID_SERIAL` 环境变量，其余调用原样交给 adbx 二进制。设备解析
//! 与校验由隐藏命令 `__resolve-device`（[`crate::commands::device`]）完成，
//! 脚本只捕获它输出的 serial，不拼接 Shell 代码执行。

use anyhow::{Result, bail};

/// zsh 与 bash 通用的包装函数：两者语法子集一致，输出同一份脚本。
const POSIX_SCRIPT: &str = r#"# adbx 终端集成：adbx use <serial> 绑定设备到当前终端，adbx unuse 解除绑定。
# 由 eval "$(adbx init zsh)" （或 bash）加载；adbx 升级后重开终端即可。
adbx() {
    case "$1" in
        use)
            case "${2:-}" in
                -h|--help) command adbx "$@"; return ;;
            esac
            shift
            local serial
            serial="$(command adbx __resolve-device "$@")" || return
            [ -n "$serial" ] || return 1
            export ANDROID_SERIAL="$serial"
            echo "✓ 当前终端已绑定 $serial"
            ;;
        unuse)
            case "${2:-}" in
                -h|--help) command adbx "$@"; return ;;
            esac
            if [ -n "${ANDROID_SERIAL:-}" ]; then
                unset ANDROID_SERIAL
                echo "✓ 已解除设备绑定"
            else
                echo "当前终端未绑定设备"
            fi
            ;;
        *)
            command adbx "$@"
            ;;
    esac
}
"#;

/// PowerShell profile 用的包装函数；通过 Get-Command 定位二进制，避免函数递归。
const POWERSHELL_SCRIPT: &str = r#"# adbx 终端集成：adbx use <serial> 绑定设备到当前终端，adbx unuse 解除绑定。
# 用 adbx init powershell | Add-Content $PROFILE 安装后重开终端。
function adbx {
    # 定位真正的 adbx 可执行文件，避免函数调用自身
    $real = Get-Command adbx -CommandType Application | Select-Object -First 1
    # 空 $args 时先取 $null，避免 StrictMode 3.0 下数组越界索引报错
    $first = if ($args.Count -gt 0) { $args[0] }
    switch -Exact ($first) {
        'use' {
            if ($args.Count -ge 2 -and ($args[1] -eq '-h' -or $args[1] -eq '--help')) {
                & $real @args
                return
            }
            $rest = @()
            if ($args.Count -gt 1) { $rest = $args[1..($args.Count - 1)] }
            $serial = (& $real __resolve-device @rest | Select-Object -First 1)
            if ($LASTEXITCODE -ne 0) { return }
            if ([string]::IsNullOrWhiteSpace($serial)) { return }
            $env:ANDROID_SERIAL = "$serial".Trim()
            Write-Host "✓ 当前终端已绑定 $env:ANDROID_SERIAL"
        }
        'unuse' {
            if ($args.Count -ge 2 -and ($args[1] -eq '-h' -or $args[1] -eq '--help')) {
                & $real @args
                return
            }
            if (Test-Path Env:\ANDROID_SERIAL) {
                Remove-Item Env:\ANDROID_SERIAL
                Write-Host "✓ 已解除设备绑定"
            } else {
                Write-Host "当前终端未绑定设备"
            }
        }
        default {
            & $real @args
        }
    }
}
"#;

/// `init` 入口：输出指定 shell 的集成脚本到 stdout。
///
/// * `shell` - shell 名称，大小写不敏感；zsh 与 bash 输出同一份脚本，
///   powershell/pwsh 输出 profile 函数；其余（含 fish）暂不支持，报错说明
pub fn run(shell: &str) -> Result<()> {
    match shell.to_ascii_lowercase().as_str() {
        "zsh" | "bash" => {
            print!("{POSIX_SCRIPT}");
            Ok(())
        }
        "powershell" | "pwsh" => {
            print!("{POWERSHELL_SCRIPT}");
            Ok(())
        }
        other => bail!("暂不支持 {other}：当前支持 zsh、bash 和 powershell"),
    }
}

#[cfg(test)]
mod tests {
    use super::{POSIX_SCRIPT, POWERSHELL_SCRIPT};

    #[test]
    fn posix_script_delegates_to_real_adbx_and_resolves_via_hidden_command() {
        // 关键行为：use 经 __resolve-device 拿 serial 再 export；unuse unset；
        // 其余命令用 command adbx 调真身，避免函数递归
        assert!(POSIX_SCRIPT.contains("command adbx __resolve-device"));
        assert!(POSIX_SCRIPT.contains("export ANDROID_SERIAL=\"$serial\""));
        assert!(POSIX_SCRIPT.contains("unset ANDROID_SERIAL"));
        assert!(POSIX_SCRIPT.contains("command adbx \"$@\""));
    }

    #[test]
    fn posix_script_passes_help_flags_through_to_real_adbx() {
        // use/unuse 的 -h/--help 要显示自身帮助而不是隐藏命令 __resolve-device 的
        assert_eq!(
            POSIX_SCRIPT
                .matches("-h|--help) command adbx \"$@\"; return")
                .count(),
            2
        );
    }

    #[test]
    fn powershell_script_sets_and_clears_environment_variable() {
        assert!(POWERSHELL_SCRIPT.contains("__resolve-device"));
        assert!(POWERSHELL_SCRIPT.contains("$env:ANDROID_SERIAL"));
        assert!(POWERSHELL_SCRIPT.contains("Remove-Item Env:\\ANDROID_SERIAL"));
        // 递归防护：函数内通过 Get-Command 定位可执行文件
        assert!(POWERSHELL_SCRIPT.contains("Get-Command adbx -CommandType Application"));
    }

    #[test]
    fn powershell_script_guards_empty_args_and_help_flags() {
        // StrictMode 3.0 下数组越界索引会报错：先取 $first 再进 switch；
        // use/unuse 的 -h/--help 直通真身，显示自身帮助
        assert!(POWERSHELL_SCRIPT.contains("$first = if ($args.Count -gt 0)"));
        assert!(POWERSHELL_SCRIPT.contains("switch -Exact ($first)"));
        assert!(POWERSHELL_SCRIPT.contains("-eq '-h' -or $args[1] -eq '--help'"));
    }
}
