---
layout: ../../layouts/Doc.astro
title: Inline images
subtitle: Pictures in the terminal — the kitty graphics protocol, iTerm2's inline images and sixel — and how they behave on screen.
kicker: Docs / Inline images
date: 2026-10-04
---

Since v0.71.18 Unterm draws pictures that programs print into the terminal. Three protocols are understood, so most image tools work without configuration:

```sh
chafa photo.jpg                     # picks kitty, sixel or iTerm2 by itself
chafa -f kitty photo.jpg
img2sixel diagram.png
magick chart.png sixel:-
viu screenshot.png
unterm-cli imgcat logo.png          # iTerm2's OSC 1337 File=
```

## What is supported

| Protocol | Supported | Not supported |
|---|---|---|
| **kitty graphics** (`APC G … ST`) | Transmit, display, transmit-and-display, delete (all, by id, by number, at the cursor, at a cell, by column, row or z-index), query. PNG (`f=100`), raw RGB/RGBA (`f=24`/`32`), zlib (`o=z`). Chunked transfers (`m=1`). Data inline (`t=d`), from a file (`t=f`) or a temporary file (`t=t`, deleted after reading). Source rectangles, `c`/`r` sizing, `C=1`, placement ids, replies with `q=1`/`q=2`. | Shared memory (`t=s`), animation frames, Unicode placeholders (`U=1`) and relative placements — each answered with `EINVAL` so the program can fall back. |
| **iTerm2** (`OSC 1337 ; File=`) | `inline=1`, `width`/`height` in cells, `px`, `%` or `auto`, `preserveAspectRatio`, `doNotMoveCursor`, `name`; the multipart form (`MultipartFile` / `FilePart` / `FileEnd`). | Downloads (`inline=0`): nothing is saved to disk. |
| **sixel** (`DCS … q … ST`) | Colour registers (RGB and HLS), repeats, raster attributes. | — |

Pictures can be PNG, JPEG, GIF (first frame), WebP or BMP, up to 40 million pixels.

## How pictures behave

- **At the cursor, sized in cells.** A picture covers a block of cells worked out from its size and the cell size the window reports, and never wider than the pane. The cursor ends on the picture's last row, beside it (sixel: below it, at the same column), so the next line of output starts under the picture.
- **They scroll with the text.** A picture is anchored to the row it was printed on. It scrolls into the scrollback with that row, is drawn with its top cut off when it is partly above the viewport, and comes back whole when you scroll up to it.
- **Text stays on top.** Pictures are drawn over cell backgrounds and under text, the way kitty stacks them by default.
- **Each pane keeps its own.** A picture is clipped at its pane's edges, and dimmed with the pane when the theme dims inactive panes.
- **The alternate screen is separate.** Pictures drawn by a full-screen program (vim, less, a TUI) belong to its alternate screen and go when it exits; the main screen's pictures come back.
- **Clearing clears them.** `clear`, `CSI 2 J` and `CSI 3 J` remove the pictures on what they erase.

Programs that ask the terminal for its size in pixels get the real one: `CSI 14 t` (text area), `CSI 16 t` (one cell), and the pty's own pixel size, which `kitten icat`, `chafa`, `timg` and `yazi` read. The device attributes (`CSI c`) report sixel support.

## For agents and scripts

The pictures in a pane are listed by MCP [`screen.images`](/docs/mcp-reference#screenimages) and `unterm-cli session images`:

```sh
$ unterm-cli session images --pane-id 1
ROW      COL   CELLS     PIXELS      PROTOCOL NAME
0        0     25x6      400x200     iterm    quad.png
```

`capture.window` (`unterm-cli screenshot --self`) shows them as drawn.

## Limits

A pane keeps up to 320 MB of decoded pictures and 1,024 placements per screen; past that, the oldest pictures that are no longer on screen are forgotten. A picture is sent to the window at most 4,096 pixels on its longest side, which is more than any pane shows.
