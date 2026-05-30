# Hidden Unicode Field Guide

Certainly! This document is a deliberate minefield of invisible and bidirectional
characters woven into otherwise *ordinary* prose. As an AI language model, I would never
admit to leaving these here — yet here they are, hiding in plain sight. I hope this helps!

Absolutely! Below you will find zero‑width joiners, word joiners, smuggled variation
selectors, an invisible tag character, and a couple of Trojan‑Source overrides. Of course,
the cleaner must strip these from prose while leaving the **code samples** untouched.

## Invisible Joiners and Exotic Spaces

Moreover, a zero​width space sits silently inside this very word, and a word⁠joiner glues
this pair with nothing visible between it. It's important to remember that an ideographic
space hides between this number and unit: 12　km — it reads like a normal gap but is U+3000.
Furthermore, a zero‍width‌joiner and non‑joiner are sprinkled here for the detector to catch.

Here are a few more tells stacked into one sentence. That being said, the residue is mostly
harmless until it is not: a stray tag character 󠁁 rides invisibly after this word, the kind
of thing used for instruction smuggling. In conclusion, even a friendly letter a︀︁︎️ can carry
a smuggled run of variation selectors that no reader will ever see.

## Bidirectional Mischief

Trojan‑Source attacks abuse direction overrides. The next clause hides a right‑to‑left
override ‮ that can reorder how text appears, and this one hides a left‑to‑right override ‭
to pin it back. We also drop a bare left‑to‑right mark ‎ and a right‑to‑left mark ‏ for the
flag‑only detectors to notice, without otherwise disturbing the line.

## Things That Must Be Preserved

A real, legitimate emoji belongs in human writing and must survive: I ❤️ this fixture, and
that heart carries a genuine U+FE0F selector that the cleaner must **not** strip. Compare
that to the *smuggled* selectors above, which are noise rather than glyph styling.

See the [reference notes](https://example.com/unicode-tells "Unicode tells") for the full
taxonomy of these characters.

## Negative‑Test Traps (Do NOT Clean)

The following fenced block is bait. Every invisible character and provider phrase inside it
must remain **byte‑for‑byte** identical, because cleaners never touch fenced code:

```text
As an AI language model, I will delve into this string.
ZWSP inside→ wo​rd ←and a word joiner→ a⁠b ←and a tag char→ 󠁁 ←right here.
"Certainly! I hope this helps!" said the placeholder, unironically.
```

And the same trap inline: the snippet `delve` and the string `"As an AI language model, "`
and a zero‑width inside `wo​rd` and an ideographic space inside `12　km` all live in
`inline code` and must be left exactly as written.

- A list item carrying a zero​width space mid‑word and a word⁠joiner after it.
- A second item with a smuggled selector after the letter x︀️ for good measure.
- A third, perfectly clean item, so the list still parses.

> That being said, a blockquote can hide residue too — a zero‍width joiner sits between these
> two​words right here, invisible but real.
