# Artwork provenance

Every raster in `assets/` descends from one image-model render of the
approved concept in this directory. The sculpture was isolated from that
render at its original 1254 px size, with the porcelain matte removed only
at its edge, and became `masters/vapor-foreground-1024.png`: a real RGBA
layer with its internal shading intact, mapped from the 824 px design
region onto the full 1024 authoring canvas. Every desktop icon is built
from it by `tools/build-icons.cjs`. The menu bar SVG and the marks are
Bézier redraws of the approved silhouette, simplified for small sizes; the
browser favicons are the orange mark SVG with its background rectangle
removed, exported to PNG and ICO. The banners and social cards composite
the legacy tile master into the generated lettering and haze.

The first cut of the app icon had a non-square tile with rounder corners
than Apple's grid and a soft shadow outside it, and macOS 26 boxed it
inside a system tile in the Dock. The second cut fixed the bounds with a
mask joined from three cubic arcs per corner, which left faint shoulders
on the diagonals. The current legacy tile is built independently of the
artwork: an 824 px enclosure at (100, 100) on the 1024 canvas cut by one
analytic superellipse (exponent 5, 128 tangent-matched cubic segments),
a porcelain gradient with soft internal edge shading and specular rim
highlights, opaque inside, zero alpha outside. The layered document under
`macos/Vapor.icon/` sidesteps the legacy mask altogether.

The banners and social cards were first generated whole, lettering
included, which left a stray mark above the light banner's `p`. They are
now composed: the earlier banners were edited into empty atmosphere
plates (prompts below), and the tile, a vector `Vapor` wordmark outlined
from Nimbus Sans Bold, and the edge highlights are placed by
`tools/build-marketing.cjs`.

The contact sheets here, both produced by `tools/make-previews.cjs`:

- `desktop-icons-preview.png` shows the legacy fallback, the Icon Composer
  foreground and backgrounds, the export sizes, the Windows unplated
  marks, the favicons, and the banners.
- `github-and-web-preview.png` shows the finished banners, social cards,
  and favicons.

The prompts below produced the current generated rasters, in the order
they were used. Reuse them when a variant is needed so the new file
matches the set.

## Atmosphere plates (current)

Light: Use case: precise-object-edit. Edit target: supplied Vapor light banner. Remove the entire app icon including tile, orange foreground and shadow, and remove all lettering completely. Output ONLY a clean empty atmospheric backdrop for later precise compositing. Preserve the near-white background and very subtle warm orange ambient haze, softly diffused across the central horizontal band, strongest slightly left of center. Keep the haze understated, almost imperceptible at edges. No discernible smoke swirls, no ribbons, no objects, no text, no sharp details. Wide 3:1 composition.

Dark: Use case: precise-object-edit. Edit target: supplied Vapor dark banner. Remove the entire app icon including tile, orange foreground and shadow, and remove all lettering completely. Output ONLY a clean empty atmospheric backdrop for later precise compositing. Preserve the deep near-black charcoal background and soft orange ambient haze, diffused across the central horizontal band strongest slightly left of center, but reduce the haze intensity by about a third. Restrained premium atmosphere. No discernible smoke swirls, no ribbons, no objects, no text, no sharp details. Wide 3:1 composition.

## App icon

Generate a single square production macOS icon asset of the EXACT app icon at left in this reference. Extract it alone and enlarge it, keeping its white rounded square and sculpted orange vapor symbol exactly as in reference. No redesign. Center icon with 7% margin in a square image. Output with TRANSPARENT BACKGROUND using actual transparency / alpha channel. Opaque white tile, only space outside tile transparent. No checkerboard pattern, no presentation sheet, no lettering or text, no additional icons. It is a clean transparent cutout of the original left app icon.

## README banner, first pass

Create the FINAL horizontal GitHub README banner for Vapor using the approved original app icon from the large left part of this reference. Wide horizontal canvas about 3:1, at least 1800 px wide if possible. A SINGLE cohesive composition on perfectly uniform opaque warm-white #FAF9F7 background. On the LEFT place the exact approved porcelain rounded-square app icon with sculpted smooth Aerospace International Orange vapor trail, billow right and two tapered wisps left, faithfully preserve reference appearance and proportions, absolutely no redesign, no faceting. Tile is front-facing, subtle local shadow, occupies about 60 percent of banner height. On the RIGHT place only the word "Vapor" in large beautifully kerned dark charcoal modern neo-grotesque sans serif lettering, Apple-like typographic restraint, medium weight similar to SF Pro Display Medium, generous scale, normal upright letters, no quirky letter substitutions. Wordmark and icon vertically centered along same optical centerline, well-balanced lockup centered horizontally in canvas as a whole, generous whitespace on all sides and a comfortable gap between icon and lettering. Clean premium native macOS product identity. Do not include any slogan, menus, labels, monochrome samples, decorations, watermarks, black gradients, smoky background, checkerboard, or extraneous text. Exact word Vapor, capital V, lower-case apor. Finished ready-to-use banner, not a mockup on a wall or a design sheet.

