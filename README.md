# stips

OpenRouter CLI — check credits, view usage, and open the API keys page.

[![Crates.io](https://img.shields.io/crates/v/stips)](https://crates.io/crates/stips)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> *stips* — Latin for a small coin or contribution.

## Install

```bash
cargo install stips
```

## Setup

Get an API key at [openrouter.ai/keys](https://openrouter.ai/keys). Supply it through `OPENROUTER_API_KEY` using your shell or secret manager. Do not pass the key as a `stips` argument.

On macOS, you can instead use the Keychain Access app to create a password item in your login keychain. Use `openrouter-api-key` as the item name (service), `openrouter` as the account name, and your API key as the password. `stips` reads this item when no environment key is set; existing Keychain entries continue to work.

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

## Environment Variables

| Variable | Description |
|---|---|
| `OPENROUTER_API_KEY` | API key. Takes precedence over the macOS Keychain. Required on non-macOS platforms. |

Unset the retired `OPENROUTER_BASE_URL` variable, even if its value is empty. The default command, `credits`, and `usage` refuse to run while it is set. Authenticated requests target `https://openrouter.ai` (`/api/v1/credits` or `/api/v1/auth/key`), do not follow redirects, and have a 10-second timeout. Standard proxy settings such as `HTTPS_PROXY` still apply.

## Low Balance Warning

When `stips credits` (or bare `stips`) detects that your remaining balance is **below $5.00**, it prints a warning to stderr pointing to the top-up page. The warning is purely informational — the exit code remains 0. In non-TTY contexts (pipes, scripts), the emoji is omitted so the output stays clean.

## Changelog

### Unreleased

- Retired CLI credential writes; existing environment and Keychain credentials still work.
- Removed the API endpoint override for authenticated requests.

### v0.2.0

- **`--json` flag** added to `credits` and `usage` for machine-readable output
- **Fixed `usage` values**: removed incorrect `/100` scaling heuristic — the API always returns dollar-denominated floats
- **Fixed exit code**: low-balance warning no longer causes a non-zero exit
- **Platform guards**: Keychain integration and browser opening are macOS-only

### v0.1.x

- **v0.1.2** — minor dependency cleanup
- **v0.1.1** — removed unused `serde_json` dependency
- **v0.1.0** — initial release: `credits`, `usage`, `key open`, macOS Keychain integration

## License

MIT
