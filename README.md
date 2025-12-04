<p align="center">
  <img src=".github/assets/header.svg" alt="record" width="100%"/>
</p>

<p align="center">
  <code>nix run github:andrewgazelka/record</code>
</p>

Give Claude Code (or any AI agent) access to your terminal. Run `record` in Ghostty/iTerm/any terminal, and AI tools can read your scrollback, see what's running, and inject commands - without terminal emulator integration.

## Why

Claude Code runs in its own sandbox. It can't see your other terminal tabs. With `record`:

1. Run `record` in a terminal tab
2. Start a dev server, SSH session, or anything
3. Claude Code reads the output via Unix socket
4. Claude Code can type commands into that session

## Usage

```sh
record              # instrumented shell
record npm run dev  # instrumented command
```

```sh
record-client list              # list sessions
record-client scrollback -l 50  # last 50 lines
record-client inject "ls\n"     # send input
```

## Protocol

Connect to `~/.record/<session-id>.sock`:

```json
{"type": "get_scrollback", "lines": 50}
{"type": "inject", "data": "curl localhost:3000\n"}
{"type": "subscribe"}
```

---

MIT
