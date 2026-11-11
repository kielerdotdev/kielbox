# Kielbox marketing site

Standalone Astro site for **https://box.kieler.dev**. It builds static HTML and
CSS with local fonts and screenshots, without browser JavaScript. It has its
own dependencies, build output and deployment configuration. No Cloudflare
Access, R2, D1 or cloud-app credentials are needed to build it.

Use Node.js 22.23.4 or newer:

```sh
cd site
npm ci
npm run dev
```

The LAN development URL uses port 4321. `npm run build` writes `site/dist/`;
`npm run preview` serves that build locally on the same port.

## Deploy the marketing site only

```sh
cd site
npm ci
npx wrangler login
npm run deploy
```

`wrangler.jsonc` deploys only the static `dist/` directory as `kielbox-site` and
binds the custom domain `box.kieler.dev`. The `kieler.dev` zone must be in the
Cloudflare account used by Wrangler. `astro.config.mjs` sets the canonical URL.
There is no Astro server adapter and no dependency on the cloud application.

The optional cloud companion remains in `../cloud/`, is **not deployed**, and
serves its authenticated library at `/` when run locally. The marketing site
links to its setup documentation rather than a nonexistent live cloud service.

## Design

`src/pages/index.astro` preserves the supplied “Kielbox Cloud (4).html” design.
`src/styles/global.css` contains the landing styles; `public/landing/` holds the
four supplied desktop screenshots. Fonts and their license are in `public/fonts/`.
The layout uses the same desktop palette as `../cloud/DESIGN.md`; the two apps
are built and deployed independently.
