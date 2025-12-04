<p align="center">
  <img src=".github/assets/header.svg" alt="record" width="100%"/>
</p>

<p align="center">
  <code>nix run github:andrewgazelka/record</code>
</p>

Transparent PTY wrapper exposing a Unix socket API for terminal introspection.

## Features

- **Scrollback access**: Read terminal output programmatically
- **Input injection**: Send keystrokes to the session
- **Live streaming**: Subscribe to output in real-time
- **Zero config**: Just wrap any command

## Use Case

AI terminal agents (like Claude Code) monitoring long-running processes:

```
Terminal                          AI Agent
┌──────────────────────┐         ┌──────────────────────┐
│ $ record             │         │ Read scrollback      │
│ [session abc...]     │───────▶ │ Inject commands      │
│ $ npm run dev        │  Unix   │ Monitor output       │
│ Server on :3000      │  Socket │                      │
└──────────────────────┘         └──────────────────────┘
```

## Usage

```sh
record              # instrumented shell
record htop         # instrumented htop
record ssh server   # instrumented ssh
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
{"type": "inject", "data": "ls -la\n"}
{"type": "subscribe"}
```

---

MIT
