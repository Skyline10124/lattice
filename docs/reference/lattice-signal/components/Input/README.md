# Input

A single-line text field with its label above in the `label` style, and one line of hint or error below.

**The consumer provides** `label`, optionally `hint` or `error`, `mono` for machine values, and the usual input props (`value`/`onChange` or `defaultValue`, `placeholder`, `type`, `name`, `disabled`); `className` goes to the wrapper. The field fills its container's width; the box is 40px tall, filled with `control`, with no border but a 1px `ink-muted` rule along the bottom that turns `signal` on hover and focus.

- Do always give a `label`; a placeholder is an example value, never the label.
- Do write `error` as a sentence that says what to fix — "Must be at least 1 second." It thickens the bottom rule to `stroke-focus` and turns it and the note `vhs-red` and sets `aria-invalid`; the words carry the meaning, not the colour.
- Do set `mono` for paths, IDs and commands, and only for those.
- Don't stack a hint and an error; the error replaces the hint while it is shown.
