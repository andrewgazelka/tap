<p align="center">
  <img src=".github/assets/header.svg" alt="rec" width="100%"/>
</p>

<p align="center">
  <code>nix run github:andrewgazelka/record</code>
</p>

Let Claude Code see and control your other terminal windows.

## The Problem

You're running a dev server in one terminal tab. You ask Claude Code to check if it's working. But Claude Code can't see that tab - it only sees its own terminal.

## The Solution

```sh
rec
```

Your shell works exactly the same - you won't notice any difference. But now Claude Code can see and type into this terminal in the background.

## Example

```sh
# Terminal 1
rec              # starts your normal shell, nothing changes
npm run dev      # use it like normal

# Meanwhile, Claude Code can:
# - See "Server running on :3000"
# - Run "curl localhost:3000" in this terminal
# - Watch for errors
```

## Commands

```sh
rec                  # start your normal shell
rec start htop       # or any command
rec list             # see active sessions
rec scrollback       # read terminal output
rec cursor           # get cursor position
rec size             # get terminal size
rec inject "ls"      # type into the terminal
rec subscribe        # stream live output
```

## Architecture

```mermaid
flowchart LR
    subgraph rec
        PTY[PTY Master]
        SB[Scrollback Buffer]
        Sock[Unix Socket]
    end

    Shell[Shell/Command] <-->|stdin/stdout| PTY
    PTY --> SB
    PTY --> Sock

    Client[rec client] <-->|JSON| Sock
    Claude[Claude Code] --> Client
```

---

MIT
