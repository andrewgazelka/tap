# tap

Terminal introspection tool that wraps shells and captures scrollback.

## Architecture

- `tap-server`: Core PTY wrapper library
- `tap`: CLI binary
- `tap-protocol`: IPC protocol for communicating with tap sessions
- `tap-config`: Configuration loading

## Reference Implementation

Zellij is cloned at `~/Projects/zellij` and serves as a reference for:
- Kitty keyboard protocol handling (`zellij-client/src/keyboard_parser.rs`)
- PTY input/output management (`zellij-server/src/panes/terminal_pane.rs`)
- Terminal grid state tracking (`zellij-server/src/panes/grid.rs`)

## Key Files

- `crates/tap-server/src/lib.rs` - Main PTY loop, I/O handling
- `crates/tap-server/src/kitty.rs` - Kitty keyboard protocol translation
- `crates/tap-server/src/input.rs` - Input processing and keybind detection
- `crates/tap-server/src/scrollback.rs` - Terminal scrollback buffer using vt100

## Testing

```bash
cargo test
cargo run -p tap
```
