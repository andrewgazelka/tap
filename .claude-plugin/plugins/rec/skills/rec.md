---
description: Monitor and interact with other terminal windows running rec sessions. Use when you need to see what's happening in another terminal, check server output, or inject commands into a recorded session.
---

# rec - Terminal Recording and Introspection

`rec` lets you see and control other terminal windows. When a user runs `rec` in another terminal, you can monitor its output, check cursor position, and even inject commands.

## Prerequisites

The user must have `rec` running in another terminal:
```sh
rec                  # Start recording with default shell
rec start htop       # Start recording with specific command
```

## Available Commands

### List Active Sessions

```sh
rec list
```

Shows all active recording sessions with their human-readable IDs (e.g., `blue-moon-fire`), PIDs, start times, and commands.

### Read Terminal Output

```sh
rec scrollback                    # Get full scrollback from latest session
rec scrollback -l 50              # Get last 50 lines
rec scrollback -s blue-moon-fire  # From specific session
```

Use this to see what's displayed in the other terminal - server logs, command output, error messages, etc.

### Get Cursor Position

```sh
rec cursor                        # From latest session
rec cursor -s blue-moon-fire      # From specific session
```

Returns `Row: N, Col: M` - useful for understanding where the user is in the terminal.

### Get Terminal Size

```sh
rec size                          # From latest session
rec size -s blue-moon-fire        # From specific session
```

Returns dimensions like `24x80` (rows x columns).

### Inject Input

```sh
rec inject "ls -la"               # Type into latest session
rec inject -s blue-moon-fire "cd /tmp"  # Into specific session
```

**Important**: This types the text but does NOT press Enter. To execute a command:
```sh
rec inject "ls -la\n"             # Include newline to execute
```

### Subscribe to Live Output

```sh
rec subscribe                     # Stream from latest session
rec subscribe -s blue-moon-fire   # From specific session
```

Streams live terminal output until interrupted. Useful for watching logs in real-time.

## Common Use Cases

### Check if a dev server is running
```sh
rec scrollback -l 20
# Look for "Server running on :3000" or similar
```

### Watch for errors in a build
```sh
rec scrollback | grep -i error
```

### Run a command in the other terminal
```sh
rec inject "npm test\n"
# Wait a moment, then check output
rec scrollback -l 30
```

### Monitor a long-running process
```sh
rec subscribe
# Ctrl+C to stop
```

## Session IDs

Sessions have human-readable IDs like `blue-moon-fire` instead of UUIDs. Use `-s` or `--session` to target a specific session when multiple are running.

## Tips

1. **Always check sessions first**: Run `rec list` to see what's available
2. **Use line limits**: `rec scrollback -l 50` is faster than full scrollback
3. **Include newlines for commands**: `rec inject "command\n"` to execute
4. **Check output after injecting**: Wait briefly, then `rec scrollback` to see results
