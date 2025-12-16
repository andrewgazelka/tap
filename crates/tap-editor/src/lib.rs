//! Editor integration utilities.
//!
//! Handles different editor command-line argument formats for opening files
//! at specific line/column positions.

use std::path::Path;

/// Known editor types with their argument formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    /// vim, nvim, vi: `+{line}` before file
    Vim,
    /// VSCode, Cursor: `-g {file}:{line}:{col}`
    VsCode,
    /// nano: `+{line},{col}` before file
    Nano,
    /// emacs: `+{line}:{col}` before file
    Emacs,
    /// helix: `{file}:{line}`
    Helix,
    /// Unknown editor, no line number support
    Unknown,
}

impl EditorKind {
    /// Detect editor kind from command name or path.
    pub fn detect(cmd: &str) -> Self {
        let name = Path::new(cmd)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(cmd);

        match name {
            "vim" | "nvim" | "vi" | "view" | "vimdiff" => Self::Vim,
            "code" | "cursor" | "code-insiders" | "codium" | "vscodium" => Self::VsCode,
            "nano" | "pico" => Self::Nano,
            "emacs" | "emacsclient" => Self::Emacs,
            "hx" | "helix" => Self::Helix,
            _ => Self::Unknown,
        }
    }
}

/// Position in a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Position {
    /// 1-indexed line number
    pub line: usize,
    /// 1-indexed column number (optional)
    pub col: Option<usize>,
}

impl Position {
    pub fn new(line: usize, col: Option<usize>) -> Self {
        Self { line, col }
    }

    pub fn line(line: usize) -> Self {
        Self { line, col: None }
    }
}

/// Build command arguments for opening a file at a position.
///
/// Returns (args_before_file, file_arg) where:
/// - `args_before_file`: arguments to add before the file path
/// - `file_arg`: the file argument (may include line number for some editors)
pub fn build_editor_args(
    editor_cmd: &str,
    file_path: &Path,
    pos: Option<Position>,
) -> (Vec<String>, String) {
    let kind = EditorKind::detect(editor_cmd);
    let file_str = file_path.display().to_string();

    let Some(pos) = pos else {
        return (vec![], file_str);
    };

    match kind {
        EditorKind::Vim => {
            // vim +42 file.txt
            (vec![format!("+{}", pos.line)], file_str)
        }
        EditorKind::VsCode => {
            // code -g file.txt:42:10
            let col = pos.col.unwrap_or(1);
            (vec!["-g".to_string()], format!("{file_str}:{}:{col}", pos.line))
        }
        EditorKind::Nano => {
            // nano +42,10 file.txt
            let arg = match pos.col {
                Some(col) => format!("+{},{col}", pos.line),
                None => format!("+{}", pos.line),
            };
            (vec![arg], file_str)
        }
        EditorKind::Emacs => {
            // emacs +42:10 file.txt
            let arg = match pos.col {
                Some(col) => format!("+{}:{col}", pos.line),
                None => format!("+{}", pos.line),
            };
            (vec![arg], file_str)
        }
        EditorKind::Helix => {
            // hx file.txt:42
            (vec![], format!("{file_str}:{}", pos.line))
        }
        EditorKind::Unknown => (vec![], file_str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_vim() {
        assert_eq!(EditorKind::detect("vim"), EditorKind::Vim);
        assert_eq!(EditorKind::detect("nvim"), EditorKind::Vim);
        assert_eq!(EditorKind::detect("/usr/bin/vim"), EditorKind::Vim);
        assert_eq!(EditorKind::detect("/opt/homebrew/bin/nvim"), EditorKind::Vim);
    }

    #[test]
    fn test_detect_vscode() {
        assert_eq!(EditorKind::detect("code"), EditorKind::VsCode);
        assert_eq!(EditorKind::detect("cursor"), EditorKind::VsCode);
        assert_eq!(EditorKind::detect("/usr/local/bin/code"), EditorKind::VsCode);
    }

    #[test]
    fn test_detect_others() {
        assert_eq!(EditorKind::detect("nano"), EditorKind::Nano);
        assert_eq!(EditorKind::detect("emacs"), EditorKind::Emacs);
        assert_eq!(EditorKind::detect("hx"), EditorKind::Helix);
        assert_eq!(EditorKind::detect("unknown-editor"), EditorKind::Unknown);
    }

    #[test]
    fn test_vim_args() {
        let (args, file) = build_editor_args("vim", Path::new("/tmp/test.txt"), Some(Position::line(42)));
        assert_eq!(args, vec!["+42"]);
        assert_eq!(file, "/tmp/test.txt");
    }

    #[test]
    fn test_vscode_args() {
        let (args, file) = build_editor_args(
            "cursor",
            Path::new("/tmp/test.txt"),
            Some(Position::new(42, Some(10))),
        );
        assert_eq!(args, vec!["-g"]);
        assert_eq!(file, "/tmp/test.txt:42:10");
    }

    #[test]
    fn test_helix_args() {
        let (args, file) = build_editor_args("hx", Path::new("/tmp/test.txt"), Some(Position::line(42)));
        assert!(args.is_empty());
        assert_eq!(file, "/tmp/test.txt:42");
    }

    #[test]
    fn test_nano_args() {
        let (args, file) = build_editor_args(
            "nano",
            Path::new("/tmp/test.txt"),
            Some(Position::new(42, Some(5))),
        );
        assert_eq!(args, vec!["+42,5"]);
        assert_eq!(file, "/tmp/test.txt");
    }

    #[test]
    fn test_no_position() {
        let (args, file) = build_editor_args("vim", Path::new("/tmp/test.txt"), None);
        assert!(args.is_empty());
        assert_eq!(file, "/tmp/test.txt");
    }
}
