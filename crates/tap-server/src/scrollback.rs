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
}
