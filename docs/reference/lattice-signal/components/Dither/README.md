# Dither

A showcase of the system's one way of drawing light and pictures, not a component: whole `dither-pixel` squares on one page-wide grid, toned by a multi-level Bayer 4x4 ordered dither across four tiers — `haze`, `glow`, `signal`, `ink` — on a plain `field` ground.

- Do use it for backgrounds with the light end only: `haze` and `glow`, sparse, glow points on a lattice behind the content.
- Do use the full scale, down to `ink`, only for a special picture — a hero, an empty-state illustration — at most one per page.
- Do give a special picture at most one cassette accent — `vhs-amber`, `vhs-red` or `phosphor` — for a sun, a lamp, an indicator: under a fifth of the picture, dithered by its own density and drawn over the iris tiers. Never in a background.
- Do treat tone as distance from the ground: on Paper the darkest parts carry the highest tone, on Tape the brightest do — so a lit picture swaps its shading by theme, or it reads as a negative on Tape.
- Do compute it per pixel from a tone between 0 and 1; each pixel takes the tier below or above its tone by its Bayer threshold, so neighbouring tiers dither into each other. The preview's script is the reference.
- Do keep everything on the same page-wide grid and draw it at 1:1, so pixels land on whole device pixels.
- Don't soften it with blur, opacity or a gradient, clip it with a circle, or round the pixels.
