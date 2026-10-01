# Textarea

A labelled multi-line text field — the input's box grown to its rows — for prompts, instructions and notes.

**The consumer provides** `label`, optionally `hint` or `error`, `mono` for commands and code, `rows` (default 4), and the usual textarea props (`value`/`onChange` or `defaultValue`, `placeholder`, `name`, `disabled`); `className` goes to the wrapper. It fills its container's width and resizes vertically only.

- Do use it wherever the answer may run past one line — a prompt, a system instruction, a note; a single value is an `Input`.
- Do follow the `Input` rules: always a label, a placeholder only as an example, an error written as a sentence that says what to fix.
- Don't set `mono` for prose; only for commands, code and logs.
