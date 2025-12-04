//! PTY wrapper server library for terminal introspection.

mod editor;
mod input;
mod scrollback;

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener as StdUnixListener;

use nix::libc;
use nix::pty::{self, OpenptyResult, Winsize};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use nix::unistd::{self, ForkResult, Pid};
use parking_lot::RwLock;
use tap_protocol::{Request, Response};
use scrollback::ScrollbackBuffer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

static SCROLLBACK: RwLock<ScrollbackBuffer> = RwLock::new(ScrollbackBuffer::new());
static MASTER_FD: std::sync::OnceLock<i32> = std::sync::OnceLock::new();

/// Configuration for starting a server session.
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    /// Command to run (defaults to $SHELL if empty).
    pub command: Vec<String>,
    /// Custom session ID (auto-generated human-readable ID if None).
    pub session_id: Option<String>,
}

fn setup_terminal(fd: &OwnedFd) -> nix::Result<Termios> {
    let orig = termios::tcgetattr(fd)?;
    let mut raw = orig.clone();
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(fd, SetArg::TCSANOW, &raw)?;
    Ok(orig)
}

fn restore_terminal(fd: &OwnedFd, termios: &Termios) {
    let _ = termios::tcsetattr(fd, SetArg::TCSANOW, termios);
}

fn get_window_size() -> Winsize {
    let mut ws: Winsize = unsafe { std::mem::zeroed() };
    unsafe {
        libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws);
    }
    ws
}

fn set_window_size(fd: i32, ws: &Winsize) {
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, ws);
    }
}

extern "C" fn handle_sigwinch(_: libc::c_int) {
    if let Some(&master_fd) = MASTER_FD.get() {
        let ws = get_window_size();
        set_window_size(master_fd, &ws);
    }
}

async fn handle_client(mut stream: UnixStream, output_rx: broadcast::Receiver<Vec<u8>>) {
    let mut buf = bytes::BytesMut::with_capacity(4096);
    let mut output_rx = output_rx;

    loop {
        buf.clear();

        tokio::select! {
            result = stream.read_buf(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        let request: Request = match serde_json::from_slice(&buf) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!("Invalid request: {e}");
                                continue;
                            }
                        };

                        let response = match request {
                            Request::GetScrollback { lines } => {
                                let scrollback = SCROLLBACK.read();
                                let content = scrollback.get_lines(lines);
                                Response::Scrollback { content }
                            }
                            Request::GetCursor => {
                                let scrollback = SCROLLBACK.read();
                                let (row, col) = scrollback.cursor_position();
                                Response::Cursor { row, col }
                            }
                            Request::Inject { data } => {
                                if let Some(&master_fd) = MASTER_FD.get() {
                                    let fd = unsafe { OwnedFd::from_raw_fd(master_fd) };
                                    let result = unistd::write(&fd, data.as_bytes());
                                    std::mem::forget(fd);
                                    match result {
                                        Ok(_) => Response::Ok,
                                        Err(e) => Response::Error { message: e.to_string() },
                                    }
                                } else {
                                    Response::Error { message: "No master FD".to_string() }
                                }
                            }
                            Request::GetSize => {
                                let ws = get_window_size();
                                Response::Size {
                                    rows: ws.ws_row,
                                    cols: ws.ws_col,
                                }
                            }
                            Request::Subscribe => {
                                Response::Subscribed
                            }
                        };

                        let response_bytes = serde_json::to_vec(&response).unwrap();
                        if stream.write_all(&response_bytes).await.is_err() {
                            break;
                        }
                        if stream.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Read error: {e}");
                        break;
                    }
                }
            }
            result = output_rx.recv() => {
                match result {
                    Ok(data) => {
                        let response = Response::Output { data };
                        let response_bytes = serde_json::to_vec(&response).unwrap();
                        if stream.write_all(&response_bytes).await.is_err() {
                            break;
                        }
                        if stream.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn run_socket_server(
    socket_path: std::path::PathBuf,
    output_tx: broadcast::Sender<Vec<u8>>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);
    let std_listener = StdUnixListener::bind(&socket_path)?;
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;

    info!("Listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                debug!("Client connected");
                let output_rx = output_tx.subscribe();
                tokio::spawn(handle_client(stream, output_rx));
            }
            Err(e) => {
                error!("Accept error: {e}");
            }
        }
    }
}

