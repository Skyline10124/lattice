# Table

A data table: a head in the `label` style over an `ink-muted` rule, rows split by `line` rules, 40px rows, no stripes and no vertical rules.

**The consumer provides** `columns` (`{ key, label, align, mono, render }`), `rows` (plain objects read by column key), optionally a `caption` in the `label` style and a `rowKey`. `render` returns any cell content — a `Tag` for a state column, a link for a name.

- Do set `mono` on IDs, counts, durations and sizes, and right-align the numeric ones so their digits line up; IDs stay left.
- Do keep one idea per column and write heads as nouns in capitals: SESSION, TOOL CALLS.
- Don't add zebra stripes, vertical rules or a surrounding frame; if the table needs a boundary, put it in a plain `Frame`.
- Don't use a table for layout.
