# Button

A square, 40px button with its label in sentence case, verb first — "Run agent", "Cancel".

**The consumer provides** `variant` (`primary` or `secondary`), the label as children (optionally led by one 20px `Icon`), and the usual button props (`onClick`, `disabled`, `type`, which defaults to `button`).

- Do use one `primary` button per view — a `signal` fill with an `on-signal` label — for the thing the view is for; every other action is `secondary` — a `control` fill with an `ink` label, `glow` on hover, no outline.
- Do keep labels short and literal; no capitals, no trailing full stop, at most one leading icon. An icon-only button is secondary and carries `aria-label`.
- Don't disable a button to explain a rule; say what is missing next to it instead. Disabled buttons drop to half opacity.
- Don't use a button for navigation; use a link.
