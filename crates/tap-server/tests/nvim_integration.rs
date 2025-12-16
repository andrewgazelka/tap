//! Integration tests for nvim behavior with tap's scrollback capture.
//!
//! These tests spawn actual nvim processes to verify that tap correctly captures
//! terminal content, including alternate screen mode behavior.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::time::Duration;

/// Helper to spawn a PTY and run commands in it.
struct PtySession {
    master: std::fs::File,
    parser: vt100::Parser,
    _child: nix::unistd::Pid,
}

impl PtySession {
    fn spawn(command: &[&str]) -> eyre::Result<Self> {
        let ws = nix::pty::Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let nix::pty::OpenptyResult { master, slave } =
            nix::pty::openpty(Some(&ws), None).map_err(|e| eyre::eyre!("openpty failed: {e}"))?;

        let child_pid = match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Child) => {
                drop(master);

                nix::unistd::setsid().expect("setsid failed");

                unsafe {
                    nix::libc::ioctl(slave.as_raw_fd(), nix::libc::TIOCSCTTY as _, 0);
                }

                let slave_raw = slave.as_raw_fd();
                unsafe {
                    nix::libc::dup2(slave_raw, nix::libc::STDIN_FILENO);
                    nix::libc::dup2(slave_raw, nix::libc::STDOUT_FILENO);
                    nix::libc::dup2(slave_raw, nix::libc::STDERR_FILENO);
                }

                if slave_raw > 2 {
                    drop(slave);
                }

                // Set TERM for proper terminal behavior
                // SAFETY: We're in a forked child process before exec, no other threads exist
                unsafe { std::env::set_var("TERM", "xterm-256color") };

                let c_cmd: Vec<std::ffi::CString> = command
                    .iter()
                    .map(|s| std::ffi::CString::new(*s).unwrap())
                    .collect();

                nix::unistd::execvp(&c_cmd[0], &c_cmd).expect("execvp failed");
                unreachable!()
            }
            Ok(nix::unistd::ForkResult::Parent { child }) => child,
            Err(e) => return Err(eyre::eyre!("fork failed: {e}")),
        };

        drop(slave);

        let master_file = unsafe { std::fs::File::from_raw_fd(master.as_raw_fd()) };
        std::mem::forget(master);

        // Set non-blocking mode
        unsafe {
            let flags = nix::libc::fcntl(master_file.as_raw_fd(), nix::libc::F_GETFL);
            nix::libc::fcntl(
                master_file.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            );
        }

        Ok(Self {
            master: master_file,
            parser: vt100::Parser::new(24, 80, 10000),
            _child: child_pid,
        })
    }

    /// Read available output and process it through the vt100 parser.
    fn read_output(&mut self) -> eyre::Result<()> {
        let mut buf = [0u8; 4096];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.parser.process(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(eyre::eyre!("read error: {e}")),
            }
        }
        Ok(())
    }

    /// Wait for output to settle (no new output for the given duration).
    fn wait_for_output(&mut self, timeout: Duration) -> eyre::Result<()> {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(50);

        loop {
            std::thread::sleep(check_interval);
            self.read_output()?;

            if start.elapsed() > timeout {
                break;
            }
        }
        Ok(())
    }

    /// Send input to the PTY.
    fn send(&mut self, data: &[u8]) -> eyre::Result<()> {
        self.master
            .write_all(data)
            .map_err(|e| eyre::eyre!("write error: {e}"))?;
        self.master
            .flush()
            .map_err(|e| eyre::eyre!("flush error: {e}"))?;
        Ok(())
    }

    /// Send keys to nvim.
    fn send_keys(&mut self, keys: &str) -> eyre::Result<()> {
        self.send(keys.as_bytes())
    }

    /// Get current screen contents.
    fn screen_contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Check if we're in alternate screen mode.
    fn is_alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Close the session.
    fn close(mut self) -> eyre::Result<()> {
        // Send :q! to exit nvim
        let _ = self.send(b"\x1b:q!\r");
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}

