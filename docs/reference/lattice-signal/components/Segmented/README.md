# Segmented

One choice from two to five options, shown as one `control`-filled strip with no dividers; a hovered segment fills with `glow`, the chosen one with `signal` and its label turns `on-signal`. It applies at once — a view or a mode, not a form answer.

**The consumer provides** `options` (strings, or `{ value, label, disabled }`), an optional `label` above in the `label` style, and either `value` with `onChange(value)` or `defaultValue` (the first option otherwise); `disabled` turns off the whole strip. Native radios sit underneath, so arrow keys move the choice and screen readers hear a labelled radio group.

- Do use it to switch how the same content is shown — "List / Grid", "24h / 7d / 30d" — or a mode that takes effect immediately.
- Do keep labels to one or two short words of similar length; a segment never wraps.
- Don't use it for choices submitted with a form (use `Radio`), for on/off (use `Switch`), or for more than five options.
- Don't use it as page navigation or tabs that load different content.
