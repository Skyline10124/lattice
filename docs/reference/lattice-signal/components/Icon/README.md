# Icon

An IBM Carbon icon drawn in the current text colour at 16, 20, 24 or 32px — the system's only icon set.

**The consumer provides** `name` (one of the fifty shipped icons, listed in the preview), `size` (default 20) and, when the icon stands alone, a `label` that screen readers speak; without a label the icon is hidden from them. Colour comes from the surrounding text, so it is `ink` in a line of text, `ink-muted` beside a hint and `on-signal` inside a primary button.

- Do pair an icon with words: at most one leading icon in a button, 20px; an icon-only button carries `label` and stays secondary.
- Do use 16px in tables and beside `text-small`, 20px with body text and in controls, 24 or 32px only for empty states and beside large headings.
- Don't colour an icon on its own or with a state colour; state is a `Tag`.
- Don't redraw, restroke or mix in icons from another set. For a new one, take the 32px master from `@carbon/icons` unchanged and add it to the set.
