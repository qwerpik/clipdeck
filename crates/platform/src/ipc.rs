//! Single-instance IPC: a local socket (Unix socket in `$XDG_RUNTIME_DIR` on Linux, a named
//! pipe on Windows). One request line in, one response out.

use std::io::{BufRead, BufReader, Read, Write};
use std::thread::JoinHandle;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name, Stream};

/// Handler: request line → response text.
pub type Handler = Box<dyn Fn(&str) -> String + Send + Sync + 'static>;

/// Errors.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Nobody is listening.
    #[error("Clipdeck is not running")]
    NotRunning,
    /// Another instance already owns the socket.
    #[error("another Clipdeck instance is running")]
    AlreadyRunning,
    /// I/O.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn name() -> std::io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let user = std::env::var("USER").unwrap_or_default();
        let file = if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
            dir.join(clipdeck_core::protocol::SOCKET_NAME)
        } else {
            dir.join(format!("clipdeck-{user}.sock"))
        };
        let file = match std::env::var_os("CLIPDECK_SOCKET") {
            Some(p) => std::path::PathBuf::from(p),
            None => file,
        };
        file.to_fs_name::<GenericFilePath>().map(|n| n.into_owned())
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        let base = std::env::var("CLIPDECK_SOCKET").unwrap_or_else(|_| {
            format!("clipdeck-{}", std::env::var("USERNAME").unwrap_or_default())
        });
        base.to_ns_name::<GenericNamespaced>()
            .map(|n| n.into_owned())
    }
}

/// Send one request to the running instance and return its response.
pub fn request(line: &str) -> Result<String, IpcError> {
    let mut conn = match Stream::connect(name()?) {
        Ok(c) => c,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(IpcError::NotRunning);
        }
        Err(e) => return Err(IpcError::Io(e)),
    };
    let mut msg = line.trim().replace('\n', " ");
    msg.push('\n');
    conn.write_all(msg.as_bytes())?;
    conn.flush()?;
    let mut out = String::new();
    conn.read_to_string(&mut out)?;
    Ok(out.trim_end().to_string())
}

/// Whether another instance answers.
pub fn is_running() -> bool {
    matches!(request("ping"), Ok(r) if r.starts_with("OK"))
}

/// Running server; dropping it does not stop the thread (it lives as long as the process).
pub struct Server {
    _thread: JoinHandle<()>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ipc::Server")
    }
}

/// Start listening. Fails with [`IpcError::AlreadyRunning`] if an instance answers.
pub fn serve(handler: Handler) -> Result<Server, IpcError> {
    if is_running() {
        return Err(IpcError::AlreadyRunning);
    }
    let n = name()?;
    let listener = match ListenerOptions::new().name(n.clone()).create_sync() {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Stale socket file from a crash: remove it and retry.
            #[cfg(unix)]
            if let Some(p) = stale_path() {
                let _ = std::fs::remove_file(p);
            }
            ListenerOptions::new().name(n).create_sync()?
        }
        Err(e) => return Err(IpcError::Io(e)),
    };
    let handler = std::sync::Arc::new(handler);
    let thread = std::thread::Builder::new()
        .name("clipdeck-ipc".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let h = handler.clone();
                // Each client gets its own short-lived thread so a stuck client cannot block others.
                let _ = std::thread::Builder::new()
                    .name("clipdeck-ipc-client".into())
                    .spawn(move || {
                        let mut reader = BufReader::new(conn);
                        let mut line = String::new();
                        if reader.by_ref().take(64 * 1024).read_line(&mut line).is_ok()
                            && !line.trim().is_empty()
                        {
                            let resp = h(line.trim());
                            let mut conn = reader.into_inner();
                            let _ = conn.write_all(resp.as_bytes());
                            let _ = conn.write_all(b"\n");
                            let _ = conn.flush();
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    });
            }
        })?;
    Ok(Server { _thread: thread })
}

#[cfg(unix)]
fn stale_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("CLIPDECK_SOCKET") {
        return Some(p.into());
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| std::path::PathBuf::from(d).join(clipdeck_core::protocol::SOCKET_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("clipdeck-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        // SAFETY: tests in this module run in one thread before any other env access.
        unsafe {
            std::env::set_var("CLIPDECK_SOCKET", dir.join("t.sock"));
        }
        #[cfg(windows)]
        unsafe {
            std::env::set_var(
                "CLIPDECK_SOCKET",
                format!("clipdeck-test-{}", std::process::id()),
            );
        }
        assert!(matches!(request("ping"), Err(IpcError::NotRunning)));
        let _s = serve(Box::new(|l| {
            if l == "ping" {
                "OK pong".into()
            } else {
                format!("ERR {l}")
            }
        }))
        .unwrap();
        assert_eq!(request("ping").unwrap(), "OK pong");
        assert_eq!(request("hello world").unwrap(), "ERR hello world");
        assert!(matches!(
            serve(Box::new(|_| String::new())),
            Err(IpcError::AlreadyRunning)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
