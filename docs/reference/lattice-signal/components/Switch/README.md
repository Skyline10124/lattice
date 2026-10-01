# Switch

An on/off switch for a setting that takes effect at once: a `control`-filled 36 × 18px track — the checkbox's height, so it never outgrows the line of text — with a full-height `ink-muted` bar, a quarter of its width, flush at its left end; when on, the track fills with `signal` and the bar, now `ink`, slides flush to the right end. No border; `glow` on hover.

**The consumer provides** `label` (what the switch turns on), and either `checked` with `onChange(checked)` or `defaultChecked`; `disabled` and other button props pass through. It renders a `button` with `role="switch"` and `aria-checked`.

- Do name the thing being turned on — "Resume on restart" — never "On/Off" or a question.
- Do use a switch only for changes that apply immediately; a choice submitted with a form is a `Checkbox`.
- Don't put two switches that depend on each other side by side without saying so.
