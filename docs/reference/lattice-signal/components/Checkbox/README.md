# Checkbox

A native checkbox with a drawn 18px square box — a `control` fill with no border, `glow` on hover — that fills with `signal` and shows an `on-signal` check when selected.

**The consumer provides** `label` and the usual input props (`checked`/`onChange` or `defaultChecked`, `name`, `value`, `disabled`). The real input stays in the page, so keyboard, forms and screen readers work as usual.

- Do use checkboxes for independent yes/no choices and for several picks from a list, inside a `fieldset` with a `label`-style legend.
- Do write labels as the thing that becomes true — "Confirm before writing files".
- Don't use a checkbox for a setting that applies at once; that is a `Switch`.