/// Test that nvim with --clean enters alternate screen and shows file content.
#[test]
#[ignore] // Requires nvim to be installed
fn test_nvim_clean_shows_file_content() {
    // Create a temp file with known content
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    let test_content =
        "Line 1: Hello from test file\nLine 2: This is test content\nLine 3: More content here\n";
    std::io::Write::write_all(&mut temp_file, test_content.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    // Spawn nvim with --clean (no plugins, no config)
    let mut session =
        PtySession::spawn(&["nvim", "--clean", "-u", "NONE", temp_path]).expect("spawn failed");

    // Wait for nvim to start
    session
        .wait_for_output(Duration::from_millis(500))
        .expect("wait failed");

    // Verify we're in alternate screen mode
    assert!(
        session.is_alternate_screen(),
        "nvim should enter alternate screen mode"
    );

    // Get screen contents
    let contents = session.screen_contents();

    // Verify the file content is visible (without Telescope or other overlays)
    assert!(
        contents.contains("Line 1: Hello from test file"),
        "screen should contain file content line 1, got: {}",
        contents
    );
    assert!(
        contents.contains("Line 2: This is test content"),
        "screen should contain file content line 2, got: {}",
        contents
    );
    assert!(
        contents.contains("Line 3: More content here"),
        "screen should contain file content line 3, got: {}",
        contents
    );

    // Verify no Telescope/plugin UI elements
    assert!(
        !contents.contains("Find File"),
        "screen should NOT contain Telescope UI elements"
    );
    assert!(
        !contents.contains("Recent Files"),
        "screen should NOT contain Telescope UI elements"
    );

    session.close().expect("close failed");
}

/// Test alternate screen entry and exit behavior.
#[test]
#[ignore] // Requires nvim to be installed
fn test_nvim_alternate_screen_lifecycle() {
    let mut session = PtySession::spawn(&["nvim", "--clean", "-u", "NONE"]).expect("spawn failed");

    // Wait for nvim to start
    session
        .wait_for_output(Duration::from_millis(500))
        .expect("wait failed");

    // Should be in alternate screen
    assert!(
        session.is_alternate_screen(),
        "nvim should be in alternate screen"
    );

    // Exit nvim
    session.send_keys("\x1b:q!\r").expect("send failed");

    // Wait for nvim to fully exit and send alternate screen exit sequence
    // Need to keep reading until alternate screen is exited or timeout
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while start.elapsed() < timeout {
        session.read_output().expect("read failed");
        if !session.is_alternate_screen() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Should exit alternate screen
    assert!(
        !session.is_alternate_screen(),
        "should exit alternate screen after quitting nvim"
    );
}

/// Test that scrollback buffer correctly captures content when in alternate screen.
#[test]
#[ignore] // Requires nvim to be installed
fn test_scrollback_captures_alternate_screen_content() {
    use tap_server::scrollback::ScrollbackBuffer;

    // Create a temp file with known content
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    let test_content = "Test line alpha\nTest line beta\nTest line gamma\n";
    std::io::Write::write_all(&mut temp_file, test_content.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    // Spawn nvim
    let mut session =
        PtySession::spawn(&["nvim", "--clean", "-u", "NONE", temp_path]).expect("spawn failed");

    // Use a ScrollbackBuffer to capture what we'd get in tap
    let mut scrollback = ScrollbackBuffer::new();

    // Read output and feed to scrollback
    std::thread::sleep(Duration::from_millis(500));

    let mut buf = [0u8; 4096];
    loop {
        match session.master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                scrollback.push(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("read error: {e}"),
        }
    }

    // Get scrollback content
    let content = scrollback.get_lines(None);

    // Should contain file content, not shell history
    assert!(
        content.contains("Test line alpha"),
        "scrollback should capture alternate screen content: {}",
        content
    );

    session.close().expect("close failed");
}
