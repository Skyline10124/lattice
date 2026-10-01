# Progress

Progress as a row of square cells lit whole, left to right, like a tape deck's level meter, with the percentage above in mono.

**The consumer provides** `value`, `max` (default 100), a `label` that says what is progressing, and optionally `cells` (default 24; for a small count of steps use the count itself — 7 of 12 tool calls → `cells: 12`). It fills its container's width and has `role="progressbar"` with the current values.

**Segmented.** Give `segments` (up to four `{ label, value, series }`) instead of `value` when the total is made of parts — the context window as system, tools, conversation and cache. Each part lights its own run of cells in `series-1` to `series-4` (by position unless `series` pins it), and a legend underneath names each part with its value in mono, so the colour is never the only label.

- Do label it with the work, not the widget: "Indexing workspace", not "Progress".
- Do let cells light whole; no easing, no glow, no striped animation.
- Don't use it for work of unknown length; say what is happening in a `Notice` instead.
- Do keep a part on the same series everywhere it appears, and fold anything past four into an "Other" part.
- Don't colour it by state; a failure is a `Notice` with an `ERROR` tag. Series colours name parts, never health.
