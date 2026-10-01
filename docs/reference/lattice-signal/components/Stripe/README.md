# Stripe

The VHS bands borrowed from tape packaging — `vhs-red`, `vhs-amber`, `signal`, top to bottom (left to right on a spine) — as three variants. Purely decorative: `aria-hidden`, never a state.

**The consumer provides** `variant` and, for a spine, the children it marks. **band** (default) fills its container's width: three `space-2` bands with `space-2` gaps. **line** is a `space-2` tall trim: three `dither-pixel` bands 1px apart. **spine** puts three `dither-pixel`-apart vertical bars, each half of `space-2` wide, `space-4` to the left of its children and runs their full height.

- Do use one band per screen: under a hero, above the caption under a frame, or as a section break.
- Do use a line to trim the top or bottom edge of a page or a printed sheet.
- Do use a spine to mark the one key figure of a view, or the title of each card in a set of up to four.
- Don't put any variant behind text, recolour or reorder its bands, or use it to signal state.
