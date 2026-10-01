# Notice

A one-line message inside the page: a state tag, one sentence in `text`, and at most one action, inside a plain `Frame` (1px `line` border, `paper` fill) tightened to `space-4` padding.

**The consumer provides** `tone` (`info`, `ok`, `warn` or `error`), the sentence as children — with any path or ID in `<code>` — and optionally `action`, usually one secondary `Button`. `info` shows an `INFO` tag on `signal`; the rest use their own tags. An `error` notice has `role="alert"` and is read out at once; the others use `role="status"`.

- Do place it where the thing happened — above the form it concerns, at the top of the panel — not floating over the page.
- Do write one sentence that says what happened, and what to do if anything: "Context window at 92%. Older turns will be summarised."
- Don't add a coloured side stripe, an icon or a shadow; the tag carries the state.
- Don't stack more than two notices; combine them or fix the cause.
