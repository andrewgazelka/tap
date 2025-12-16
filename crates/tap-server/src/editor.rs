//! Editor integration for viewing scrollback.

use std::os::fd::BorrowedFd;

use eyre::WrapErr as _;
use std::io::Write as _;

/// Open scrollback content in the configured editor.
/// This function temporarily restores the terminal to cooked mode.
///
/// If `cursor_line` is provided, the editor will open at that line number.
pub fn open_scrollback_in_editor(
    scrollback_content: &str,
    editor_cmd: &str,
    orig_termios: Option<&nix::sys::termios::Termios>,
    cursor_line: Option<usize>,
) -> eyre::Result<()> {
    // Create temp file with scrollback content
    let mut temp_file = tempfile::NamedTempFile::new()
        .wrap_err("failed to create temporary file for scrollback")?;
    temp_file
        .write_all(scrollback_content.as_bytes())
        .wrap_err("failed to write scrollback to temporary file")?;
    temp_file
        .flush()
        .wrap_err("failed to flush temporary file")?;
    let temp_path = temp_file.path().to_owned();

    // Restore terminal to cooked mode if we have original termios
    let stdin_fd = unsafe { BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) };
    if let Some(termios) = orig_termios {
        let _ = nix::sys::termios::tcsetattr(stdin_fd, nix::sys::termios::SetArg::TCSANOW, termios);
    }

    // Parse editor command and spawn
    let parts: Vec<&str> = editor_cmd.split_whitespace().collect();
    let (cmd, args) = parts
        .split_first()
        .ok_or_else(|| eyre::eyre!("empty editor command — set $EDITOR or configure tap"))?;

    let mut command = std::process::Command::new(cmd);
    command.args(args.iter().copied());

    // Add line number argument for vim/nvim (uses +{line} syntax)
    if let Some(line) = cursor_line {
        let cmd_name = std::path::Path::new(cmd)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(cmd);
        if matches!(cmd_name, "vim" | "nvim" | "vi") {
            command.arg(format!("+{line}"));
        }
    }

    command.arg(&temp_path);
    let status = command
        .status()
        .wrap_err_with(|| format!("failed to spawn editor '{cmd}'"))?;

    if !status.success() {
        tracing::warn!("editor exited with status: {status}");
    }

    // Restore raw mode
    let stdin_fd = unsafe { BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) };
    let mut raw = nix::sys::termios::tcgetattr(stdin_fd)
        .wrap_err("failed to get terminal attributes after editor")?;
    nix::sys::termios::cfmakeraw(&mut raw);
    nix::sys::termios::tcsetattr(stdin_fd, nix::sys::termios::SetArg::TCSANOW, &raw)
        .wrap_err("failed to restore raw terminal mode")?;

    // Temp file is automatically deleted when temp_file drops
    Ok(())
}
