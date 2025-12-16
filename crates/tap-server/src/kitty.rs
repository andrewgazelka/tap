//! Kitty keyboard protocol handling.
//!
//! This module handles translation between kitty keyboard protocol CSI u sequences
//! and traditional terminal input, similar to how zellij handles it.

/// Tracks whether the inner application has requested kitty keyboard protocol support.
#[derive(Debug, Default)]
pub struct KittyState {
    /// Whether the inner app has enabled kitty keyboard protocol
    pub inner_supports_kitty: bool,
}

impl KittyState {
    pub const fn new() -> Self {
        Self {
            inner_supports_kitty: false,
        }
    }

    /// Check PTY output for kitty keyboard protocol enable/disable sequences.
    /// Updates internal state accordingly.
    pub fn process_pty_output(&mut self, data: &[u8]) {
        // Look for kitty keyboard protocol sequences in output:
        // - CSI > Pu : push flags (enable if P > 0)
        // - CSI < u  : pop flags (disable)
        // - CSI = Pm : set mode (enable if P > 0)
        // - CSI ? u  : query (we don't need to track this)
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'[' {
                if let Some((enabled, consumed)) = self.parse_kitty_sequence(&data[i..]) {
                    if let Some(e) = enabled {
                        self.inner_supports_kitty = e;
                        tracing::debug!("inner kitty support changed to: {}", e);
                    }
                    i += consumed;
                    continue;
                }
            }
            i += 1;
        }
    }

    /// Parse a kitty keyboard protocol sequence starting at the given position.
    /// Returns (Some(enabled), bytes_consumed) if this is a kitty enable/disable sequence.
    /// Returns (None, bytes_consumed) if this is a kitty sequence but doesn't change state.
    fn parse_kitty_sequence(&self, data: &[u8]) -> Option<(Option<bool>, usize)> {
        // Must start with ESC [
        if data.len() < 3 || data[0] != 0x1b || data[1] != b'[' {
            return None;
        }

        let rest = &data[2..];

        // CSI > Pu - push keyboard mode
        if rest.first() == Some(&b'>') {
            return self.parse_push_sequence(rest);
        }

        // CSI < u - pop keyboard mode
        if rest.first() == Some(&b'<') && rest.get(1) == Some(&b'u') {
            return Some((Some(false), 4)); // ESC [ < u
        }

        // CSI = Pm - set keyboard mode
        if rest.first() == Some(&b'=') {
            return self.parse_set_sequence(rest);
        }

        None
    }

    fn parse_push_sequence(&self, rest: &[u8]) -> Option<(Option<bool>, usize)> {
        // Format: > [number] u
        // rest starts at '>'
        let mut i = 1; // skip '>'
        let mut num = 0u32;
        let mut has_num = false;

        while i < rest.len() {
            match rest[i] {
                b'0'..=b'9' => {
                    num = num * 10 + (rest[i] - b'0') as u32;
                    has_num = true;
                    i += 1;
                }
                b'u' => {
                    // ESC [ > [num] u
                    let enabled = if has_num { num > 0 } else { false };
                    return Some((Some(enabled), 2 + i + 1)); // ESC [ + rest consumed
                }
                b';' | b':' => {
                    // Skip modifiers
                    i += 1;
                }
                _ => break,
            }
        }
        None
    }

    fn parse_set_sequence(&self, rest: &[u8]) -> Option<(Option<bool>, usize)> {
        // Format: = [number] m
        // rest starts at '='
        let mut i = 1; // skip '='
        let mut num = 0u32;
        let mut has_num = false;

        while i < rest.len() {
            match rest[i] {
                b'0'..=b'9' => {
                    num = num * 10 + (rest[i] - b'0') as u32;
                    has_num = true;
                    i += 1;
                }
                b'm' => {
                    // ESC [ = [num] m
                    let enabled = if has_num { num > 0 } else { false };
                    return Some((Some(enabled), 2 + i + 1)); // ESC [ + rest consumed
                }
                b';' | b':' => {
                    // Skip additional params
                    i += 1;
                }
                _ => break,
            }
        }
        None
    }
}