## README banner, light (the plate's source)

Edit this Vapor LIGHT MODE banner. Keep the exact icon and wordmark, their positions and sizes, typeface and BOLD weight, kerning, sculpted depth and lighting, and the 3:1 canvas. Change ONLY the background atmospheric effect: completely REMOVE ALL visible ribbons, orange waves, swirling bands, filaments, streaks, and structured smoke. Replace them with an extremely soft barely perceptible diffuse peach atmospheric haze, like warm light scattered through a trace of mist in a white studio. It has no defined shapes or edges, no flowing lines, no visible tendrils; mostly clean pearl-white negative space. A very faint broad warm blush immediately behind the icon and near the lower edge of the lettering, fading seamlessly to pure off-white at all sides. Reduce background effect prominence by about 90 percent. Keep lettering and icon crisp. Tone down cast orange glow around tile to a subtle natural bounce light. Elegant restrained airiness, not visible decorative smoke. Exact text 'Vapor'. Opaque finished horizontal banner.

## README banner, dark (the plate's source)

Edit this Vapor DARK MODE banner. Preserve the icon and wordmark exactly, positions and sizes, BOLD font weight, typeface, kerning, shallow sculpted volume, and 3:1 canvas. Change ONLY background atmosphere and excessive orange reflected glow: REMOVE every orange ribbon, flowing wave, filament, line, structured band of smoke and sharp bright streak. Background becomes mostly clean deep graphite #0D1117, with only extremely soft diffuse low-opacity warm amber atmospheric haze behind the icon and lower letters. Think a trace of mist illuminated by a distant warm light, no identifiable smoke shapes, no tendrils, no flowing wave pattern. The entire effect is heavily defocused with no defined boundary, softly fading into the charcoal. Reduce background effect prominence by 90 percent. Reduce orange rim glow on icon and bottom of letters to subtle warm reflected light, retaining pearly white bold letters and original orange icon. Keep the design spacious, calm and crisp with atmosphere in the background. Exact text 'Vapor'. Opaque finished horizontal banner.

## Social card, dark

Adapt this exact revised DARK Vapor banner to a 2:1 landscape social sharing card. Preserve the design, icon shape, BOLD pearly white wordmark, typeface, kerning, shallow sculpted letter volume, subtle warm highlights, and the calm dark graphite palette. Recompose with icon LEFT and Vapor RIGHT, whole lockup centered horizontally and vertically, spanning 68 percent of canvas width, with wide safe margins and generous quiet space above and below. ONLY background effect is the same very soft low-opacity warm amber atmospheric haze behind the icon and lower text; distant warm light in barely visible mist, diffuse and almost imperceptible. No ribbons, waves, wisps, tendrils, filaments, defined smoke shapes, streaks, or additional decoration. Haze fades completely into graphite at edges. Exact text 'Vapor', no other text. Opaque finished social image with a 2:1 canvas.

## Social card, light

Create the matching LIGHT social card from reference 1 (dark social composition) using the palette of reference 2 (light banner). Keep reference 1's 2:1 aspect ratio, exact centered icon-left and wordmark-right layout, relative scale, BOLD typeface, letter shapes and kerning, and wide safe margins. Change to clean pearl-white backdrop, charcoal graphite sculpted 'Vapor' lettering with subtle shallow depth and soft warm lower-edge reflections. Preserve original white porcelain tile and orange vapor symbol. Background atmosphere is ONLY an extremely faint diffuse peach haze behind the icon and below the lettering, a trace of warm light in white air, fading completely to white at edges. No ribbons, waves, filaments, streaks, tendrils, shaped smoke or other decoration. Restrained almost imperceptible haze, lots of clean white negative space. Exact text 'Vapor', no other text. Opaque finished 2:1 landscape social card.
