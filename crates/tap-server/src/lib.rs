//! PTY wrapper server library for terminal introspection.

mod editor;
mod input;
mod scrollback;

use std::os::fd::{AsRawFd as _, FromRawFd as _};

use crossterm::execute;
use eyre::WrapErr as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const DEFAULT_SHELL: &str = "/bin/sh";
const HUMAN_ID_WORDS: usize = 3;
const BROADCAST_CHANNEL_SIZE: usize = 1024;
const IO_BUFFER_SIZE: usize = 4096;

static SCROLLBACK: parking_lot::RwLock<scrollback::ScrollbackBuffer> =
    parking_lot::RwLock::new(scrollback::ScrollbackBuffer::new());
static MASTER_FD: std::sync::OnceLock<i32> = std::sync::OnceLock::new();

/// Configuration for starting a server session.
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    /// Command to run (defaults to $SHELL if empty).
    pub command: Vec<String>,
    /// Custom session ID (auto-generated human-readable ID if None).
    pub session_id: Option<String>,
}

fn setup_terminal(fd: &std::os::fd::OwnedFd) -> nix::Result<nix::sys::termios::Termios> {
    let orig = nix::sys::termios::tcgetattr(fd)?;
    let mut raw = orig.clone();
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(fd, nix::sys::termios::SetArg::TCSANOW, &raw)?;
    Ok(orig)
}

fn restore_terminal(fd: &std::os::fd::OwnedFd, termios: &nix::sys::termios::Termios) {
    let _ = nix::sys::termios::tcsetattr(fd, nix::sys::termios::SetArg::TCSANOW, termios);
}

fn get_window_size() -> nix::pty::Winsize {
    let mut ws: nix::pty::Winsize = unsafe { std::mem::zeroed() };
    unsafe {
        nix::libc::ioctl(nix::libc::STDIN_FILENO, nix::libc::TIOCGWINSZ, &mut ws);
    }
    ws
}

fn set_window_size(fd: i32, ws: &nix::pty::Winsize) {
    unsafe {
        nix::libc::ioctl(fd, nix::libc::TIOCSWINSZ, ws);
    }
}

extern "C" fn handle_sigwinch(_: nix::libc::c_int) {
    if let Some(&master_fd) = MASTER_FD.get() {
        let ws = get_window_size();
        set_window_size(master_fd, &ws);
    }
}

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    output_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
) {
    let mut buf = bytes::BytesMut::with_capacity(IO_BUFFER_SIZE);
    let mut output_rx = output_rx;

    loop {
        buf.clear();

        tokio::select! {
            result = stream.read_buf(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        let request: tap_protocol::Request = match serde_json::from_slice(&buf) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!("invalid request: {e}");
                                continue;
                            }
                        };

                        let response = match request {
                            tap_protocol::Request::GetScrollback { lines } => {
                                let scrollback = SCROLLBACK.read();
                                let content = scrollback.get_lines(lines);
                                tap_protocol::Response::Scrollback { content }
                            }
                            tap_protocol::Request::GetCursor => {
                                let scrollback = SCROLLBACK.read();
                                let (row, col) = scrollback.cursor_position();
                                tap_protocol::Response::Cursor { row, col }
                            }
                            tap_protocol::Request::Inject { data } => {
                                if let Some(&master_fd) = MASTER_FD.get() {
                                    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_fd) };
                                    let result = nix::unistd::write(&fd, data.as_bytes());
                                    std::mem::forget(fd);
                                    match result {
                                        Ok(_) => tap_protocol::Response::Ok,
                                        Err(e) => tap_protocol::Response::Error { message: e.to_string() },
                                    }
                                } else {
                                    tap_protocol::Response::Error { message: "no master FD".to_string() }
                                }
                            }
                            tap_protocol::Request::GetSize => {
                                let ws = get_window_size();
                                tap_protocol::Response::Size {
                                    rows: ws.ws_row,
                                    cols: ws.ws_col,
                                }
                            }
                            tap_protocol::Request::Subscribe => {
                                tap_protocol::Response::Subscribed
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
                        tracing::error!("read error: {e}");
                        break;
                    }
                }
            }
            result = output_rx.recv() => {
                match result {
                    Ok(data) => {
                        let response = tap_protocol::Response::Output { data };
                        let response_bytes = serde_json::to_vec(&response).unwrap();
                        if stream.write_all(&response_bytes).await.is_err() {
                            break;
                        }
                        if stream.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn run_socket_server(
    socket_path: std::path::PathBuf,
    output_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);
    let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    tracing::info!("listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tracing::debug!("client connected");
                let output_rx = output_tx.subscribe();
                tokio::spawn(handle_client(stream, output_rx));
            }
            Err(e) => {
                tracing::error!("accept error: {e}");
            }
        }
    }
}

