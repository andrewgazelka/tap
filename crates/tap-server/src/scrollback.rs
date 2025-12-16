const DEFAULT_SCROLLBACK_LINES: usize = 10000;
const DEFAULT_TERMINAL_ROWS: u16 = 24;
const DEFAULT_TERMINAL_COLS: u16 = 80;

/// A scrollback buffer backed by vt100 terminal emulator.
///
/// Handles alternate screen mode properly - when a TUI app enters alternate
/// screen, we preserve the main screen scrollback and combine it with the
/// alternate screen content when reading.
pub struct ScrollbackBuffer {
    parser: Option<vt100::Parser>,
    max_lines: usize,
    /// Saved main screen content when entering alternate screen mode
    saved_main_content: Option<String>,
    /// Track previous alternate screen state to detect transitions
    was_alternate: bool,
}

impl ScrollbackBuffer {
    pub const fn new() -> Self {
        Self {
            parser: None,
            max_lines: DEFAULT_SCROLLBACK_LINES,
            saved_main_content: None,
            was_alternate: false,
        }
    }

    fn ensure_parser(&mut self) -> &mut vt100::Parser {
        self.parser.get_or_insert_with(|| {
            vt100::Parser::new(DEFAULT_TERMINAL_ROWS, DEFAULT_TERMINAL_COLS, self.max_lines)
        })
    }

    pub fn push(&mut self, data: &[u8]) {
        // Check if we need to save main screen before processing
        // (in case this data switches to alternate screen)
        let was_alternate_before = self.parser.as_ref().is_some_and(|p| p.screen().alternate_screen());

        self.ensure_parser().process(data);

        let is_alternate_now = self.parser.as_ref().is_some_and(|p| p.screen().alternate_screen());

        // Detect transition from main -> alternate screen
        if !was_alternate_before && is_alternate_now {
            // We just entered alternate screen - but the content has already been
            // replaced. We need to save BEFORE the switch happens.
            // Unfortunately vt100 doesn't give us a hook for this, so we need to
            // save content continuously while in main screen mode.
            tracing::debug!("entered alternate screen mode");
        }

        // Detect transition from alternate -> main screen
        if was_alternate_before && !is_alternate_now {
            // We just exited alternate screen - clear saved content
            self.saved_main_content = None;
            tracing::debug!("exited alternate screen mode");
        }

        // If we're in main screen mode, keep saving content for potential alternate switch
        if !is_alternate_now {
            if let Some(parser) = &self.parser {
                self.saved_main_content = Some(parser.screen().contents());
            }
        }

        self.was_alternate = is_alternate_now;
    }

    pub fn get_lines(&self, count: Option<usize>) -> String {
        let Some(parser) = &self.parser else {
            return String::new();
        };

        let screen = parser.screen();
        let is_alternate = screen.alternate_screen();

        // If in alternate screen and we have saved main content, combine them
        let all_contents = if is_alternate {
            if let Some(saved) = &self.saved_main_content {
                // Combine: saved main scrollback + current alternate screen
                let alternate_content = screen.contents();
                format!("{saved}\n--- alternate screen ---\n{alternate_content}")
            } else {
                screen.contents()
            }
        } else {
            screen.contents()
        };

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

    /// Returns true if currently in alternate screen mode
    pub fn is_alternate_screen(&self) -> bool {
        self.parser.as_ref().is_some_and(|p| p.screen().alternate_screen())
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

    #[test]
    fn test_alternate_screen_detection() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"main screen content");
        assert!(!buf.is_alternate_screen());

        // Enter alternate screen mode
        buf.push(b"\x1b[?1049h");
        assert!(buf.is_alternate_screen());

        // Exit alternate screen mode
        buf.push(b"\x1b[?1049l");
        assert!(!buf.is_alternate_screen());
    }

    #[test]
    fn test_alternate_screen_preserves_main_content() {
        let mut buf = ScrollbackBuffer::new();
        buf.push(b"line1\r\nline2\r\nline3");

        // Enter alternate screen
        buf.push(b"\x1b[?1049h");
        buf.push(b"alternate content");

        // Should contain both main and alternate content
        let content = buf.get_lines(None);
        assert!(content.contains("line1"));
        assert!(content.contains("line2"));
        assert!(content.contains("alternate content"));
    }
}
