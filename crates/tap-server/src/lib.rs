//! PTY wrapper server library for terminal introspection.

mod editor;
pub mod input;
mod kitty;
pub mod scrollback;

use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::execute;
use eyre::WrapErr as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Mutex;

const DEFAULT_SHELL: &str = "/bin/sh";
const HUMAN_ID_WORDS: usize = 3;
const BROADCAST_CHANNEL_SIZE: usize = 1024;
const IO_BUFFER_SIZE: usize = 4096;

/// Atomically modify the sessions file with exclusive locking.
fn modify_sessions_file(
    path: &std::path::Path,
    f: impl FnOnce(&mut Vec<serde_json::Value>),
) -> eyre::Result<()> {
    use std::io::{Read as _, Seek as _, Write as _};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .wrap_err_with(|| format!("failed to open sessions file {}", path.display()))?;

    file.lock()
        .wrap_err_with(|| format!("failed to lock sessions file {}", path.display()))?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .wrap_err("failed to read sessions file")?;

    let mut sessions: Vec<serde_json::Value> = if content.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&content).unwrap_or_default()
    };

    f(&mut sessions);

    file.set_len(0)
        .wrap_err("failed to truncate sessions file")?;
    file.seek(std::io::SeekFrom::Start(0))
        .wrap_err("failed to seek sessions file")?;
    file.write_all(serde_json::to_string_pretty(&sessions).unwrap().as_bytes())
        .wrap_err("failed to write sessions file")?;

    // Lock released on drop
    Ok(())
}

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
    /// Start detached (no terminal attached).
    pub detached: bool,
}

fn setup_terminal(fd: BorrowedFd<'_>) -> nix::Result<nix::sys::termios::Termios> {
    let orig = nix::sys::termios::tcgetattr(fd)?;
    let mut raw = orig.clone();
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(fd, nix::sys::termios::SetArg::TCSANOW, &raw)?;
    Ok(orig)
}