fn wait_for_child(child: nix::unistd::Pid) -> i32 {
    loop {
        match nix::sys::wait::waitpid(child, None) {
            Ok(nix::sys::wait::WaitStatus::Exited(_, code)) => return code,
            Ok(nix::sys::wait::WaitStatus::Signaled(_, sig, _)) => return 128 + sig as i32,
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
    let tap_config = tap_config::load().wrap_err("failed to load tap configuration")?;
    let mut input_processor = input::InputProcessor::new(&tap_config)
        .wrap_err("failed to initialize input processor")?;
    let editor_cmd = tap_config::get_editor(&tap_config);

    let session_id = config
        .session_id
        .unwrap_or_else(|| human_id::gen_id(HUMAN_ID_WORDS));

    let socket_dir = tap_protocol::socket_dir();
    std::fs::create_dir_all(&socket_dir)
        .wrap_err_with(|| format!("failed to create socket directory {}", socket_dir.display()))?;
    let socket_path = tap_protocol::socket_path(&session_id);

    let command = if config.command.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| DEFAULT_SHELL.to_string())]
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
    )
    .wrap_err_with(|| format!("failed to write sessions file {}", sessions_file.display()))?;

    // Open PTY using openpty
    let ws = get_window_size();
    let nix::pty::OpenptyResult { master, slave } =
        nix::pty::openpty(Some(&ws), None).map_err(|e| eyre::eyre!("openpty failed: {e}"))?;

    let master_raw_fd = master.as_raw_fd();

    // Store master FD for signal handler
    MASTER_FD
        .set(master_raw_fd)
        .map_err(|_| eyre::eyre!("failed to set MASTER_FD — was run() called multiple times?"))?;

    // Set up SIGWINCH handler
    unsafe {
        nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGWINCH,
            nix::sys::signal::SigHandler::Handler(handle_sigwinch),
        )
        .map_err(|e| eyre::eyre!("failed to set SIGWINCH handler: {e}"))?;
    }

    // Fork child process
    let child_pid = match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Child) => {
            drop(master);

            nix::unistd::setsid().expect("setsid failed");

            // Set controlling terminal
            unsafe {
                nix::libc::ioctl(slave.as_raw_fd(), nix::libc::TIOCSCTTY as _, 0);
            }

            // Dup slave to stdin/stdout/stderr using libc directly
            let slave_raw = slave.as_raw_fd();
            unsafe {
                nix::libc::dup2(slave_raw, nix::libc::STDIN_FILENO);
                nix::libc::dup2(slave_raw, nix::libc::STDOUT_FILENO);
                nix::libc::dup2(slave_raw, nix::libc::STDERR_FILENO);
            }

            if slave_raw > 2 {
                drop(slave);
            }

            let c_cmd: Vec<std::ffi::CString> = command
                .iter()
                .map(|s| std::ffi::CString::new(s.as_str()).unwrap())
                .collect();

            nix::unistd::execvp(&c_cmd[0], &c_cmd).expect("execvp failed");
            unreachable!()
        }
        Ok(nix::unistd::ForkResult::Parent { child }) => child,
        Err(e) => {
            return Err(eyre::eyre!("fork failed: {e}"));
        }
    };

    // Close slave in parent
    drop(slave);

    // Save terminal state and set raw mode
    let stdin_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(nix::libc::STDIN_FILENO) };
    let orig_termios = match setup_terminal(&stdin_fd) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!("not a terminal or failed to set raw mode: {e}");
            None
        }
    };
    // Don't close stdin
    std::mem::forget(stdin_fd);

    // Enable Kitty keyboard protocol for proper Alt-key detection
    let keyboard_enhanced = if orig_termios.is_some() {
        let mut stdout = std::io::stdout();
        match execute!(
            stdout,
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        ) {
            Ok(()) => {
                tracing::debug!("enabled Kitty keyboard protocol");
                true
            }
            Err(e) => {
                tracing::debug!("Kitty keyboard protocol not supported: {e}");
                false
            }
        }
    } else {
        false
    };

    // Set up broadcast channel for output
    let (output_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(BROADCAST_CHANNEL_SIZE);

    // Start server
    let server_output_tx = output_tx.clone();
    let server_socket_path = socket_path.clone();
    tokio::spawn(async move {
        if let Err(e) = run_socket_server(server_socket_path, server_output_tx).await {
            tracing::error!("server error: {e}");
        }
    });

    println!("\x1b[2m[tap: session {session_id}]\x1b[0m");

    // Main I/O loop
    let mut master_file = tokio::fs::File::from_std(unsafe {
        std::fs::File::from_raw_fd(master.as_raw_fd())
    });
    // Prevent double-close
    std::mem::forget(master);

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut master_buf = vec![0u8; IO_BUFFER_SIZE];
    let mut stdin_buf = vec![0u8; IO_BUFFER_SIZE];

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
                        tracing::debug!("master read error: {e}");
                        break 0;
                    }
                }
            }
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) => break 0,
                    Ok(n) => {
                        let input_bytes = &stdin_buf[..n];
                        tracing::debug!("stdin received {} bytes: {:02x?}", n, input_bytes);
                        match input_processor.process(input_bytes) {
                            input::InputResult::Passthrough(bytes) => {
                                if !bytes.is_empty() {
                                    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_raw_fd) };
                                    if nix::unistd::write(&fd, &bytes).is_err() {
                                        std::mem::forget(fd);
                                        break 1;
                                    }
                                    std::mem::forget(fd);
                                }
                            }
                            input::InputResult::Action(input::KeybindAction::OpenEditor) => {
                                tracing::debug!("OpenEditor action triggered!");
                                let scrollback_content = SCROLLBACK.read().get_lines(None);
                                if let Err(e) = editor::open_scrollback_in_editor(
                                    &scrollback_content,
                                    &editor_cmd,
                                    orig_termios.as_ref(),
                                ) {
                                    tracing::error!("failed to open editor: {e}");
                                }
                            }
                            input::InputResult::NeedMore => {
                                // Wait for timeout or more input
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("stdin read error: {e}");
                        break 0;
                    }
                }
            }
            _ = tokio::time::sleep(input_processor.escape_timeout()), if input_processor.has_pending_escape() => {
                if let input::InputResult::Passthrough(bytes) = input_processor.timeout_escape()
                    && !bytes.is_empty()
                {
                    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_raw_fd) };
                    let _ = nix::unistd::write(&fd, &bytes);
                    std::mem::forget(fd);
                }
            }
        }
    };

    // Disable Kitty keyboard protocol
    if keyboard_enhanced {
        let mut stdout = std::io::stdout();
        let _ = execute!(stdout, crossterm::event::PopKeyboardEnhancementFlags);
        tracing::debug!("disabled Kitty keyboard protocol");
    }

    // Restore terminal
    if let Some(ref termios) = orig_termios {
        let stdin_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(nix::libc::STDIN_FILENO) };
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
