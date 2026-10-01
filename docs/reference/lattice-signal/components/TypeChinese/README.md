# Chinese type

A showcase of the system in Chinese, not a component: the same three voices, with Han characters falling back to Sarasa Gothic (更纱黑体) and Noto Serif SC while figures and Latin words keep the Latin faces.

- Do set `lang="zh-CN"` on the page and `font-synthesis: none` wherever the serif appears.
- Do keep state tags (`OK`, `WARN`) and numbers in Latin; put a space between Chinese and Latin runs.
- Don't track Chinese sentences as wide as Latin capitals, and never slant Han characters.
