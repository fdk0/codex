use std::path::Path;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use anyhow::Result;
use app_test_support::app_server_json_shutdown_event;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

#[test]
fn strict_config_rejects_unknown_config_fields_for_app_server() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["app-server", "--strict-config", "--listen", "off"])
        .assert()
        .failure()
        .stderr(contains("unknown configuration field"));

    Ok(())
}

#[test]
fn app_server_emits_json_info_events() -> Result<()> {
    let codex_home = TempDir::new()?;
    let event = app_server_json_shutdown_event("codex", &["app-server"], codex_home.path())?;

    assert_eq!(
        event,
        json!({
            "level": "INFO",
            "fields": {
                "message": "processor task exited",
                "exit_reason": "last_connection_closed",
                "remaining_connection_count": 0,
                "shutdown_forced": false,
            },
            "target": "codex_app_server",
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn app_server_proxy_relays_raw_websocket_upgrade_bytes() -> Result<()> {
    use std::io::ErrorKind;
    use std::io::Read;
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::thread;

    const UPGRADE_REQUEST: &[u8] = b"GET / HTTP/1.1\r\n\
Host: localhost\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
Sec-WebSocket-Version: 13\r\n\r\n";

    let codex_home = TempDir::new()?;
    let socket_path = codex_home.path().join("app-server.sock");
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;

    let server = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(err) if err.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;

        let mut received = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !received.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..read]);
        }
        stream.write_all(&received)?;
        Ok(received)
    });

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["app-server", "proxy", "--sock"])
        .arg(&socket_path)
        .write_stdin(UPGRADE_REQUEST)
        .output()?;
    let received = server.join().expect("proxy test server should not panic")?;

    assert!(
        output.status.success(),
        "proxy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(received, UPGRADE_REQUEST);
    assert_eq!(output.stdout, UPGRADE_REQUEST);

    Ok(())
}
