# Frame

The system's container, in two variants: **marked** (default) — four edges in `line` at `stroke-frame`, broken at each corner around a plus mark in `ink` (arms of `mark-arm`, with a `mark-arm` break to the edges), `space-12` padding — and **plain** — one continuous `stroke-hairline` border in `line`, `space-6` padding. Both are square with a `paper` fill.

**The consumer provides** `variant` (`marked` or `plain`), the children and the width; the frame fills its container's width and grows with its content. A marked frame's plus marks reach `mark-arm` outside its box, so leave at least that much room around it (the page gutter `space-6` is enough).

- Do use one marked frame for the one thing a page is about — a countdown, a key figure, a single form — with the page's `title` above and `tagline` below, each `space-6` away.
- Do use plain frames for everything else that needs a boundary: settings panels, a code block, cards in a list. They may repeat and sit side by side.
- Don't use more than one marked frame on a page, nest any frame inside a marked one, or give either a shadow or radius.
