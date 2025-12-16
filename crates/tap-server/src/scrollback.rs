const DEFAULT_SCROLLBACK_LINES: usize = 10000;
const DEFAULT_TERMINAL_ROWS: u16 = 24;
const DEFAULT_TERMINAL_COLS: u16 = 80;

/// A scrollback buffer backed by vt100 terminal emulator.
pub struct ScrollbackBuffer {
    parser: Option<vt100::Parser>,
    max_lines: usize,
}

impl ScrollbackBuffer {
    pub const fn new() -> Self {
        Self {
            parser: None,
            max_lines: DEFAULT_SCROLLBACK_LINES,
        }
    }

    fn ensure_parser(&mut self) -> &mut vt100::Parser {
        self.parser.get_or_insert_with(|| {
            vt100::Parser::new(DEFAULT_TERMINAL_ROWS, DEFAULT_TERMINAL_COLS, self.max_lines)
        })
    }

    pub fn push(&mut self, data: &[u8]) {
        self.ensure_parser().process(data);
    }

    pub fn get_lines(&self, count: Option<usize>) -> String {
        let Some(parser) = &self.parser else {
            return String::new();
        };

        let screen = parser.screen();

        // Just return current screen contents - vt100 handles alternate screen internally
        let all_contents = screen.contents();

        match count {
            Some(n) => {
                let lines: Vec<&str> = all_contents.lines().collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            }
            None => all_contents,
        }
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        let Some(parser) = &self.parser else {
            return (0, 0);
        };

        let screen = parser.screen();
        (
            screen.cursor_position().0 as usize,
            screen.cursor_position().1 as usize,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_simple_text() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"hello world");
        assert_eq!(buf.get_lines(None).trim(), "hello world");
    }

    #[test]
    fn test_push_with_newlines() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"line1\r\nline2\r\nline3");
        let content = buf.get_lines(None);
        assert!(content.contains("line1"));
        assert!(content.contains("line2"));
        assert!(content.contains("line3"));
    }

    #[test]
    fn test_get_last_n_lines() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"line1\r\nline2\r\nline3\r\nline4\r\n");
        let last_two = buf.get_lines(Some(2));
        assert!(last_two.contains("line3") || last_two.contains("line4"));
    }

    #[test]
    fn test_cursor_position() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"hello\r\nworld");
        let (row, col) = buf.cursor_position();
        assert_eq!(row, 1);
        assert_eq!(col, 5);
    }

    #[test]
    fn test_strips_ansi_escapes() {
        let mut buf = ScrollbackBuffer::new();
        // Color escape sequence for red text
        buf.push(b"\x1b[31mred text\x1b[0m");
        let content = buf.get_lines(None);
        // Should contain the text but not raw escape sequences
        assert!(content.contains("red text"));
        assert!(!content.contains("\x1b[31m"));
        assert!(!content.contains("[31m"));
    }

    // =========================================================================
    // Alternate screen integration tests (simulating vim/vi behavior)
    // =========================================================================

    /// When entering alternate screen, only alternate content should be visible
    #[test]
    fn test_alternate_screen_shows_only_alternate_content() {
        let mut buf = ScrollbackBuffer::new();

        // Write content to main screen
        buf.push(b"main line 1\r\nmain line 2\r\nmain line 3");

        // Enter alternate screen (like vim does)
        buf.push(b"\x1b[?1049h");

        // Write content to alternate screen
        buf.push(b"alternate content here");

        // Should show ONLY alternate content, NOT main content
        let content = buf.get_lines(None);
        assert!(
            content.contains("alternate content here"),
            "should contain alternate content"
        );
        assert!(
            !content.contains("main line 1"),
            "should NOT contain main screen content when in alternate mode"
        );
        assert!(
            !content.contains("main line 2"),
            "should NOT contain main screen content when in alternate mode"
        );
    }

    /// Simulates vim workflow: enter alternate, show file, exit, restore main
    #[test]
    fn test_vim_like_workflow() {
        let mut buf = ScrollbackBuffer::new();

        // User types some commands in shell
        buf.push(b"$ ls -la\r\nfile1.txt\r\nfile2.txt\r\n$ vim file1.txt\r\n");

        // Vim enters alternate screen
        buf.push(b"\x1b[?1049h");

        // Vim shows file content (simplified)
        buf.push(b"Hello from file1.txt\r\nThis is vim editing mode");

        // While in vim, should only see vim content
        let content_in_vim = buf.get_lines(None);
        assert!(content_in_vim.contains("Hello from file1.txt"));
        assert!(
            !content_in_vim.contains("$ ls -la"),
            "shell history should not be visible while in vim"
        );

        // User exits vim (:q)
        buf.push(b"\x1b[?1049l");

        // Back to main screen, should see original shell content
        let content_after_vim = buf.get_lines(None);
        assert!(
            content_after_vim.contains("$ ls -la"),
            "shell history should be restored after exiting vim"
        );
        assert!(
            content_after_vim.contains("file1.txt"),
            "ls output should be visible after exiting vim"
        );
    }

    /// Test multiple alternate screen enter/exit cycles
    #[test]
    fn test_multiple_alternate_screen_cycles() {
        let mut buf = ScrollbackBuffer::new();

        // Initial main screen content
        buf.push(b"session start\r\n");

        for i in 1..=3 {
            // Enter alternate screen
            buf.push(b"\x1b[?1049h");
            buf.push(format!("editor session {i}").as_bytes());

            // Should only see current editor session
            let in_alt = buf.get_lines(None);
            assert!(in_alt.contains(&format!("editor session {i}")));
            assert!(!in_alt.contains("session start"));

            // Exit alternate screen
            buf.push(b"\x1b[?1049l");

            // Should be back to main
            let in_main = buf.get_lines(None);
            assert!(in_main.contains("session start"));
        }
    }

    /// Test that alternate screen content is isolated
    #[test]
    fn test_alternate_screen_isolation() {
        let mut buf = ScrollbackBuffer::new();

        // Main screen has scrollback history
        for i in 1..=50 {
            buf.push(format!("history line {i}\r\n").as_bytes());
        }

        // Enter alternate screen (TUI app)
        buf.push(b"\x1b[?1049h");
        buf.push(b"TUI application interface");

        // Alt-e equivalent: get_lines should return ONLY the TUI content
        let content = buf.get_lines(None);
        assert!(content.contains("TUI application interface"));
        assert!(
            !content.contains("history line 1"),
            "scrollback history should not leak into alternate screen view"
        );
        assert!(
            !content.contains("history line 50"),
            "scrollback history should not leak into alternate screen view"
        );
    }
}
