# Countdown

Days, hours, minutes and seconds left until a target instant, as four `numeral` cells with `label` units, ticking once a second.

**The consumer provides** `to` (an ISO string, a `Date` or epoch milliseconds — include the time zone offset, e.g. `2027-01-01T00:00:00+08:00`, or it counts to local midnight) and, optionally, `onComplete`, called once when it reaches zero, and `labels`, the four unit words (default `DAYS HOURS MIN SEC`; in Chinese `['天', '时', '分', '秒']`, which screen readers also read out). Under `lang="zh"` the unit words step up to 14px so Han characters read at the same size as Latin capitals. It fills its container's width; below 640px it stacks two by two at 64px. It stops at `00 00 00 00`; an invalid `to` shows `--`.

- Do place it inside a `Frame`, with the event on a `title` line above (`AN AGENT · 2027.01.01`) and one `tagline` below (`SOMETHING IS COMING.` / `即将到来。`).
- Do let the digits change in place; no flip or slide animation.
- Don't use more than one countdown on a page, or show seconds for a target more than a year away without a reason.
- Screen readers get the remaining time in words through `role="timer"`, which is not announced on every tick; announce completion yourself in `onComplete` if it matters.
