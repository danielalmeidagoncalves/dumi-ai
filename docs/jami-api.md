# Jami integration spike (2026-10-25)

Goal: find the simplest reliable way for dumi to chat over Jami on this
machine, before writing any code (plan step 8).

## What is installed here

| Piece | Status |
|---|---|
| `jami-daemon` 20260917-2 (Arch) | ships **`libjami.so` 17.0.0 + headers only** (`/usr/include/jami/*.h`, `pkg-config jami`) |
| standalone `jamid`/`dring` daemon | **not shipped** — nothing to talk D-Bus to |
| REST control API (`restcpp`) | **not shipped** |
| `jami-qt` 20260917.0-2 | Qt client that **embeds the daemon in-process** (links `libjami` directly, no D-Bus server for us) |
| daemon currently running | no |

## Options considered

### A. D-Bus client (`jami-rs` crate, or hand-rolled `zbus`)
`jami-rs` 0.2.0 (by a Jami core dev) is a D-Bus client that expects a
standalone daemon exposing `cx.ring.Ring.ConfigurationManager` on the session
bus. **Ruled out**: that daemon does not exist on this machine, and building
one from source is a huge detour. The Qt client does not provide this service
either (it embeds the daemon; the `net.jami.*` strings in it are QML modules).

### B. Embed `libjami` in dumi via a small extern "C" shim — **CHOSEN**
dumi loads `/usr/lib/libjami.so` and hosts its own Jami node in-process:
- `libjami::init(LIBJAMI_FLAG_NO_LOCAL_MEDIA)` — no audio/video stack needed
- `libjami::start()`
- `libjami::getAccountList()` / `libjami::addAccount(...)` — dumi gets its
  **own Jami account** (its own Jami ID; no conflicts with the Qt client's
  account database)
- `libjami::sendMessage(accountId, conversationId, message, replyTo)`
  (`conversation_interface.h:89`)
- incoming messages via `libjami::registerSignalHandlers(...)` with the
  `"SwarmMessageReceived"` signal (`conversation_interface.h:402`,
  signature: accountId, conversationId, `SwarmMessage&` with `from` +
  payloads)

Because `libjami` is a C++ API (mangled symbols, std::string/std::map
parameters), Rust cannot call it directly. The shim is a ~150-line C++ file
exposing plain `extern "C"` functions, compiled by `build.rs` via
`pkg-config jami` (`-ljami`). Only what dumi needs:

```text
dumi_jami_init(data_dir, callback)   // init + start + install trampoline
dumi_jami_accounts()                 // list account ids
dumi_jami_account_id()               // dumi's own Jami ID (for the owner to add)
dumi_jami_create_account()           // first-run account creation
dumi_jami_conversations(account)     // conversation ids
dumi_jami_send_message(conv, text)   // reply
callback(account, conversation, from_uri, body)  // Rust fn, called per message
```

The callback crosses C++ → C → Rust; everything else stays plain types
(C strings, null-terminated lists).

### C. Talk to the Qt client
No stable IPC surface. Ruled out.

## Consequences of the decision

- dumi is its own Jami identity. Setup: run dumi once, it prints its Jami
  ID; the owner adds it as a contact from their Jami app; the owner's Jami
  ID goes into `owner_jami_id` in the config (allowlist — dumi must never
  answer strangers, it can run shell commands).
- No D-Bus dependency, no extra daemon, works headless (NO_LOCAL_MEDIA).
- Cost: one small C++ shim + `cc`/`pkg-config` in `build.rs`. This is the
  only non-pure-Rust piece of dumi, kept in one folder (`jami-shim/`).

## Implementation outline (plan steps 9–10)

1. `jami-shim/shim.cpp` + `build.rs`: compile the shim, link `jami`.
2. `src/channel/jami.rs`: wraps the shim; on `SwarmMessageReceived`, check
   `from` against `owner_jami_id`, hand text to the agent, send reply back.
3. `src/channel/mod.rs`: the `Channel` trait console + jami both implement.
4. Scheduler delivery grows a "send via Jami if available" path.

## Open questions for first contact

- Swarm conversations need a contact request/accept flow between the owner
  and dumi's account before messages flow (one-time, manual).
- libjami 17 is new; if the headers' signatures drift, the shim is the only
  place to touch.
