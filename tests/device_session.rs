#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

fn fixture() -> TempDir {
    let dir = tempdir().unwrap();
    symlink(env!("CARGO_BIN_EXE_adbx"), dir.path().join("adbx")).unwrap();
    let adb = dir.path().join("adb");
    fs::write(
        &adb,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$ADB_TEST_LOG"
case "$*" in
    'devices -l') printf 'List of devices attached\nphone device model:Test\n' ;;
    *'shell pm list packages'*) printf 'package:com.example.app\n' ;;
    *'shell pm clear'*) printf 'Success\n' ;;
    *) exit 9 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(adb, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn adbx(dir: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_adbx"));
    cmd.env("PATH", dir.path())
        .env("ADB_TEST_LOG", dir.path().join("adb.log"))
        .env_remove("ANDROID_SERIAL");
    cmd
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explicit_empty_serial_fails_before_any_adb_call() {
    for args in [
        vec!["-s", "", "clear", "com.example.app"],
        vec!["--serial", "", "clear", "com.example.app"],
        vec!["--serial=", "clear", "com.example.app"],
        vec!["clear", "com.example.app", "-s", ""],
        vec!["clear", "com.example.app", "--serial="],
    ] {
        for env in ["", "bound-phone"] {
            let dir = fixture();
            let output = adbx(&dir)
                .args(&args)
                .env("ANDROID_SERIAL", env)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{args:?}, env={env:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("serial 不能为空"));
            assert!(!dir.path().join("adb.log").exists());
        }
    }
}

#[test]
fn serial_fallback_and_explicit_priority_preserve_device_selection() {
    for (env, explicit, expected) in [
        (None, None, "phone"),
        (Some(""), None, "phone"),
        (Some("bound-phone"), None, "bound-phone"),
        (Some("bound-phone"), Some("flag-phone"), "flag-phone"),
    ] {
        let dir = fixture();
        let mut cmd = adbx(&dir);
        if let Some(env) = env {
            cmd.env("ANDROID_SERIAL", env);
        }
        if let Some(serial) = explicit {
            cmd.args(["-s", serial]);
        }
        let output = cmd.args(["clear", "com.example.app"]).output().unwrap();
        assert_success(&output);
        let log = fs::read_to_string(dir.path().join("adb.log")).unwrap();
        let enumeration = if expected == "phone" {
            "devices -l\n"
        } else {
            ""
        };
        assert_eq!(
            log,
            format!(
                "{enumeration}-s {expected} shell pm list packages\n-s {expected} shell pm clear com.example.app\n"
            )
        );
    }
}

fn posix_shells() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["bash", "zsh"]
    } else {
        &["bash"]
    }
}

fn shell_command(dir: &TempDir, shell: &str, script: &str) -> Command {
    let init = adbx(dir).args(["init", shell]).output().unwrap();
    assert_success(&init);
    let path = dir.path().join("init.sh");
    fs::write(&path, init.stdout).unwrap();
    let mut cmd = Command::new(format!("/bin/{shell}"));
    cmd.args(["-f", "-c", script, "adbx-test"])
        .env("PATH", dir.path())
        .env("ADBX_INIT", path)
        .env("ADB_TEST_LOG", dir.path().join("adb.log"))
        .env("ANDROID_SERIAL", "original-phone");
    cmd
}

#[test]
fn posix_help_anywhere_preserves_binding_and_prints_public_help() {
    for shell in posix_shells() {
        for args in [
            vec!["use", "--help"],
            vec!["use", "-h"],
            vec!["use", "phone", "--help"],
            vec!["use", "phone", "-h"],
            vec!["unuse", "--help"],
            vec!["unuse", "-s", "phone", "--help"],
        ] {
            let dir = fixture();
            let expected = adbx(&dir)
                .args(&args)
                .env("ANDROID_SERIAL", "original-phone")
                .output()
                .unwrap();
            assert_success(&expected);
            let output = shell_command(
                &dir,
                shell,
                r#"set -u
source "$ADBX_INIT"
adbx "$@"
result=$?
printf '\nBINDING=%s\nSTATUS=%s\n' "$ANDROID_SERIAL" "$result"
"#,
            )
            .args(&args)
            .output()
            .unwrap();
            assert_success(&output);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!(
                    "{}\nBINDING=original-phone\nSTATUS=0\n",
                    String::from_utf8_lossy(&expected.stdout)
                ),
                "{shell}: {args:?}"
            );
            assert!(!dir.path().join("adb.log").exists());
        }
    }
}

