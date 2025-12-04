<p align="center">
  <img src=".github/assets/header.svg" alt="record" width="100%"/>
</p>

<p align="center">
  <code>nix run github:andrewgazelka/record</code>
</p>

Let Claude Code see and control your other terminal windows.

## The Problem

You're running a dev server in one terminal tab. You ask Claude Code to check if it's working. But Claude Code can't see that tab - it only sees its own terminal.

## The Solution

```sh
# In your terminal (Ghostty, iTerm, Terminal.app, etc.)
record

# Now Claude Code can:
# - See everything printed to this terminal
# - Type commands into this terminal
# - Watch for errors in real-time
```

That's it. Run `record` instead of opening a plain shell, and Claude Code gains visibility into that session.

## Example

```sh
# Terminal 1: Run record, then start your server
record
npm run dev

# Terminal 2: Claude Code can now see "Server running on :3000"
# and can run "curl localhost:3000" in Terminal 1
```

## Commands

```sh
record-client list         # see active sessions
record-client scrollback   # read terminal output
record-client inject "ls"  # type into the terminal
```

---

MIT
