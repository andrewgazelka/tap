//! Input processing with keybind detection.

const ESC_BYTE: u8 = 0x1b;

/// Input processor state machine for detecting keybinds.
pub struct InputProcessor {
    keybinds: Vec<(tap_config::Keybind, KeybindAction)>,
    escape_timeout: std::time::Duration,
    pending_escape: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindAction {
    OpenEditor,
}

#[derive(Debug)]
pub enum InputResult {
    /// Pass these bytes through to the PTY.
    Passthrough(Vec<u8>),
    /// A keybind was triggered.
    Action(KeybindAction),
    /// Need more input (waiting for escape timeout).
    NeedMore,
}

impl InputProcessor {
    pub fn new(config: &tap_config::Config) -> eyre::Result<Self> {
        let mut keybinds = Vec::new();

        let editor_keybind = tap_config::Keybind::parse(&config.keybinds.editor)?;
        keybinds.push((editor_keybind, KeybindAction::OpenEditor));

        Ok(Self {
            keybinds,
            escape_timeout: std::time::Duration::from_millis(config.timing.escape_timeout_ms),
            pending_escape: false,
        })
    }

    #[must_use]
    pub fn escape_timeout(&self) -> std::time::Duration {
        self.escape_timeout
    }

    #[must_use]
    pub fn has_pending_escape(&self) -> bool {
        self.pending_escape
    }

    /// Process input bytes, returning what action to take.
    pub fn process(&mut self, bytes: &[u8]) -> InputResult {
        tracing::debug!("Input bytes: {:?} (hex: {:02x?})", bytes, bytes);

        if bytes.is_empty() {
            if self.pending_escape {
                self.pending_escape = false;
                return InputResult::Passthrough(vec![ESC_BYTE]);
            }
            return InputResult::Passthrough(vec![]);
        }

        // Check if we have a pending escape and new input
        let effective_bytes = if self.pending_escape {
            self.pending_escape = false;
            let mut v = vec![ESC_BYTE];
            v.extend_from_slice(bytes);
            v
        } else {
            bytes.to_vec()
        };

        // Check for keybind matches
        for (keybind, action) in &self.keybinds {
            tracing::debug!(
                "Checking keybind {:?} against {:02x?}",
                keybind,
                effective_bytes
            );
            if let Some(consumed) = keybind.matches(&effective_bytes) {
                tracing::debug!("Keybind matched! consumed={}", consumed);
                // If there are remaining bytes after the keybind, we'd need to handle them
                // For now, assume keybinds consume all input in that read
                if consumed == effective_bytes.len() {
                    return InputResult::Action(*action);
                }
                // Partial match with trailing bytes - trigger action, remaining bytes are lost
                // This is acceptable for our use case
                return InputResult::Action(*action);
            }
        }

        // Check if this is just an escape byte that might be start of Alt sequence
        if effective_bytes.len() == 1 && effective_bytes[0] == ESC_BYTE {
            self.pending_escape = true;
            return InputResult::NeedMore;
        }

        InputResult::Passthrough(effective_bytes)
    }

    /// Called when escape timeout expires.
    pub fn timeout_escape(&mut self) -> InputResult {
        if self.pending_escape {
            self.pending_escape = false;
            InputResult::Passthrough(vec![ESC_BYTE])
        } else {
            InputResult::Passthrough(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_processor() -> InputProcessor {
        let config = tap_config::Config::default();
        InputProcessor::new(&config).unwrap()
    }

    #[test]
    fn test_passthrough_normal_input() {
        let mut proc = default_processor();
        match proc.process(b"hello") {
            InputResult::Passthrough(bytes) => assert_eq!(bytes, b"hello"),
            _ => panic!("Expected passthrough"),
        }
    }

    #[test]
    fn test_escape_triggers_pending() {
        let mut proc = default_processor();
        match proc.process(&[ESC_BYTE]) {
            InputResult::NeedMore => {}
            _ => panic!("Expected NeedMore for lone ESC"),
        }
        assert!(proc.has_pending_escape());
    }

    #[test]
    fn test_alt_e_triggers_action() {
        let mut proc = default_processor();
        match proc.process(&[ESC_BYTE, b'e']) {
            InputResult::Action(KeybindAction::OpenEditor) => {}
            _ => panic!("Expected OpenEditor action"),
        }
    }

    #[test]
    fn test_pending_escape_then_e() {
        let mut proc = default_processor();
        // First, lone ESC
        match proc.process(&[ESC_BYTE]) {
            InputResult::NeedMore => {}
            _ => panic!("Expected NeedMore"),
        }
        // Then 'e' arrives
        match proc.process(b"e") {
            InputResult::Action(KeybindAction::OpenEditor) => {}
            _ => panic!("Expected OpenEditor action"),
        }
    }

    #[test]
    fn test_pending_escape_timeout() {
        let mut proc = default_processor();
        proc.process(&[ESC_BYTE]);
        match proc.timeout_escape() {
            InputResult::Passthrough(bytes) => assert_eq!(bytes, vec![ESC_BYTE]),
            _ => panic!("Expected passthrough of ESC"),
        }
        assert!(!proc.has_pending_escape());
    }

    #[test]
    fn test_escape_then_other_key() {
        let mut proc = default_processor();
        proc.process(&[ESC_BYTE]);
        // Non-matching key
        match proc.process(b"x") {
            InputResult::Passthrough(bytes) => assert_eq!(bytes, vec![ESC_BYTE, b'x']),
            _ => panic!("Expected passthrough"),
        }
    }

    #[test]
    fn test_ctrl_e_triggers_action() {
        let mut config = tap_config::Config::default();
        config.keybinds.editor = "Ctrl-e".to_string();
        let mut proc = InputProcessor::new(&config).unwrap();
        // Ctrl-e is 0x05
        match proc.process(&[0x05]) {
            InputResult::Action(KeybindAction::OpenEditor) => {}
            other => panic!("Expected OpenEditor action, got {:?}", other),
        }
    }
}
