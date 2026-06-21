# FileID — website

The marketing landing page for [FileID](https://github.com/AdamNolle/FileID), the
on-device AI file organizer.

## What it is

A single static page — plain HTML, CSS, and one small vanilla-JS file. **No
framework, no build step, no bundler, no external runtime dependencies.** Open
`index.html` in any browser and it just works.

```
website/
├── index.html      # the page (semantic, accessible markup)
├── style.css       # brand styling + responsive layout
├── app.js          # animated LavaLamp canvas background + scroll reveal
├── assets/
│   ├── fileid-logo.svg   # app icon (from platforms/linux/data/)
│   └── fileid-256.png    # raster icon (from platforms/windows/.../Assets/Logo/)
└── README.md
```

## Design

Matches the FileID brand: a dark base with the gold `#FFCC00` primary and
lavender `#B19BCE` / cyan `#A0E2EA` / pink `#F2A6C0` accents, an animated
LavaLamp-style gradient (a `<canvas>` of drifting radial-gradient blobs that
mirrors the apps' `LavaLampBackground`), spring-ish easing, and glassmorphism
cards. It is responsive (mobile → desktop), accessible (semantic landmarks,
skip link, alt text, focus-visible styles, contrast-checked), and respects
`prefers-reduced-motion` (the background renders a single static frame and
scroll reveals are disabled).

All claims on the page are accurate to the project: no fake download buttons —
every CTA links to the GitHub repo or the in-repo build/packaging instructions.

## Local preview

No tooling required. Either open the file directly:

```bash
open website/index.html        # macOS
xdg-open website/index.html    # Linux
```

…or serve it (so relative asset paths and the canvas behave exactly as in
production):

```bash
cd website && python3 -m http.server 8000
# then visit http://localhost:8000
```

## Deployment

Deployed to **GitHub Pages** by `.github/workflows/pages.yml`. The workflow:

1. triggers on push to `main` whenever anything under `website/**` changes (and
   can be run manually via **workflow_dispatch**);
2. uploads the `website/` directory as a Pages artifact
   (`actions/upload-pages-artifact`); and
3. publishes it with `actions/deploy-pages` (permissions `pages: write`,
   `id-token: write`).

Because the site is fully static, there is no build phase — the directory is
uploaded as-is. To enable it the first time, set **Settings → Pages → Source**
to **GitHub Actions** in the repository.
