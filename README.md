# dumi

A tiny personal AI agent, written in Rust as a learning project.
dumi chats with you, runs commands on your machine, remembers things,
and schedules its own reminders.

**Design rule #1: keep it simple.** One binary crate, small files, heavy
comments. The codebase doubles as a Rust course for its maintainer.

## Status

- [x] Terminal chat with any OpenAI-compatible model (cloud or local)
- [x] Tools: run shell commands, read/write files, long-term memory, reminders
- [ ] Scheduler: make stored reminders actually fire (+ heartbeat) — next up
- [ ] Jami channel: chat with dumi over P2P Jami messages

## Quickstart

1. Install Rust stable via [rustup](https://rustup.rs).
2. Build: `cargo build --release`
3. Configure: `cp example.config.toml ~/.config/dumi/config.toml`
   and edit `base_url`, `api_key` and `model`.
4. Run: `cargo run --release`
5. Try:
   - *"remember that I prefer plain text answers"*
   - *"remind me in 1 minute to stretch"* (stored; firing lands with the scheduler)
   - *"run `uname -a` and explain it"*

## Security warning

`run_command` and `write_file` mean the agent can do anything your user
account can. Only run dumi for yourself, and never expose it to strangers
(an owner allowlist arrives with the Jami channel).

## License

MIT — see [LICENSE](LICENSE).