#[test]
fn posix_empty_invocation_under_nounset_reaches_cli_and_continues() {
    for shell in posix_shells() {
        let dir = fixture();
        let output = shell_command(
            &dir,
            shell,
            r#"set -u
source "$ADBX_INIT"
adbx
result=$?
printf 'CONTINUED=%s\nBINDING=%s\n' "$result" "$ANDROID_SERIAL"
"#,
        )
        .output()
        .unwrap();
        assert_success(&output);
        assert_eq!(output.stdout, b"CONTINUED=2\nBINDING=original-phone\n");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
        assert!(!dir.path().join("adb.log").exists());
    }
}

#[test]
fn posix_binding_success_failure_and_option_separator_preserve_contract() {
    for shell in posix_shells() {
        let dir = fixture();
        let output = shell_command(
            &dir,
            shell,
            r#"set -u
source "$ADBX_INIT"
adbx use phone || exit 10
[ "$ANDROID_SERIAL" = phone ] || exit 11
adbx use missing && exit 12
[ "$ANDROID_SERIAL" = phone ] || exit 13
adbx use -- --help && exit 14
[ "$ANDROID_SERIAL" = phone ] || exit 15
adbx unuse || exit 16
[ "${ANDROID_SERIAL:-}" = '' ] || exit 17
"#,
        )
        .output()
        .unwrap();
        assert_success(&output);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        assert_eq!(
            fs::read_to_string(dir.path().join("adb.log")).unwrap(),
            "devices -l\ndevices -l\ndevices -l\n"
        );
    }
}

#[test]
#[ignore = "需要安装 PowerShell 7（pwsh）"]
fn powershell_help_anywhere_preserves_binding_and_prints_public_help() {
    let dir = fixture();
    let init = adbx(&dir).args(["init", "powershell"]).output().unwrap();
    assert_success(&init);
    let path = dir.path().join("init.ps1");
    fs::write(&path, init.stdout).unwrap();
    let paths = std::env::var_os("PATH").unwrap();
    let path_env = std::env::join_paths(
        std::iter::once(dir.path().to_path_buf()).chain(std::env::split_paths(&paths)),
    )
    .unwrap();
    for args in [
        vec!["use", "--help"],
        vec!["use", "phone", "--help"],
        vec!["use", "phone", "-h"],
        vec!["unuse", "--help"],
        vec!["unuse", "-s", "phone", "--help"],
    ] {
        let expected = adbx(&dir)
            .args(&args)
            .env("ANDROID_SERIAL", "original-phone")
            .output()
            .unwrap();
        assert_success(&expected);
        let output = Command::new("pwsh")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                r#"
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0
. $env:ADBX_INIT
$callArgs = $env:ADBX_TEST_ARGS -split '\|'
adbx @callArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output "BINDING=$env:ANDROID_SERIAL"
"#,
            ])
            .env("PATH", &path_env)
            .env("ADBX_INIT", &path)
            .env("ADBX_TEST_ARGS", args.join("|"))
            .env("ANDROID_SERIAL", "original-phone")
            .env("ADB_TEST_LOG", dir.path().join("adb.log"))
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            format!(
                "{}BINDING=original-phone\n",
                String::from_utf8_lossy(&expected.stdout)
            )
        );
        assert!(!dir.path().join("adb.log").exists());
    }
}
