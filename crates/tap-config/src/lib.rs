//! Configuration for tap terminal sessions.

/// Main configuration structure.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Config {
    /// Editor to use for the edit command.
    /// Falls back to $EDITOR, then $VISUAL, then "vi".
    pub editor: Option<String>,

    /// Keybind configuration.
    pub keybinds: KeybindConfig,

    /// Timing configuration.
    pub timing: TimingConfig,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct KeybindConfig {
    /// Keybind to open scrollback in editor.
    /// Format: "Alt-e", "Ctrl-e", etc.
    pub editor: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct TimingConfig {
    /// Timeout in milliseconds to distinguish ESC from Alt-key sequences.
    pub escape_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            editor: None,
            keybinds: KeybindConfig::default(),
            timing: TimingConfig::default(),
        }
    }
}

impl Default for KeybindConfig {
    fn default() -> Self {
        Self {
            editor: "Alt-e".to_string(),
        }
    }
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            escape_timeout_ms: 50,
        }
    }
}

/// Returns the config file path: ~/.config/tap/config.toml
#[must_use]
pub fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
        .join("tap")
        .join("config.toml")
}

/// Load configuration from default path, falling back to defaults if not found.
pub fn load() -> eyre::Result<Config> {
    let path = config_path();
    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    } else {
        Ok(Config::default())
    }
}

/// Get the effective editor command.
#[must_use]
pub fn get_editor(config: &Config) -> String {
    config
        .editor
        .clone()
        .or_else(|| std::env::var("EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
        .unwrap_or_else(|| "vi".to_string())
}

/// Parsed keybind representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keybind {
    Alt(char),
    Ctrl(char),
}

impl Keybind {
    /// Parse a keybind string like "Alt-e" or "Ctrl-e".
    pub fn parse(s: &str) -> eyre::Result<Self> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 2 {
            eyre::bail!("Invalid keybind format: {s}");
        }
        let modifier = parts[0].to_lowercase();
        let key = parts[1]
            .chars()
            .next()
            .ok_or_else(|| eyre::eyre!("Missing key in keybind: {s}"))?;

        match modifier.as_str() {
            "alt" => Ok(Keybind::Alt(key)),
            "ctrl" => Ok(Keybind::Ctrl(key.to_ascii_lowercase())),
            _ => eyre::bail!("Unknown modifier: {modifier}"),
        }
    }

    /// Check if this keybind matches the given bytes.
    /// Returns the number of bytes consumed if matched, None otherwise.
    /// Supports both legacy terminal sequences and Kitty keyboard protocol.
    #[must_use]
    pub fn matches(&self, bytes: &[u8]) -> Option<usize> {
        // First try Kitty keyboard protocol: CSI <codepoint>;<modifiers>u
        if let Some(consumed) = self.matches_kitty(bytes) {
            return Some(consumed);
        }

        // Fall back to legacy sequences
        match self {
            Keybind::Alt(c) => {
                // Alt-key is ESC followed by the character
                if bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1] == *c as u8 {
                    Some(2)
                } else {
                    None
                }
            }
            Keybind::Ctrl(c) => {
                // Ctrl-key is the character with upper bits cleared
                let ctrl_byte = (*c as u8) & 0x1f;
                if !bytes.is_empty() && bytes[0] == ctrl_byte {
                    Some(1)
                } else {
                    None
                }
            }
        }
    }

    /// Match Kitty keyboard protocol sequences: CSI <codepoint>;<modifiers>u
    /// Modifiers: 1=none, 2=shift, 3=alt, 4=shift+alt, 5=ctrl, etc.
    fn matches_kitty(&self, bytes: &[u8]) -> Option<usize> {
        // Must start with CSI (ESC [)
        if bytes.len() < 4 || bytes[0] != 0x1b || bytes[1] != b'[' {
            return None;
        }

        // Find the 'u' terminator
        let u_pos = bytes.iter().position(|&b| b == b'u')?;
        if u_pos < 3 {
            return None;
        }

        // Parse the sequence between '[' and 'u'
        let seq = std::str::from_utf8(&bytes[2..u_pos]).ok()?;
        let parts: Vec<&str> = seq.split(';').collect();

        let codepoint: u32 = parts.first()?.parse().ok()?;
        let modifiers: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

        let expected_char = match self {
            Keybind::Alt(c) => *c,
            Keybind::Ctrl(c) => *c,
        };

        if codepoint != expected_char as u32 {
            return None;
        }

        // Check modifiers (encoded as modifier_value + 1)
        // Alt = 2, so Alt modifier = 3
        // Ctrl = 4, so Ctrl modifier = 5
        let matches = match self {
            Keybind::Alt(_) => modifiers == 3,  // Alt only
            Keybind::Ctrl(_) => modifiers == 5, // Ctrl only
        };

        if matches { Some(u_pos + 1) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keybind_parse_alt() {
        let kb = Keybind::parse("Alt-e").unwrap();
        assert_eq!(kb, Keybind::Alt('e'));
    }

    #[test]
    fn test_keybind_parse_ctrl() {
        let kb = Keybind::parse("Ctrl-c").unwrap();
        assert_eq!(kb, Keybind::Ctrl('c'));
    }

    #[test]
    fn test_keybind_matches_alt() {
        let kb = Keybind::Alt('e');
        assert_eq!(kb.matches(&[0x1b, b'e']), Some(2));
        assert_eq!(kb.matches(&[0x1b, b'x']), None);
        assert_eq!(kb.matches(&[0x1b]), None);
    }

    #[test]
    fn test_keybind_matches_ctrl() {
        let kb = Keybind::Ctrl('c');
        // Ctrl-C is 0x03
        assert_eq!(kb.matches(&[0x03]), Some(1));
        assert_eq!(kb.matches(&[0x04]), None);
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.keybinds.editor, "Alt-e");
        assert_eq!(config.timing.escape_timeout_ms, 50);
    }

    #[test]
    fn test_ctrl_e_end_to_end() {
        // Simulate what happens with "Ctrl-e" from config
        let kb = Keybind::parse("Ctrl-e").unwrap();
        assert_eq!(kb, Keybind::Ctrl('e'));
        // Ctrl-e should match byte 0x05
        assert_eq!(kb.matches(&[0x05]), Some(1));
    }

    #[test]
    fn test_kitty_protocol_alt_e() {
        let kb = Keybind::Alt('e');
        // Kitty protocol: CSI 101;3u (Alt-e)
        // 101 = 'e', 3 = Alt modifier
        let kitty_seq = b"\x1b[101;3u";
        assert_eq!(kb.matches(kitty_seq), Some(8));
    }

    #[test]
    fn test_kitty_protocol_ctrl_e() {
        let kb = Keybind::Ctrl('e');
        // Kitty protocol: CSI 101;5u (Ctrl-e)
        // 101 = 'e', 5 = Ctrl modifier
        let kitty_seq = b"\x1b[101;5u";
        assert_eq!(kb.matches(kitty_seq), Some(8));
    }

    #[test]
    fn test_kitty_protocol_wrong_modifier() {
        let kb = Keybind::Alt('e');
        // Wrong modifier (Ctrl instead of Alt)
        let kitty_seq = b"\x1b[101;5u";
        assert_eq!(kb.matches(kitty_seq), None);
    }
}
