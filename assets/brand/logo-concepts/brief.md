You are designing logo concepts for a new software product. Work only inside the current directory. Do not use the network. Do not install anything.

# Product
"Aster Office" is a native desktop office suite (word processor, spreadsheet, slides, Markdown editor) for macOS, Windows and Linux. It is written in Rust, works fully offline, and its selling points are speed, the same look on every platform, and files that stay on the user's machine.

"Aster" is Greek for "star" and is also a star-shaped flower with many thin petals around a centre.

# Visual direction
- Flat and geometric. No gradients, no shadows, no filters, no 3D, no textures.
- Neutral palette: near-black #0b0b0b, white, and exactly ONE accent colour per concept. Propose the accent yourself. Do not use emerald green (#047857 or close to it).
- The interface it sits in is quiet: neutral greys, thin lines, 6px corner radii, single-row toolbars. The mark should feel precise and calm, not playful.
- Must stay recognisable at 16x16 px, so few shapes and no thin details that vanish when small.
- Must NOT resemble the logos of Microsoft Office / Microsoft 365, LibreOffice, Google Workspace, WPS Office, Apple iWork, or the Rust language logo. Do not use a letter inside a coloured square in the Microsoft style.

# Deliverables
Create FOUR clearly different concepts, named concept-a ... concept-d. They should differ in idea, not just in colour. Aim for a spread such as: a star/aster built from document or page shapes; a monogram "A"; an abstract mark built from the three document types (page, grid, slide); a pure geometric aster flower.

For each concept write these files:
1. `concept-X-icon.svg`: the app icon, viewBox="0 0 1024 1024". A rounded-square background (corner radius about 224) with the mark centred on it.
2. `concept-X-mark.svg`: the mark alone in a single colour (#0b0b0b) on a transparent background, viewBox="0 0 1024 1024". This is what will be used at 16 px and in monochrome contexts.
3. `concept-X-lockup.svg`: horizontal lockup, the mark at the left and the wordmark "Aster Office" to its right, viewBox="0 0 1600 400", transparent background. Use <text> with font-family="Inter, 'Helvetica Neue', Helvetica, Arial, sans-serif"; this is a concept stage so live text is fine.

Also write `concepts.md` with, for each concept: one sentence on the idea, the accent colour hex, and one honest sentence on its main weakness.

# SVG rules
- Hand-written, clean SVG 1.1. Only these elements: svg, title, g, path, rect, circle, ellipse, line, polygon, polyline, text, defs, clipPath.
- No <script>, no <style> blocks, no <image>, no <use> of external files, no external URLs other than the SVG namespace, no embedded fonts, no raster data, no foreignObject, no animation.
- Every file starts with <svg xmlns="http://www.w3.org/2000/svg" ...> and contains a <title>.
- Use coordinates with at most one decimal place. Build radial shapes with real computed coordinates, not approximations that look uneven.
- Keep each file under 6 KB.

When finished, list the files you wrote. Do not write anything else to disk.