fn wait_for_child(child: Pid) -> i32 {
    use nix::sys::wait::{WaitStatus, waitpid};
    loop {
        match waitpid(child, None) {
            Ok(WaitStatus::Exited(_, code)) => return code,
            Ok(WaitStatus::Signaled(_, sig, _)) => return 128 + sig as i32,
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return 1,
        }
    }
}

/// Run the PTY server with the given configuration.
/// Returns the exit code of the child process.
pub async fn run(config: ServerConfig) -> eyre::Result<i32> {
    // Load tap config for keybinds
    let tap_config = tap_config::load()?;
    let mut input_processor = input::InputProcessor::new(&tap_config)?;
    let editor_cmd = tap_config::get_editor(&tap_config);

    let session_id = config
        .session_id
        .unwrap_or_else(|| human_id::gen_id(3));

    let socket_dir = tap_protocol::socket_dir();
    std::fs::create_dir_all(&socket_dir)?;
    let socket_path = tap_protocol::socket_path(&session_id);

    let command = if config.command.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())]
    } else {
        config.command.clone()
    };

    // Write session info
    let sessions_file = tap_protocol::sessions_file();
    let mut sessions: Vec<serde_json::Value> = std::fs::read_to_string(&sessions_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    sessions.push(serde_json::json!({
        "id": session_id,
        "pid": std::process::id(),
        "started": chrono::Utc::now().to_rfc3339(),
        "command": command,
    }));
    std::fs::write(
        &sessions_file,
        serde_json::to_string_pretty(&sessions).unwrap(),
    )?;

    // Open PTY using openpty
    let ws = get_window_size();
    let OpenptyResult { master, slave } =
        pty::openpty(Some(&ws), None).map_err(|e| eyre::eyre!("openpty failed: {e}"))?;

    let master_raw_fd = master.as_raw_fd();

    // Store master FD for signal handler
    MASTER_FD
        .set(master_raw_fd)
        .map_err(|_| eyre::eyre!("Failed to set MASTER_FD"))?;

    // Set up SIGWINCH handler
    unsafe {
        signal::signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch))
            .map_err(|e| eyre::eyre!("Failed to set SIGWINCH handler: {e}"))?;
    }

    // Fork child process
    let child_pid = match unsafe { unistd::fork() } {
        Ok(ForkResult::Child) => {
            drop(master);

            unistd::setsid().expect("setsid failed");

            // Set controlling terminal
            unsafe {
                libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
            }

            // Dup slave to stdin/stdout/stderr using libc directly
            let slave_raw = slave.as_raw_fd();
            unsafe {
                libc::dup2(slave_raw, libc::STDIN_FILENO);
                libc::dup2(slave_raw, libc::STDOUT_FILENO);
                libc::dup2(slave_raw, libc::STDERR_FILENO);
            }

            if slave_raw > 2 {
                drop(slave);
            }

            let c_cmd: Vec<std::ffi::CString> = command
                .iter()
                .map(|s| std::ffi::CString::new(s.as_str()).unwrap())
                .collect();

            unistd::execvp(&c_cmd[0], &c_cmd).expect("execvp failed");
            unreachable!()
        }
        Ok(ForkResult::Parent { child }) => child,
        Err(e) => {
            return Err(eyre::eyre!("Fork failed: {e}"));
        }
    };

    // Close slave in parent
    drop(slave);

    // Save terminal state and set raw mode
    let stdin_fd = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
    let orig_termios = match setup_terminal(&stdin_fd) {
        Ok(t) => Some(t),
        Err(e) => {
            debug!("Not a terminal or failed to set raw mode: {e}");
            None
        }
    };
    // Don't close stdin
    std::mem::forget(stdin_fd);

    // Set up broadcast channel for output
    let (output_tx, _) = broadcast::channel::<Vec<u8>>(1024);

    // Start server
    let server_output_tx = output_tx.clone();
    let server_socket_path = socket_path.clone();
    tokio::spawn(async move {
        if let Err(e) = run_socket_server(server_socket_path, server_output_tx).await {
            error!("Server error: {e}");
        }
    });

    println!("\x1b[2m[tap: session {session_id}]\x1b[0m");

    // Main I/O loop
    let mut master_file =
        tokio::fs::File::from_std(unsafe { std::fs::File::from_raw_fd(master.as_raw_fd()) });
    // Prevent double-close
    std::mem::forget(master);

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut master_buf = vec![0u8; 4096];
    let mut stdin_buf = vec![0u8; 4096];

    let exit_code = loop {
        tokio::select! {
            result = master_file.read(&mut master_buf) => {
                match result {
                    Ok(0) => break 0,
                    Ok(n) => {
                        let data = master_buf[..n].to_vec();

                        // Update scrollback
                        SCROLLBACK.write().push(&data);

                        // Broadcast to subscribers
                        let _ = output_tx.send(data.clone());

                        // Write to stdout
                        if stdout.write_all(&data).await.is_err() {
                            break 1;
                        }
                        let _ = stdout.flush().await;
                    }
                    Err(e) => {
                        debug!("Master read error: {e}");
                        break 0;
                    }
                }
            }
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) => break 0,
                    Ok(n) => {
                        match input_processor.process(&stdin_buf[..n]) {
                            input::InputResult::Passthrough(bytes) => {
                                if !bytes.is_empty() {
                                    let fd = unsafe { OwnedFd::from_raw_fd(master_raw_fd) };
                                    if unistd::write(&fd, &bytes).is_err() {
                                        std::mem::forget(fd);
                                        break 1;
                                    }
                                    std::mem::forget(fd);
                                }
                            }
                            input::InputResult::Action(input::KeybindAction::OpenEditor) => {
                                let scrollback_content = SCROLLBACK.read().get_lines(None);
                                if let Err(e) = editor::open_scrollback_in_editor(
                                    &scrollback_content,
                                    &editor_cmd,
                                    orig_termios.as_ref(),
                                ) {
                                    error!("Failed to open editor: {e}");
                                }
                            }
                            input::InputResult::NeedMore => {
                                // Wait for timeout or more input
                            }
                        }
                    }
                    Err(e) => {
                        debug!("Stdin read error: {e}");
                        break 0;
                    }
                }
            }
            _ = tokio::time::sleep(input_processor.escape_timeout()), if input_processor.has_pending_escape() => {
                if let input::InputResult::Passthrough(bytes) = input_processor.timeout_escape() {
                    if !bytes.is_empty() {
                        let fd = unsafe { OwnedFd::from_raw_fd(master_raw_fd) };
                        let _ = unistd::write(&fd, &bytes);
                        std::mem::forget(fd);
                    }
                }
            }
        }
    };

    // Restore terminal
    if let Some(ref termios) = orig_termios {
        let stdin_fd = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
        restore_terminal(&stdin_fd, termios);
        std::mem::forget(stdin_fd);
    }

    // Clean up socket and session entry
    let _ = std::fs::remove_file(&socket_path);

    // Remove session from sessions.json
    if let Ok(content) = std::fs::read_to_string(&sessions_file)
        && let Ok(mut sessions) = serde_json::from_str::<Vec<serde_json::Value>>(&content)
    {
        sessions.retain(|s| s.get("id").and_then(|v| v.as_str()) != Some(&session_id));
        let _ = std::fs::write(
            &sessions_file,
            serde_json::to_string_pretty(&sessions).unwrap(),
        );
    }

    // Wait for child
    let final_code = wait_for_child(child_pid);

    if final_code == 0 && exit_code == 0 {
        Ok(0)
    } else {
        Ok(final_code)
    }
}
