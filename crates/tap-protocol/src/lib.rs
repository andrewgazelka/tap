//! Shared protocol types for tap terminal sessions.

/// Session metadata stored in sessions.json.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub started: String,
    pub command: Vec<String>,
    /// Whether a client is currently attached to this session.
    #[serde(default)]
    pub attached: bool,
}

/// Client requests to the server.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Get the last N lines from scrollback buffer.
    GetScrollback { lines: Option<usize> },
    /// Get current cursor position.
    GetCursor,
    /// Inject input into the PTY.
    Inject { data: String },
    /// Get terminal size.
    GetSize,
    /// Subscribe to live output.
    Subscribe,
    /// Attach to the session (take over stdin/stdout).
    Attach {
        /// Terminal rows.
        rows: u16,
        /// Terminal columns.
        cols: u16,
    },
    /// Send input from attached client to PTY.
    Input { data: Vec<u8> },
    /// Resize the PTY from attached client.
    Resize { rows: u16, cols: u16 },
}

/// Server responses.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Scrollback buffer content.
    Scrollback { content: String },
    /// Cursor position.
    Cursor { row: usize, col: usize },
    /// Terminal size.
    Size { rows: u16, cols: u16 },
    /// Live output data (for subscribed clients).
    Output { data: Vec<u8> },
    /// Subscription confirmed.
    Subscribed,
    /// Attach confirmed - client now owns stdin/stdout.
    Attached {
        /// Current scrollback content for initial display.
        scrollback: String,
    },
    /// Session has ended (child process exited).
    SessionEnded { exit_code: i32 },
    /// Success.
    Ok,
    /// Error.
    Error { message: String },
}

/// Get the socket directory path.
#[must_use]
pub fn socket_dir() -> std::path::PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".tap")))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/tap"))
}

/// Get socket path for a session ID.
#[must_use]
pub fn socket_path(session_id: &str) -> std::path::PathBuf {
    socket_dir().join(format!("{session_id}.sock"))
}

/// Get the sessions index file path.
#[must_use]
pub fn sessions_file() -> std::path::PathBuf {
    socket_dir().join("sessions.json")
}
