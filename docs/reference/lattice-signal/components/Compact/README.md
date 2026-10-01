# Compact

The product density, shown as a session window: not a component but a scope. Put `className="ls-compact"` on a window, pane or panel and every component inside tightens — 28px (`control-compact`) controls and table rows, `compact-text` at 13px, `space-1` and `space-3` steps, field labels and column heads in `compact-label` (11px, 0.08em), tags at 11px and 0.12em, a 32 × 16 switch, 16px checkbox and radio boxes. Colours, square corners, fills, tags and the focus ring stay the same.

- Do use compact for app chrome: session lists, tool-call logs, settings, inspectors, toolbars. Keep the expressive scale for landing pages, heroes, empty states and one-off announcements.
- Do write compact text in sentence case; only `compact-label` is in capitals. Machine values use `compact-code` (12px mono).
- Do mark the selected row with `glow` and hover with `control`; in a dense list draw the focus ring inside the row, so it never covers a neighbour.
- Don't put the marked frame, the VHS stripe, dither or the serif display inside a compact area; a hairline (`line`) separates panes.
