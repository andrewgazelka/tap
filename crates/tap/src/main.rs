//! Unified CLI for tap terminal sessions.

use std::os::fd::BorrowedFd;

use eyre::WrapErr as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(clap::Parser)]
#[command(name = "tap", about = "Terminal session manager for tiling WM users")]
struct Args {
    /// Enable debug logging to ~/.tap/debug.log
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start a new session (default when no command given).
    Start {
        /// Command to run (defaults to $SHELL).
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
        /// Start detached (in background).
        #[arg(short, long)]
        detached: bool,
    },
    /// Attach to a running session.
    Attach {
        /// Session ID (uses latest if not specified).
        session: Option<String>,
    },
    /// List all active sessions.
    List,
    /// Get scrollback buffer from a session.
    Scrollback {
        /// Session ID (uses latest if not specified).
        #[arg(short, long)]
        session: Option<String>,
        /// Number of lines to retrieve.
        #[arg(short, long)]
        lines: Option<usize>,
    },
    /// Get cursor position.
    Cursor {
        /// Session ID (uses latest if not specified).
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Get terminal size.
    Size {
        /// Session ID (uses latest if not specified).
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Inject input into a session.
    Inject {
        /// Session ID (uses latest if not specified).
        #[arg(short, long)]
        session: Option<String>,
        /// Text to inject.
        text: String,
    },
    /// Subscribe to live output stream.
    Subscribe {
        /// Session ID (uses latest if not specified).
        #[arg(short, long)]
        session: Option<String>,
    },
}

async fn get_client(session: Option<String>) -> eyre::Result<tap_client::Client> {
    match session {
        Some(id) => tap_client::Client::connect(&id)
            .await
            .wrap_err_with(|| format!("failed to connect to session '{id}'")),
        None => tap_client::Client::connect_latest()
            .await
            .wrap_err("failed to connect to latest session"),
    }
}

async fn run_start(command: Vec<String>, detached: bool) -> eyre::Result<()> {
    let config = tap_server::ServerConfig {
        command,
        session_id: None,
        detached,
    };
    match tap_server::run(config).await? {
        tap_server::RunResult::Exited(code) => std::process::exit(code),
        tap_server::RunResult::Detached { session_id } => {
            if detached {
                // Started detached - keep the process running
                // Wait forever (the PTY loop runs in a background task)
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            } else {
                // User detached interactively
                println!("Use `tap attach {session_id}` to reattach");
                std::process::exit(0);
            }
        }
    }
}

fn get_window_size() -> (u16, u16) {
    let mut ws: nix::pty::Winsize = unsafe { std::mem::zeroed() };
    unsafe {
        nix::libc::ioctl(nix::libc::STDIN_FILENO, nix::libc::TIOCGWINSZ, &mut ws);
    }
    (ws.ws_row, ws.ws_col)
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

async fn run_attach(session: Option<String>) -> eyre::Result<()> {
    let mut client = get_client(session.clone()).await?;

    // Get current terminal size
    let (rows, cols) = get_window_size();

    // Attach to the session
    let scrollback = client
        .attach(rows, cols)
        .await
        .wrap_err("failed to attach to session")?;

    // Set up terminal
    let stdin_fd = unsafe { BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) };
    let orig_termios = setup_terminal(stdin_fd).ok();

    // Clear screen and print scrollback
    print!("\x1b[2J\x1b[H"); // Clear screen and move to top-left
    print!("{scrollback}");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let session_name = session.as_deref().unwrap_or("latest");
    eprintln!("\x1b[2m[attached to {session_name}]\x1b[0m");

    // Load config for keybinds
    let tap_config = tap_config::load().wrap_err("failed to load tap configuration")?;
    let mut input_processor = tap_server::input::InputProcessor::new(&tap_config)
        .wrap_err("failed to initialize input processor")?;

    // Main I/O loop
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut stdin_buf = vec![0u8; 4096];

    let exit_code = loop {
        tokio::select! {
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) => break 0,
                    Ok(n) => {
                        let input_bytes = &stdin_buf[..n];
                        match input_processor.process(input_bytes) {
                            tap_server::input::InputResult::Passthrough(bytes) => {
                                if !bytes.is_empty() {
                                    if let Err(e) = client.send_input(bytes).await {
                                        tracing::debug!("send_input error: {e}");
                                        break 1;
                                    }
                                }
                            }
                            tap_server::input::InputResult::Action(tap_server::input::KeybindAction::Detach) => {
                                break 0;
                            }
                            tap_server::input::InputResult::Action(tap_server::input::KeybindAction::OpenEditor) => {
                                // Not supported in attach mode
                            }
                            tap_server::input::InputResult::NeedMore => {
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
            result = client.read_output() => {
                match result {
                    Ok(Some(data)) => {
                        if stdout.write_all(&data).await.is_err() {
                            break 1;
                        }
                        let _ = stdout.flush().await;
                    }
                    Ok(None) => {
                        // Session ended
                        break 0;
                    }
                    Err(e) => {
                        tracing::debug!("read_output error: {e}");
                        break 0;
                    }
                }
            }
            _ = tokio::time::sleep(input_processor.escape_timeout()), if input_processor.has_pending_escape() => {
                if let tap_server::input::InputResult::Passthrough(bytes) = input_processor.timeout_escape()
                    && !bytes.is_empty()
                {
                    let _ = client.send_input(bytes).await;
                }
            }
        }
    };

    // Restore terminal
    if let Some(ref termios) = orig_termios {
        restore_terminal(stdin_fd, termios);
    }

    eprintln!("\n\x1b[2m[detached]\x1b[0m");

    std::process::exit(exit_code);
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let args = <Args as clap::Parser>::parse();

    // Setup logging
    if args.debug {
        let log_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".tap")
            .join("logs");
        std::fs::create_dir_all(&log_dir)?;

        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let log_filename = format!("{timestamp}.log");
        let log_path = log_dir.join(&log_filename);
        let log_file = std::fs::File::create(&log_path)?;

        tracing_subscriber::fmt()
            .with_writer(log_file)
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .init();

        eprintln!("debug log: {}", log_path.display());
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    // Default to Start if no command given
    let command = args.command.unwrap_or(Command::Start {
        command: vec![],
        detached: false,
    });

    match command {
        Command::Start { command, detached } => {
            run_start(command, detached).await?;
        }
        Command::Attach { session } => {
            run_attach(session).await?;
        }
        Command::List => {
            let sessions = tap_client::list_sessions()?;
            if sessions.is_empty() {
                println!("No active sessions");
            } else {
                println!(
                    "{:<25} {:<8} {:<10} {:<25} COMMAND",
                    "ID", "PID", "ATTACHED", "STARTED"
                );
                for session in sessions {
                    let attached_str = if session.attached { "yes" } else { "no" };
                    println!(
                        "{:<25} {:<8} {:<10} {:<25} {}",
                        session.id,
                        session.pid,
                        attached_str,
                        session.started,
                        session.command.join(" ")
                    );
                }
            }
        }
        Command::Scrollback { session, lines } => {
            let mut client = get_client(session).await?;
            let content = client.get_scrollback(lines).await?;
            print!("{content}");
        }
        Command::Cursor { session } => {
            let mut client = get_client(session).await?;
            let (row, col) = client.get_cursor().await?;
            println!("Row: {row}, Col: {col}");
        }
        Command::Size { session } => {
            let mut client = get_client(session).await?;
            let (rows, cols) = client.get_size().await?;
            println!("{rows}x{cols}");
        }
        Command::Inject { session, text } => {
            let mut client = get_client(session).await?;
            client.inject(&text).await?;
            println!("Injected");
        }
        Command::Subscribe { session } => {
            let mut client = get_client(session).await?;
            client.subscribe().await?;
            let mut stdout = tokio::io::stdout();
            while let Some(data) = client.read_output().await? {
                stdout.write_all(&data).await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}
