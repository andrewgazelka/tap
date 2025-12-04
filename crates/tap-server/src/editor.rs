//! Editor integration for viewing scrollback.

use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};

use nix::libc;
use nix::sys::termios::{self, SetArg, Termios};

/// Open scrollback content in the configured editor.
/// This function temporarily restores the terminal to cooked mode.
pub fn open_scrollback_in_editor(
    scrollback_content: &str,
    editor_cmd: &str,
    orig_termios: Option<&Termios>,
) -> eyre::Result<()> {
    // Create temp file with scrollback content
    let mut temp_file = tempfile::NamedTempFile::new()?;
    temp_file.write_all(scrollback_content.as_bytes())?;
    temp_file.flush()?;
    let temp_path = temp_file.path().to_owned();

    // Restore terminal to cooked mode if we have original termios
    let stdin_fd = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
    if let Some(termios) = orig_termios {
        let _ = termios::tcsetattr(&stdin_fd, SetArg::TCSANOW, termios);
    }
    std::mem::forget(stdin_fd);

    // Parse editor command and spawn
    let parts: Vec<&str> = editor_cmd.split_whitespace().collect();
    let (cmd, args) = parts
        .split_first()
        .ok_or_else(|| eyre::eyre!("Empty editor command"))?;

    let status = std::process::Command::new(cmd)
        .args(args.iter().copied())
        .arg(&temp_path)
        .status()?;

    if !status.success() {
        tracing::warn!("Editor exited with status: {status}");
    }

    // Restore raw mode
    let stdin_fd = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
    let mut raw = termios::tcgetattr(&stdin_fd)?;
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(&stdin_fd, SetArg::TCSANOW, &raw)?;
    std::mem::forget(stdin_fd);

    // Temp file is automatically deleted when temp_file drops
    Ok(())
}
