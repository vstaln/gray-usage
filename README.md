<h1 align="center">gray-usage</h1>
<p align="center">Per-session token &amp; cost accounting — port of pi-usage + pi-harness-runtime /usage.</p>

`/usage` scans `~/.gray/sessions/*.jsonl`, sums every entry's `usage`
object per session, estimates cost via a configurable rate table, shows
live `turn_end` counters for the running session, and queries provider
balance endpoints where they exist — all in one table.

## Commands

- `/usage` — last 25 sessions (by mtime) + TOTAL + live + balances.
- `/usage all` — every session file.
- `/usage reset` — clear live counters.
- `/tokens` — alias: gray's builtin `/usage` shadows the plugin command
  in the interactive REPL, so `/tokens` is the working interactive path.
  (`gray plugin check` and the `command/run` wire reach `/usage` fine.)

## Report

```
session    model          turns   input  output  cached    est$
abc123…    claude-sonnet-4    12   98.2k  41.0k   60.1k    0.83
…
provider balances:
  openrouter   data.total_credits=86.2  data.total_usage=87.9
```

## Configuration — `~/.gray/usage/config.json`

```json
{
  "rates": {"swe-2": {"input": 3.0, "output": 15.0, "cache_read": 0.30}},
  "providers": {
    "xai": {"url": "https://api.x.ai/v1/billing/balance", "auth_env": "XAI_API_KEY"}
  }
}
```

- `rates`: substring match on the session's model id → USD/MTok for the
  `est$` column. Built-in defaults cover the claude-*/gpt-5/grok families;
  unpriced models show `-`.
- `providers`: provider→balance-endpoint map. `auth_key` reads a key from
  `~/.gray/auth.json`; `auth_env` reads one from the environment. Fetches
  run in parallel via `curl -m 8` and degrade to `unreachable (HTTP n)`
  or `no curl` rows — a dead endpoint never breaks the report.

Shipped defaults: **openrouter** (`/api/v1/credits`, verified working),
**opencode-zen** and **xai** (candidate paths — upstream exposes no
public balance endpoint today, so they report `unreachable`; point them
at a working endpoint in config).

## Install

```sh
cargo build --release
gray plugin install ~/grayplugins/gray-usage/target/release/gray-usage
```

## Limits

`est$` is an estimate — prices drift and cache-write pricing isn't
modeled. The session store is authoritative; live counters only cover
what `turn_end` saw since the sidecar started. State honors `$GRAY_HOME`.
