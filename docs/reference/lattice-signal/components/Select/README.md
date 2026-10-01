# Select

A labelled native dropdown with a drawn square-capped chevron, for one choice out of more than five.

**The consumer provides** `label`, `options` (strings or `{ value, label, disabled }`) or `<option>` children, optionally `placeholder` for an empty first choice, `hint` or `error`, and the usual select props (`value`/`onChange` or `defaultValue`, `name`, `disabled`). It is the native control underneath, so keyboard, forms and mobile pickers behave as the platform does. Where the browser supports `appearance: base-select`, the open list is styled too: the field's width, no border, `paper` rows on a `control` mat with their text in line with the field's, `glow` under the pointer and the chosen row in `signal`; elsewhere the platform's own list opens.

- Do use it for six or more options, or a list that changes; two to five that should stay visible are `Radio` (in a form) or `Segmented` (a view).
- Do add a `placeholder` when there is no sensible default, so nothing is chosen by accident.
- Don't put actions in a select; a menu of commands is not a choice.
