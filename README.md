# stips

OpenRouter CLI — check credits, view usage, manage your API key.

[![Crates.io](https://img.shields.io/crates/v/stips)](https://crates.io/crates/stips)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> *stips* — Latin for a small coin or contribution.

## Install

```bash
cargo install stips
```

## Setup

Get an API key at [openrouter.ai/keys](https://openrouter.ai/keys), then save it to the macOS Keychain:

```bash
stips key save sk-or-...
```

The key is stored under the service name `openrouter-api-key` and retrieved automatically on each command.

On non-macOS platforms, set the `OPENROUTER_API_KEY` environment variable instead (see [Environment Variables](#environment-variables)).

## Commands

### `stips` / `stips credits`

Show your current credit balance. Running `stips` with no subcommand is equivalent to `stips credits`.

```
$ stips
$13.74 remaining  ($636.26 used of $650.00)
```

If your remaining balance is below **$5.00**, a warning is printed to stderr:

```
⚠️  Low — top up at https://openrouter.ai/credits
```

The warning never affects the exit code — `stips credits` always exits 0 on success.

#### `stips credits --json`

Output the balance as JSON, suitable for scripting:

```json
{
  "remaining": 13.74,
  "used": 636.26,
  "total": 650.00
}
```

### `stips usage`

Show spend broken down by period:

```
$ stips usage
Daily:   $0.11
Weekly:  $1.45
Monthly: $10.75
```

All values are in USD as returned by the OpenRouter API.

#### `stips usage --json`

```json
{
  "daily": 0.11,
  "weekly": 1.45,
  "monthly": 10.75
}
```

### `stips key open`

Open [openrouter.ai/keys](https://openrouter.ai/keys) in the default browser (macOS only).

```bash
stips key open
```

### `stips key save <key>`

Save an API key to the macOS Keychain (macOS only). Overwrites any previously saved key.

```bash
stips key save sk-or-v1-...
```

## Environment Variables

| Variable | Description |
|---|---|
| `OPENROUTER_API_KEY` | API key. Takes precedence over the macOS Keychain. Required on non-macOS platforms. |
| `OPENROUTER_BASE_URL` | Override the API base URL (default: `https://openrouter.ai`). Useful for testing against a local mock. |

## Low Balance Warning

When `stips credits` (or bare `stips`) detects that your remaining balance is **below $5.00**, it prints a warning to stderr pointing to the top-up page. The warning is purely informational — the exit code remains 0. In non-TTY contexts (pipes, scripts), the emoji is omitted so the output stays clean.

## Changelog

### v0.2.0

- **`--json` flag** added to `credits` and `usage` for machine-readable output
- **Fixed `usage` values**: removed incorrect `/100` scaling heuristic — the API always returns dollar-denominated floats
- **Fixed exit code**: low-balance warning no longer causes a non-zero exit
- **Platform guards**: `key save` and `key open` now compile and fail gracefully on non-macOS
- **`OPENROUTER_BASE_URL` override** for testing against local mocks

### v0.1.x

- **v0.1.2** — minor dependency cleanup
- **v0.1.1** — removed unused `serde_json` dependency
- **v0.1.0** — initial release: `credits`, `usage`, `key open`, `key save`, macOS Keychain integration

## License

MIT
