Meeting Notes — Q3 Planning
===========================

It's worth noting that these notes were typed fast and never cleaned up, so they are a
tapestry of half-thoughts, smart quotes, and the occasional em dash — exactly the kind of
residue the tool should normalize without touching the structure. Moreover, a word⁠joiner and
a smuggled selector on the letter x︀️ snuck in here too; that being said, the structure holds.

Attendees
---------

- Maya (chair)
- Devon
- Priya — joining remotely from 2021–2023 cohort alumni group
- A guest who, in today's fast-paced world, “forgot” to mute

Decisions
---------

1. Ship the *robust* path first; delve into the experimental one only if time allows.
2. Leverage the existing CI; do not, under any circumstances, build a new one.
3. Keep the API fast, reliable, and scalable — in that order of priority.

> Devon's one-liner, quoted verbatim so it must keep its punctuation:
> “It's not just a refactor — it's a rescue.”

Open Questions
--------------

- Do we normalize curly apostrophes in user content? (Maya: it’s worth a flag.)
- There is a non-breaking space hiding right here →  ← between the arrows.
- And a zero-width space hiding right here →​← that you cannot see but the linter can.
- The ellipsis question is still open…

Action Items
------------

1. **Maya:** draft the spec.
   - Include the "locator, not serializer" principle.
   - Note the em-dash policy explicitly.
2. **Devon:** wire up the `--dry-run` flag.
   1. Print a per-rule changelog.
   2. Never write on a dry run.
3. **Priya:** collect a small fixture corpus.[^corpus]

Snippet To Remember
-------------------

Do not let the tool touch this block — it is intentionally full of bait:

```text
"As an AI language model, I can help you delve into a tapestry of robust, seamless
synergy…" — this entire line lives in a code fence and MUST remain byte-for-byte.
```

Inline bait too: the literal `it's worth noting that…` inside backticks must not change.

---

That's all for today. In conclusion, next sync is a testament to our optimism: same time,
same room. Absolutely!

[^corpus]: Aim for ~100 labeled docs — half human, half machine — to calibrate thresholds.
