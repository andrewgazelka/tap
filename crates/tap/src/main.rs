//! Unified CLI for tap terminal sessions.

use eyre::WrapErr as _;
use tokio::io::AsyncWriteExt as _;

#[derive(clap::Parser)]
#[command(name = "tap", about = "Terminal introspection and control")]
struct Args {
    /// Enable debug logging to ~/.tap/debug.log
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start a recording session (default when no command given).
    Start {
        /// Command to run (defaults to $SHELL).
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
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

async fn run_start(command: Vec<String>) -> eyre::Result<()> {
    let config = tap_server::ServerConfig {
        command,
        session_id: None,
    };
    let exit_code = tap_server::run(config).await?;
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
            .join(".tap");
        std::fs::create_dir_all(&log_dir)?;
        let log_file = std::fs::File::create(log_dir.join("debug.log"))?;

        tracing_subscriber::fmt()
            .with_writer(log_file)
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .init();

        tracing::info!("debug logging enabled to ~/.tap/debug.log");
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    // Default to Start if no command given
    let command = args.command.unwrap_or(Command::Start { command: vec![] });

    match command {
        Command::Start { command } => {
            run_start(command).await?;
        }
        Command::List => {
            let sessions = tap_client::list_sessions()?;
            if sessions.is_empty() {
                println!("No active sessions");
            } else {
                println!("{:<25} {:<8} {:<25} COMMAND", "ID", "PID", "STARTED");
                for session in sessions {
                    println!(
                        "{:<25} {:<8} {:<25} {}",
                        session.id,
                        session.pid,
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
