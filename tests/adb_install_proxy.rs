//! 使用隔离目录和本地代理检查安装入口；不下载文件或访问真实设备。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn installer(home: &std::path::Path, proxy: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adbx"));
    command
        .arg("adb-install")
        .env("HOME", home)
        .env("LOCALAPPDATA", home)
        .env("PATH", home)
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("HTTPS_PROXY", proxy)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// 模拟合法 CONNECT 响应并检查客户端确实进入 TLS，避免仅检查错误文本的假通过。
/// 超时后终止子进程，让旧版的 256 字节边界死锁也能作为测试失败返回。
fn assert_proxy_starts_tls(response: Vec<u8>, split_at: Option<usize>) {
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let mut child = installer(home.path(), &proxy).spawn().unwrap();
    let server = thread::spawn(move || -> std::io::Result<[u8; 3]> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(std::io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err),
            }
        };
        // macOS 的 accept 会继承监听 socket 的非阻塞状态。
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        socket.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte)?;
            request.push(byte[0]);
        }
        assert!(request.starts_with(b"CONNECT dl.google.com:443 HTTP/1.1\r\n"));
        if let Some(split) = split_at {
            socket.write_all(&response[..split])?;
            thread::sleep(Duration::from_millis(100));
            socket.write_all(&response[split..])?;
        } else {
            socket.write_all(&response)?;
        }
        let mut tls_header = [0; 3];
        socket.read_exact(&mut tls_header)?;
        Ok(tls_header)
    });
    let handshake = server.join().unwrap();
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let header = handshake.unwrap_or_else(|err| {
        panic!(
            "未进入 TLS：{err}；{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(&header[..2], &[0x16, 0x03], "应收到 TLS Handshake record");
}

#[test]
fn fragmented_connect_response_starts_tls() {
    assert_proxy_starts_tls(
        b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec(),
        Some(9),
    );
}

#[test]
fn exact_256_byte_connect_response_starts_tls() {
    let mut response = b"HTTP/1.1 200 Connection established\r\nX-Padding: ".to_vec();
    response.resize(252, b'x');
    response.extend_from_slice(b"\r\n\r\n");
    assert_proxy_starts_tls(response, None);
}

#[test]
fn unsupported_proxy_error_does_not_expose_credentials() {
    let home = tempfile::tempdir().unwrap();
    let output = installer(
        home.path(),
        "socks5://review-user:review-password@127.0.0.1:1080",
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("HTTPS_PROXY"));
    assert!(error.contains("不支持的代理协议"));
    assert!(!error.contains("review-user"));
    assert!(!error.contains("review-password"));
}
