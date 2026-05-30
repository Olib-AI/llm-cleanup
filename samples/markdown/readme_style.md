# widget-forge

> A *seamless*, **robust** toolkit for forging widgets — fast, reliable, and scalable.

[![build](https://img.shields.io/badge/build-passing-green)](https://example.com/ci)
[![license](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)

It's worth noting that `widget-forge` was built to leverage a single idea: do the boring thing
well. In today's fast-paced world, that is, in a sense, a small act of rebellion — and a
testament to the team that resisted the urge to add a plugin system nobody asked for.

## Installation

```sh
# These comments contain fingerprints on purpose — "delve", “smart quotes”, an em dash —
# and they MUST survive cleaning because they live in a fenced code block.
cargo install widget-forge          # it's worth noting that this line is code, not prose…
```

You can also build from source:

```sh
git clone https://example.com/widget-forge.git
cd widget-forge
cargo build --release
```

## Usage

The CLI mirrors the library one-to-one, so anything you can script you can also call directly.

```rust
use widget_forge::Forge;

fn main() {
    // Inline-ish trap: the string below looks like AI prose but is code.
    let banner = "Let's dive into widgets — a tapestry of possibilities…";
    let forge = Forge::new().robust(true).seamless(true);
    println!("{banner}: {}", forge.run());
}
```

When you need a one-off, the `forge` binary and its `--help` text cover the rest.

## Feature Matrix

| Feature              | `light` | `standard` | `aggressive` | Preserves formatting? |
|----------------------|:-------:|:----------:|:------------:|:---------------------:|
| Zero-width stripping |    ✅   |     ✅     |      ✅      |          ✅           |
| Curly → straight     |    —    |     ✅     |      ✅      |          ✅           |
| Ellipsis `…` → `...` |    —    |     ✅     |      ✅      |          ✅           |
| Em-dash replace      |    —    |     —      |   opt-in     |          ✅           |
| Lexical suggestions  |    —    |   report   |      ✅      |          ✅           |

Legend: a dash (`—`) in the *table cells above is content the tool may normalize, while the
em dashes in **this sentence** — used as connectors — are the kind a human writes on purpose.

## How It Works

The pipeline is four honest steps:

1. **Parse** the bytes to locate prose spans (never to regenerate them).
2. **Detect** fingerprints read-only, so a preview can be truthful.
3. **Transform** prose strings, idempotently.
4. **Splice** replacements into the *original* bytes, right-to-left.

See the [design notes](../../DESIGN.md) and the [project README](../../README.md) for the
full rationale. External reference: [CommonMark spec](https://spec.commonmark.org/).

![widget-forge architecture diagram](https://example.com/img/architecture.png "Four-stage pipeline")

## Contributing

Pull requests are welcome. It's worth noting that we delve into every diff with care, and we
ask that you keep prose plain and let the code speak for itself.

---

Licensed under MIT. © 2026 the widget-forge authors.
