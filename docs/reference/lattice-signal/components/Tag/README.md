# Tag

A solid, square tag with one word in capitals — the Lattice TUI's mode tags on paper. **State** tags say what state something is in; **emphasis** tags bring the cassette palette to words that are not states.

**The consumer provides** `tone` and a one-word label as children. State tones — `prompt`, `warn`, `error`, `ok` — fill with `signal`, `amber`, `vhs-red`, `phosphor` and default to their own word. Emphasis tones — `iris`, `red`, `amber`, `cyan`, `ink` — fill with `signal`, `vhs-red`, `vhs-amber`, `phosphor`, `ink` and need the word. The label is `on-signal`, `on-status`, `on-vhs-amber` or `paper`, set in mono capitals at the `label` size and tracking — the one place words stay mono, matching the TUI.

- Do put the tag first on its line, followed by the message in `text` (sans), with any path or ID inside it in `code` — `WARN  Context window at 92%.`
- Do keep the word to the four defaults where you can; a custom word stays one word, capitals, no punctuation.
- Do use emphasis tags beside a heading, at the start of list and table rows, or on a card — NEW, BETA, LIVE, a category, a version. Keep one meaning to one colour, and three or four colours at most in one set.
- Don't use a tag as a button or a link, or stack several on one line.
- Don't put a state word (WARN, ERROR, OK) on an emphasis tag, or style a tag outside these tones; for a state, use its state tag.
