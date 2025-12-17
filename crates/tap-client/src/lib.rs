//! Client library for interacting with tap sessions.

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

pub use tap_protocol::{Request, Response, Session, sessions_file, socket_dir, socket_path};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no active tap sessions found — start one with `tap`")]
    NoSessions,
    #[error("session '{0}' not found — run `tap list` to see active sessions")]
    SessionNotFound(String),
    #[error("server error: {0}")]
    Server(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// List all active tap sessions.
pub fn list_sessions() -> Result<Vec<Session>> {
    let sessions_file = sessions_file();
    let content = std::fs::read_to_string(&sessions_file).unwrap_or_else(|_| "[]".to_string());
    let sessions: Vec<Session> = serde_json::from_str(&content)?;

    // Filter to only sessions with valid sockets
    let sessions: Vec<Session> = sessions
        .into_iter()
        .filter(|s| socket_path(&s.id).exists())
        .collect();

    Ok(sessions)
}

/// Client for interacting with a tap session.
pub struct Client {
    stream: tokio::io::BufReader<tokio::net::UnixStream>,
}

impl Client {
    /// Connect to a session by ID.
    pub async fn connect(session_id: &str) -> Result<Self> {
        let path = socket_path(session_id);
        if !path.exists() {
            return Err(Error::SessionNotFound(session_id.to_string()));
        }
        let stream = tokio::net::UnixStream::connect(&path).await?;
        Ok(Self {
            stream: tokio::io::BufReader::new(stream),
        })
    }

    /// Connect to the most recent session.
    pub async fn connect_latest() -> Result<Self> {
        let sessions = list_sessions()?;
        let session = sessions.last().ok_or(Error::NoSessions)?;
        Self::connect(&session.id).await
    }

    async fn send_request(&mut self, request: &Request) -> Result<Response> {
        let request_bytes = serde_json::to_vec(request)?;
        self.stream.get_mut().write_all(&request_bytes).await?;

        let mut line = String::new();
        self.stream.read_line(&mut line).await?;
        let response: Response = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Get scrollback buffer content.
    pub async fn get_scrollback(&mut self, lines: Option<usize>) -> Result<String> {
        let response = self.send_request(&Request::GetScrollback { lines }).await?;
        match response {
            Response::Scrollback { content } => Ok(content),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Get cursor position (row, col).
    pub async fn get_cursor(&mut self) -> Result<(usize, usize)> {
        let response = self.send_request(&Request::GetCursor).await?;
        match response {
            Response::Cursor { row, col } => Ok((row, col)),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Get terminal size (rows, cols).
    pub async fn get_size(&mut self) -> Result<(u16, u16)> {
        let response = self.send_request(&Request::GetSize).await?;
        match response {
            Response::Size { rows, cols } => Ok((rows, cols)),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Inject input into the PTY.
    pub async fn inject(&mut self, data: &str) -> Result<()> {
        let response = self
            .send_request(&Request::Inject {
                data: data.to_string(),
            })
            .await?;
        match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Subscribe to live output stream.
    /// After calling this, use `read_output()` to receive output chunks.
    pub async fn subscribe(&mut self) -> Result<()> {
        let response = self.send_request(&Request::Subscribe).await?;
        match response {
            Response::Subscribed => Ok(()),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Read the next output chunk after subscribing.
    /// Returns None if the connection is closed.
    pub async fn read_output(&mut self) -> Result<Option<Vec<u8>>> {
        let mut line = String::new();
        let n = self.stream.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let response: Response = serde_json::from_str(&line)?;
        match response {
            Response::Output { data } => Ok(Some(data)),
            Response::Error { message } => Err(Error::Server(message)),
            Response::SessionEnded { .. } => Ok(None),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Attach to the session (take over stdin/stdout).
    /// Returns the initial scrollback content if successful.
    pub async fn attach(&mut self, rows: u16, cols: u16) -> Result<String> {
        let response = self.send_request(&Request::Attach { rows, cols }).await?;
        match response {
            Response::Attached { scrollback } => Ok(scrollback),
            Response::Error { message } => Err(Error::Server(message)),
            _ => Err(Error::Server("unexpected response".to_string())),
        }
    }

    /// Send input to the PTY (for attached clients).
    pub async fn send_input(&mut self, data: Vec<u8>) -> Result<()> {
        let request = Request::Input { data };
        let request_bytes = serde_json::to_vec(&request)?;
        self.stream.get_mut().write_all(&request_bytes).await?;
        Ok(())
    }

    /// Resize the PTY (for attached clients).
    pub async fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let request = Request::Resize { rows, cols };
        let request_bytes = serde_json::to_vec(&request)?;
        self.stream.get_mut().write_all(&request_bytes).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_dir() {
        let dir = socket_dir();
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn test_list_sessions_empty() {
        // This should not panic even if no sessions exist
        let result = list_sessions();
        assert!(result.is_ok());
    }
}