/// Translate a kitty CSI u sequence to traditional terminal input.
/// Returns (translated_bytes, bytes_consumed) if successful.
pub fn translate_csi_u_to_traditional(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    // Format: ESC [ codepoint ; modifiers u
    if data.len() < 4 || data[0] != 0x1b || data[1] != b'[' {
        return None;
    }

    let u_pos = data.iter().position(|&b| b == b'u')?;
    if u_pos < 3 {
        return None;
    }

    // Check this isn't a special kitty sequence (>, <, =, ?)
    if matches!(data[2], b'>' | b'<' | b'=' | b'?') {
        return None;
    }

    let seq = std::str::from_utf8(&data[2..u_pos]).ok()?;
    let parts: Vec<&str> = seq.split(';').collect();

    let codepoint: u32 = parts.first()?.parse().ok()?;
    let modifiers: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

    // Modifiers: value is (actual_modifiers + 1)
    // bit 0 (1): shift
    // bit 1 (2): alt
    // bit 2 (4): ctrl
    // bit 3 (8): super
    let mod_bits = modifiers.saturating_sub(1);
    let has_shift = mod_bits & 1 != 0;
    let has_alt = mod_bits & 2 != 0;
    let has_ctrl = mod_bits & 4 != 0;

    let mut result = Vec::new();
    let consumed = u_pos + 1;

    // Handle special keys
    match codepoint {
        27 => {
            // ESC
            result.push(0x1b);
            return Some((result, consumed));
        }
        13 => {
            // Enter
            if has_alt {
                result.push(0x1b);
            }
            result.push(0x0d);
            return Some((result, consumed));
        }
        9 => {
            // Tab
            if has_alt {
                result.push(0x1b);
            }
            if has_shift {
                // Shift+Tab is typically ESC [ Z
                result.clear();
                if has_alt {
                    result.push(0x1b);
                }
                result.extend_from_slice(b"\x1b[Z");
            } else {
                result.push(0x09);
            }
            return Some((result, consumed));
        }
        127 => {
            // Backspace
            if has_alt {
                result.push(0x1b);
            }
            if has_ctrl {
                result.push(0x08); // Ctrl+Backspace often sends BS
            } else {
                result.push(0x7f);
            }
            return Some((result, consumed));
        }
        _ => {}
    }

    // Handle letter keys (a-z, A-Z)
    let is_letter =
        (0x41..=0x5a).contains(&codepoint) || (0x61..=0x7a).contains(&codepoint);
    if is_letter {
        let c = codepoint as u8;
        if has_ctrl {
            // Ctrl+letter -> control character (0x01-0x1a)
            let ctrl_char = c.to_ascii_lowercase() & 0x1f;
            if has_alt {
                result.push(0x1b);
            }
            result.push(ctrl_char);
            return Some((result, consumed));
        } else if has_alt {
            // Alt+letter -> ESC + letter
            result.push(0x1b);
            let letter = if has_shift {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            result.push(letter);
            return Some((result, consumed));
        }
    }

    // Handle other printable ASCII (digits, symbols)
    if codepoint < 128 {
        let c = codepoint as u8;
        if has_ctrl {
            // Some Ctrl combinations have special meanings
            // Ctrl+[ = ESC, Ctrl+\ = FS, Ctrl+] = GS, Ctrl+^ = RS, Ctrl+_ = US
            // Ctrl+@ = NUL, Ctrl+2 = NUL, Ctrl+6 = RS
            if c == b'[' {
                result.push(0x1b);
            } else if c == b'\\' {
                result.push(0x1c);
            } else if c == b']' {
                result.push(0x1d);
            } else if c == b'^' || c == b'6' {
                result.push(0x1e);
            } else if c == b'_' || c == b'-' {
                result.push(0x1f);
            } else if c == b'@' || c == b'2' {
                result.push(0x00);
            } else {
                // For other chars, just pass through with alt prefix if needed
                if has_alt {
                    result.push(0x1b);
                }
                result.push(c);
            }
            return Some((result, consumed));
        } else if has_alt {
            result.push(0x1b);
            result.push(c);
            return Some((result, consumed));
        } else {
            // Plain key, just pass through
            result.push(c);
            return Some((result, consumed));
        }
    }

    // For keys we can't translate, return None to pass through raw
    None
}

/// Translate all CSI u sequences in a buffer to traditional format.
/// Returns the translated buffer.
pub fn translate_all_csi_u(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        // Check if this looks like a CSI u sequence
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'[' {
            if let Some((translated, consumed)) = translate_csi_u_to_traditional(&data[i..]) {
                result.extend(translated);
                i += consumed;
                continue;
            }
        }
        result.push(data[i]);
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_ctrl_c() {
        // CSI 99 ; 5 u = Ctrl+C (codepoint 99 = 'c', modifier 5 = ctrl)
        let input = b"\x1b[99;5u";
        let (translated, consumed) = translate_csi_u_to_traditional(input).unwrap();
        assert_eq!(translated, vec![0x03]); // Ctrl+C
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_translate_ctrl_d() {
        let input = b"\x1b[100;5u";
        let (translated, consumed) = translate_csi_u_to_traditional(input).unwrap();
        assert_eq!(translated, vec![0x04]); // Ctrl+D
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_translate_alt_e() {
        // CSI 101 ; 3 u = Alt+E (codepoint 101 = 'e', modifier 3 = alt)
        let input = b"\x1b[101;3u";
        let (translated, consumed) = translate_csi_u_to_traditional(input).unwrap();
        assert_eq!(translated, vec![0x1b, b'e']); // ESC e
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_translate_plain_a() {
        let input = b"\x1b[97u";
        let (translated, consumed) = translate_csi_u_to_traditional(input).unwrap();
        assert_eq!(translated, vec![b'a']);
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_translate_enter() {
        let input = b"\x1b[13u";
        let (translated, consumed) = translate_csi_u_to_traditional(input).unwrap();
        assert_eq!(translated, vec![0x0d]);
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_kitty_state_push() {
        let mut state = KittyState::new();
        assert!(!state.inner_supports_kitty);

        // Push with flags > 0 enables
        state.process_pty_output(b"\x1b[>1u");
        assert!(state.inner_supports_kitty);

        // Pop disables
        state.process_pty_output(b"\x1b[<u");
        assert!(!state.inner_supports_kitty);
    }

    #[test]
    fn test_kitty_state_set() {
        let mut state = KittyState::new();

        // Set mode with value > 0 enables
        state.process_pty_output(b"\x1b[=1m");
        assert!(state.inner_supports_kitty);

        // Set mode with value 0 disables
        state.process_pty_output(b"\x1b[=0m");
        assert!(!state.inner_supports_kitty);
    }

    #[test]
    fn test_translate_all() {
        let input = b"hello\x1b[99;5uworld";
        let result = translate_all_csi_u(input);
        assert_eq!(result, b"hello\x03world");
    }
}
