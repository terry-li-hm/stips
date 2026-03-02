# stips

OpenRouter CLI — check credits, view usage, manage your API key.

> *stips* — Latin for a small coin or contribution.

## Install

```bash
cargo install stips
```

## Usage

```bash
stips                  # check credit balance (default)
stips credits          # same as above
stips usage            # daily / weekly / monthly spend
stips key open         # open openrouter.ai/keys in browser
stips key save <key>   # save API key to macOS keychain
```

### Examples

```
$ stips
$13.74 remaining  ($636.26 used of $650.00)

$ stips usage
Daily:   $0.11
Weekly:  $1.45
Monthly: $10.75
```

## Setup

Get an API key at [openrouter.ai/keys](https://openrouter.ai/keys), then:

```bash
stips key save sk-or-...
```

The key is stored in macOS Keychain under `openrouter-api-key`.

## License

MIT