fn restore_terminal(fd: BorrowedFd<'_>, termios: &nix::sys::termios::Termios) {
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

fn set_window_size_raw(fd: i32, rows: u16, cols: u16) {
    let ws = nix::pty::Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    set_window_size(fd, &ws);
}

/// Channel for sending input to the PTY from attached clients.
type InputSender = tokio::sync::mpsc::UnboundedSender<Vec<u8>>;
type InputReceiver = tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>;

/// Shared state for attached client.
struct AttachedClient {
    /// Sender for PTY output to the attached client.
    output_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

/// Handle JSON protocol clients (scrollback queries, inject, etc.).
async fn handle_json_client(
    mut stream: tokio::net::UnixStream,
    output_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    input_tx: InputSender,
    attached_client: Arc<Mutex<Option<AttachedClient>>>,
    session_ended: Arc<AtomicBool>,
) {
    let mut buf = bytes::BytesMut::with_capacity(IO_BUFFER_SIZE);
    let mut output_rx = output_rx;

    loop {
        buf.clear();

        if session_ended.load(Ordering::Relaxed) {
            let response = tap_protocol::Response::SessionEnded { exit_code: 0 };
            let response_bytes = serde_json::to_vec(&response).unwrap();
            let _ = stream.write_all(&response_bytes).await;
            let _ = stream.write_all(b"\n").await;
            break;
        }

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
                                if input_tx.send(data.into_bytes()).is_ok() {
                                    tap_protocol::Response::Ok
                                } else {
                                    tap_protocol::Response::Error { message: "session ended".to_string() }
                                }
                            }
                            tap_protocol::Request::GetSize => {
                                if let Some(&master_fd) = MASTER_FD.get() {
                                    let mut ws: nix::pty::Winsize = unsafe { std::mem::zeroed() };
                                    unsafe {
                                        nix::libc::ioctl(master_fd, nix::libc::TIOCGWINSZ, &mut ws);
                                    }
                                    tap_protocol::Response::Size {
                                        rows: ws.ws_row,
                                        cols: ws.ws_col,
                                    }
                                } else {
                                    tap_protocol::Response::Error { message: "no master FD".to_string() }
                                }
                            }
                            tap_protocol::Request::Subscribe => {
                                tap_protocol::Response::Subscribed
                            }
                            tap_protocol::Request::Attach { rows, cols } => {
                                // Check if already attached
                                let mut attached = attached_client.lock().await;
                                if attached.is_some() {
                                    tap_protocol::Response::Error { message: "session already has attached client".to_string() }
                                } else {
                                    // Set up attached client
                                    let (client_output_tx, mut client_output_rx) = tokio::sync::mpsc::unbounded_channel();
                                    *attached = Some(AttachedClient { output_tx: client_output_tx });
                                    drop(attached);

                                    // Resize PTY to client's terminal size
                                    if let Some(&master_fd) = MASTER_FD.get() {
                                        set_window_size_raw(master_fd, rows, cols);
                                    }

                                    // Get current scrollback for initial display
                                    let scrollback = SCROLLBACK.read().get_lines(None);

                                    // Send attach response
                                    let response = tap_protocol::Response::Attached { scrollback };
                                    let response_bytes = serde_json::to_vec(&response).unwrap();
                                    if stream.write_all(&response_bytes).await.is_err() {
                                        let mut attached = attached_client.lock().await;
                                        *attached = None;
                                        break;
                                    }
                                    if stream.write_all(b"\n").await.is_err() {
                                        let mut attached = attached_client.lock().await;
                                        *attached = None;
                                        break;
                                    }

                                    // Now switch to binary I/O mode for this client
                                    // Split stream for bidirectional communication
                                    let (mut read_half, mut write_half) = stream.into_split();

                                    // Forward input from client to PTY
                                    let input_tx_clone = input_tx.clone();
                                    let attached_client_clone = attached_client.clone();
                                    let session_ended_clone = session_ended.clone();
                                    tokio::spawn(async move {
                                        let mut buf = vec![0u8; IO_BUFFER_SIZE];
                                        loop {
                                            if session_ended_clone.load(Ordering::Relaxed) {
                                                break;
                                            }
                                            match read_half.read(&mut buf).await {
                                                Ok(0) => break,
                                                Ok(n) => {
                                                    // Parse as protocol message first
                                                    if let Ok(request) = serde_json::from_slice::<tap_protocol::Request>(&buf[..n]) {
                                                        match request {
                                                            tap_protocol::Request::Input { data } => {
                                                                if input_tx_clone.send(data).is_err() {
                                                                    break;
                                                                }
                                                            }
                                                            tap_protocol::Request::Resize { rows, cols } => {
                                                                if let Some(&master_fd) = MASTER_FD.get() {
                                                                    set_window_size_raw(master_fd, rows, cols);
                                                                }
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                                Err(_) => break,
                                            }
                                        }
                                        // Client disconnected - clear attached state
                                        let mut attached = attached_client_clone.lock().await;
                                        *attached = None;
                                    });

                                    // Forward output from PTY to client
                                    loop {
                                        tokio::select! {
                                            Some(data) = client_output_rx.recv() => {
                                                let response = tap_protocol::Response::Output { data };
                                                let response_bytes = serde_json::to_vec(&response).unwrap();
                                                if write_half.write_all(&response_bytes).await.is_err() {
                                                    break;
                                                }
                                                if write_half.write_all(b"\n").await.is_err() {
                                                    break;
                                                }
                                            }
                                            else => break,
                                        }
                                    }

                                    // Session ended or client disconnected
                                    return;
                                }
                            }
                            tap_protocol::Request::Input { data } => {
                                // Direct input (for non-attached clients)
                                if input_tx.send(data).is_ok() {
                                    tap_protocol::Response::Ok
                                } else {
                                    tap_protocol::Response::Error { message: "session ended".to_string() }
                                }
                            }
                            tap_protocol::Request::Resize { rows, cols } => {
                                if let Some(&master_fd) = MASTER_FD.get() {
                                    set_window_size_raw(master_fd, rows, cols);
                                    tap_protocol::Response::Ok
                                } else {
                                    tap_protocol::Response::Error { message: "no master FD".to_string() }
                                }
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
    input_tx: InputSender,
    attached_client: Arc<Mutex<Option<AttachedClient>>>,
    session_ended: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);
    let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    tracing::info!("listening on {}", socket_path.display());

    loop {
        if session_ended.load(Ordering::Relaxed) {
            break Ok(());
        }

        match listener.accept().await {
            Ok((stream, _)) => {
                tracing::debug!("client connected");
                let output_rx = output_tx.subscribe();
                let input_tx = input_tx.clone();
                let attached_client = attached_client.clone();
                let session_ended = session_ended.clone();
                tokio::spawn(handle_json_client(
                    stream,
                    output_rx,
                    input_tx,
                    attached_client,
                    session_ended,
                ));
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

/// Result of running in attached mode.
pub enum RunResult {
    /// Session ended normally with exit code.
    Exited(i32),
    /// User detached from session.
    Detached { session_id: String },
}

/// Run the PTY server with the given configuration.
pub async fn run(config: ServerConfig) -> eyre::Result<RunResult> {
    // Load tap config for keybinds
    let tap_config = tap_config::load().wrap_err("failed to load tap configuration")?;
    let mut input_processor =
        input::InputProcessor::new(&tap_config).wrap_err("failed to initialize input processor")?;
    let editor_cmd = tap_config::get_editor(&tap_config);

    let session_id = config
        .session_id
        .unwrap_or_else(|| human_id::gen_id(HUMAN_ID_WORDS));

    let socket_dir = tap_protocol::socket_dir();
    std::fs::create_dir_all(&socket_dir)
        .wrap_err_with(|| format!("failed to create socket directory {}", socket_dir.display()))?;
    let socket_path = tap_protocol::socket_path(&session_id);

    let command = if config.command.is_empty() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| DEFAULT_SHELL.to_string());
        // Force login/interactive mode for shells that need it to load config
        if shell.ends_with("/nu") || shell.ends_with("/nushell") {
            vec![shell, "-l".to_string()] // nushell needs -l to load config.nu
        } else if shell.ends_with("/bash") || shell.ends_with("/zsh") {
            vec![shell, "-i".to_string()]
        } else {
            vec![shell]
        }
    } else {
        config.command.clone()
    };

    // Write session info (with file locking for concurrent access)
    let sessions_file = tap_protocol::sessions_file();
    let session_id_clone = session_id.clone();
    let command_clone = command.clone();
    modify_sessions_file(&sessions_file, |sessions| {
        sessions.push(serde_json::json!({
            "id": session_id_clone,
            "pid": std::process::id(),
            "started": chrono::Utc::now().to_rfc3339(),
            "command": command_clone,
            "attached": !config.detached,
        }));
    })?;

    // Open PTY using openpty
    let ws = if config.detached {
        // Default size for detached sessions
        nix::pty::Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    } else {
        get_window_size()
    };
    let nix::pty::OpenptyResult { master, slave } =
        nix::pty::openpty(Some(&ws), None).map_err(|e| eyre::eyre!("openpty failed: {e}"))?;

    let master_raw_fd = master.as_raw_fd();

    // Store master FD for signal handler
    MASTER_FD
        .set(master_raw_fd)
        .map_err(|_| eyre::eyre!("failed to set MASTER_FD — was run() called multiple times?"))?;

    // Set up SIGWINCH handler (only if attached)
    if !config.detached {
        unsafe {
            extern "C" fn handle_sigwinch(_: nix::libc::c_int) {
                if let Some(&master_fd) = MASTER_FD.get() {
                    let ws = get_window_size();
                    set_window_size(master_fd, &ws);
                }
            }
            nix::sys::signal::signal(
                nix::sys::signal::Signal::SIGWINCH,
                nix::sys::signal::SigHandler::Handler(handle_sigwinch),
            )
            .map_err(|e| eyre::eyre!("failed to set SIGWINCH handler: {e}"))?;
        }
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

    // Set up broadcast channel for output
    let (output_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(BROADCAST_CHANNEL_SIZE);

    // Set up input channel
    let (input_tx, mut input_rx): (InputSender, InputReceiver) =
        tokio::sync::mpsc::unbounded_channel();

    // Attached client state
    let attached_client: Arc<Mutex<Option<AttachedClient>>> = Arc::new(Mutex::new(None));
    let session_ended = Arc::new(AtomicBool::new(false));

    // Start server
    let server_output_tx = output_tx.clone();
    let server_socket_path = socket_path.clone();
    let server_input_tx = input_tx.clone();
    let server_attached_client = attached_client.clone();
    let server_session_ended = session_ended.clone();
    tokio::spawn(async move {
        if let Err(e) = run_socket_server(
            server_socket_path,
            server_output_tx,
            server_input_tx,
            server_attached_client,
            server_session_ended,
        )
        .await
        {
            tracing::error!("server error: {e}");
        }
    });

    let shell_name = std::path::Path::new(&command[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&command[0]);

    // If starting detached, fork to background and return
    if config.detached {
        println!("\x1b[2m[tap: {shell_name} · {session_id} (detached)]\x1b[0m");

        // Run PTY I/O loop in background
        let master_file =
            tokio::fs::File::from_std(unsafe { std::fs::File::from_raw_fd(master.as_raw_fd()) });
        std::mem::forget(master);

        let output_tx_clone = output_tx.clone();
        let attached_client_clone = attached_client.clone();
        let session_ended_clone = session_ended.clone();
        let sessions_file_clone = sessions_file.clone();
        let session_id_clone = session_id.clone();
        let socket_path_clone = socket_path.clone();

        tokio::spawn(async move {
            run_pty_loop_detached(
                master_file,
                master_raw_fd,
                input_rx,
                output_tx_clone,
                attached_client_clone,
                session_ended_clone,
                child_pid,
                sessions_file_clone,
                session_id_clone,
                socket_path_clone,
            )
            .await;
        });

        return Ok(RunResult::Detached { session_id });
    }

    // Running attached - set up terminal
    let stdin_fd = unsafe { BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) };
    let orig_termios = match setup_terminal(stdin_fd) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!("not a terminal or failed to set raw mode: {e}");
            None
        }
    };

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

    println!("\x1b[2m[tap: {shell_name} · {session_id}]\x1b[0m");

    // Main I/O loop
    let mut master_file =
        tokio::fs::File::from_std(unsafe { std::fs::File::from_raw_fd(master.as_raw_fd()) });
    // Prevent double-close
    std::mem::forget(master);

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut master_buf = vec![0u8; IO_BUFFER_SIZE];
    let mut stdin_buf = vec![0u8; IO_BUFFER_SIZE];

    let mut detached = false;
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
                                    // Always translate CSI u sequences to traditional terminal input.
                                    let translated = kitty::translate_all_csi_u(&bytes);
                                    if translated != bytes {
                                        tracing::debug!(
                                            "translated CSI u: {:02x?} -> {:02x?}",
                                            bytes,
                                            translated
                                        );
                                    }

                                    let fd = unsafe { BorrowedFd::borrow_raw(master_raw_fd) };
                                    if nix::unistd::write(fd, &translated).is_err() {
                                        break 1;
                                    }
                                }
                            }
                            input::InputResult::Action(input::KeybindAction::OpenEditor) => {
                                tracing::debug!("OpenEditor action triggered!");
                                let scrollback = SCROLLBACK.read();
                                let scrollback_content = scrollback.get_lines(None);
                                let (cursor_row, cursor_col) = scrollback.cursor_position();

                                let total_lines = scrollback_content.lines().count();
                                let viewport_height = 24;
                                let cursor_line =
                                    total_lines.saturating_sub(viewport_height) + cursor_row + 1;

                                drop(scrollback);

                                if let Err(e) = editor::open_scrollback_in_editor(
                                    &scrollback_content,
                                    &editor_cmd,
                                    orig_termios.as_ref(),
                                    Some(tap_editor::Position::new(cursor_line, Some(cursor_col + 1))),
                                ) {
                                    tracing::error!("failed to open editor: {e}");
                                }
                            }
                            input::InputResult::Action(input::KeybindAction::Detach) => {
                                tracing::debug!("Detach action triggered!");
                                detached = true;
                                break 0;
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
            Some(data) = input_rx.recv() => {
                // Input from socket clients
                let fd = unsafe { BorrowedFd::borrow_raw(master_raw_fd) };
                let _ = nix::unistd::write(fd, &data);
            }
            _ = tokio::time::sleep(input_processor.escape_timeout()), if input_processor.has_pending_escape() => {
                if let input::InputResult::Passthrough(bytes) = input_processor.timeout_escape()
                    && !bytes.is_empty()
                {
                    let translated = kitty::translate_all_csi_u(&bytes);
                    let fd = unsafe { BorrowedFd::borrow_raw(master_raw_fd) };
                    let _ = nix::unistd::write(fd, &translated);
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
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) };
        restore_terminal(stdin_fd, termios);
    }

    if detached {
        // Update session to show detached
        let _ = modify_sessions_file(&sessions_file, |sessions| {
            for s in sessions.iter_mut() {
                if s.get("id").and_then(|v| v.as_str()) == Some(&session_id) {
                    s["attached"] = serde_json::json!(false);
                }
            }
        });

        println!("\n\x1b[2m[detached from {session_id}]\x1b[0m");

        // Continue PTY server in background
        let output_tx_clone = output_tx.clone();
        let attached_client_clone = attached_client.clone();
        let session_ended_clone = session_ended.clone();
        let sessions_file_clone = sessions_file.clone();
        let session_id_clone = session_id.clone();
        let socket_path_clone = socket_path.clone();

        tokio::spawn(async move {
            run_pty_loop_detached(
                master_file,
                master_raw_fd,
                input_rx,
                output_tx_clone,
                attached_client_clone,
                session_ended_clone,
                child_pid,
                sessions_file_clone,
                session_id_clone,
                socket_path_clone,
            )
            .await;
        });

        return Ok(RunResult::Detached { session_id });
    }

    // Clean up socket and session entry
    let _ = std::fs::remove_file(&socket_path);

    // Remove session from sessions.json (with file locking)
    let _ = modify_sessions_file(&sessions_file, |sessions| {
        sessions.retain(|s| s.get("id").and_then(|v| v.as_str()) != Some(&session_id));
    });

    // Wait for child
    let final_code = wait_for_child(child_pid);

    if final_code == 0 && exit_code == 0 {
        Ok(RunResult::Exited(0))
    } else {
        Ok(RunResult::Exited(final_code))
    }
}

/// Run the PTY I/O loop in detached mode (no local terminal).
async fn run_pty_loop_detached(
    mut master_file: tokio::fs::File,
    master_raw_fd: i32,
    mut input_rx: InputReceiver,
    output_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    attached_client: Arc<Mutex<Option<AttachedClient>>>,
    session_ended: Arc<AtomicBool>,
    child_pid: nix::unistd::Pid,
    sessions_file: std::path::PathBuf,
    session_id: String,
    socket_path: std::path::PathBuf,
) {
    let mut master_buf = vec![0u8; IO_BUFFER_SIZE];

    loop {
        tokio::select! {
            result = master_file.read(&mut master_buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = master_buf[..n].to_vec();

                        // Update scrollback
                        SCROLLBACK.write().push(&data);

                        // Broadcast to subscribers
                        let _ = output_tx.send(data.clone());

                        // Send to attached client if any
                        if let Some(client) = attached_client.lock().await.as_ref() {
                            let _ = client.output_tx.send(data);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("master read error: {e}");
                        break;
                    }
                }
            }
            Some(data) = input_rx.recv() => {
                let fd = unsafe { BorrowedFd::borrow_raw(master_raw_fd) };
                let _ = nix::unistd::write(fd, &data);
            }
        }
    }

    // Mark session as ended
    session_ended.store(true, Ordering::Relaxed);

    // Clean up socket and session entry
    let _ = std::fs::remove_file(&socket_path);
    let _ = modify_sessions_file(&sessions_file, |sessions| {
        sessions.retain(|s| s.get("id").and_then(|v| v.as_str()) != Some(&session_id));
    });

    // Wait for child
    let _ = wait_for_child(child_pid);
}
