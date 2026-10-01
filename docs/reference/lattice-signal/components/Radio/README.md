# Radio

A native radio button with a drawn 18px square box — a `control` fill with no border, `glow` on hover; when selected a `signal` square (`space-2`) sits at its centre — square like every other control, and told apart from a checkbox by that inner square instead of a fill and check.

**The consumer provides** `label`, a shared `name` for the group, each option's `value`, and `checked`/`onChange` or `defaultChecked`. Wrap the group in a `fieldset` with a `legend` in the `label` style.

- Do use radios for one choice out of two to five options that should all stay visible; more than that is a select.
- Do preselect the sensible default so the group never starts empty.
- Don't mix radios and checkboxes in one group.
