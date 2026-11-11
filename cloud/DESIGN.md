---
name: Kielbox web
description: Desktop-matched capture library and static marketing page
colors:
  deep: "#141414"
  surface: "#1d1d1d"
  surface-2: "#262626"
  surface-3: "#2f2f2f"
  line: "rgb(255 255 255 / 0.08)"
  ring: "rgb(255 255 255 / 0.16)"
  t1: "#f2f2f2"
  t2: "#a8a8a8"
  t3: "#8f8f8f"
  accent: "#49c970"
  accent-hover: "#5bd27e"
  ink: "#07210e"
  rec: "#f1484f"
  rec-text: "#f29b96"
  busy: "#f2cf76"
typography:
  body:
    fontFamily: "'Geist', ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif"
    fontSize: "14px"
    fontWeight: 440
    lineHeight: "20px"
  title:
    fontFamily: "'Geist', ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: "28px"
    letterSpacing: "-0.012em"
  label:
    fontFamily: "'Geist', ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif"
    fontSize: "13px"
    fontWeight: 530
    lineHeight: "16px"
  mono:
    fontFamily: "'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace"
    fontSize: "12px"
  display:
    fontFamily: "'Geist', ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif"
    fontSize: "clamp(36px, 5.2vw, 60px)"
    fontWeight: 600
    lineHeight: 1.06
    letterSpacing: "-0.035em"
rounded:
  control: "6px"
  media: "8px"
  panel: "10px"
  card: "12px"
spacing:
  inline: "8px"
  panel: "16px"
  columns: "24px"
  section: "32px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  button-secondary:
    backgroundColor: "{colors.surface-2}"
    textColor: "{colors.t1}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  button-secondary-hover:
    backgroundColor: "{colors.surface-3}"
    textColor: "{colors.t1}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.t2}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  button-danger:
    backgroundColor: "rgb(241 72 79 / 0.14)"
    textColor: "{colors.rec-text}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  input:
    backgroundColor: "rgb(255 255 255 / 0.03)"
    textColor: "{colors.t1}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  chip:
    backgroundColor: "transparent"
    textColor: "{colors.t2}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 10px"
  navigation:
    backgroundColor: "{colors.deep}"
    textColor: "{colors.t2}"
    height: "40px"
    padding: "0 14px"
  share-panel:
    backgroundColor: "rgb(255 255 255 / 0.03)"
    textColor: "{colors.t1}"
    rounded: "{rounded.panel}"
    padding: "16px"
---
# Design System: Kielbox web

## Overview

The supplied Kielbox HTML references are the visual authority. Preserve their desktop palette, compact controls and media-first composition. This document records the implemented system, not a new visual direction.

The authenticated cloud library uses **Operate** mode. The landing page uses **Persuade** mode, with larger headings and more space while retaining the same type and green accent. No separate product or brand doctrine is inferred.

**Key Characteristics:**

- Self-hosted Geist and Geist Mono.
- Dark inset app layout with compact controls.
- Unboxed thumbnail captions and wrapping detail columns.
- Static landing and public share pages without runtime JavaScript.

## Colors

The palette follows the desktop theme in `crates/desktop/src/rune_theme.rs`. The frontmatter records the implemented CSS values from `src/styles.css`.

### Primary

Green accent identifies primary actions, active share status, caret and focus. The lighter green is the primary hover state; dark green supplies readable text on accent buttons.

### Neutral

Deep charcoal backs the viewport. The inset main area uses the first surface tone; controls use the higher surface tones. Foreground, secondary and muted text establish three levels of emphasis. Translucent white supplies borders and rings. Red and pale red identify destructive actions and revoked links; yellow is the busy-state token.

## Typography

Geist and Geist Mono are self-hosted variable fonts with system fallbacks. Use Geist for interface text and Geist Mono for measurements, formats and share URLs. The title, body, label and landing display roles are recorded above. Library capture titles use weight 500; metadata uses 12.5px type with an 18px line height. Landing body text uses 16px type with a 24px line height; introductory copy uses 18px with 28px leading.

## Layout

The app header is 40px tall. Main content is inset 8px and uses 22px top padding, responsive 16px to 32px horizontal padding and 32px bottom padding. At 600px and below, padding becomes 18px 16px 28px and the library search occupies its own row.

Library thumbnails use a 16:9 ratio. The grid fills available width with columns at least 280px wide, shrinking to the viewport on phones. Gaps are 26px vertically and 16px horizontally. Captions sit below the image without a surrounding box. Format and duration labels overlay the bottom corners.

Detail media and the share panel use wrapping flex columns with 560px and 340px bases and a 24px gap. The share panel takes the full width on phones. Public share pages have a 960px maximum measure, media before the title, a conditional download action and a quiet footer.

The landing page uses a centered 1184px container with responsive 16px to 32px side padding. Its preview caps at 1040px. Sections use generous 96px vertical padding; the hero starts with 72px top padding. Mobile hero padding reduces to 40px above and 48px below.

## Elevation & Depth

Tonal differences establish the main layers. Thin translucent outlines separate thumbnails, controls and panels. Selected chips and download actions add compact shadows; floating UI uses the compound shadow recorded in the sidecar. The landing recording screenshot uses a broad dark shadow. Keep this distinction between app controls and marketing screenshots.

## Shapes

Controls and cloud thumbnails have 6px corners. Media uses 8px corners; the inset main area has 10px top corners. The share panel has 10px corners. The landing hero screenshot uses 12px corners; gallery screenshots use 8px corners. Overlays clip to the thumbnail.

## Components

### Buttons

Cloud buttons are compact, 28px tall with 10px horizontal padding. Primary uses green with dark text; secondary uses the second surface and bright text; ghost uses secondary text; danger uses translucent red with pale red text. Hover changes tone. Disabled buttons reduce opacity and suppress interaction. Pressed controls move down 1px. Landing calls to action are 44px tall with 20px horizontal padding and 10px corners.

### Chips

Cloud filter chips use quiet text on a transparent background. Selected chips gain a translucent white fill, outline and compact shadow. Keep the exclusive selection behavior supplied by Base UI.

### Cards / Containers

Library entries group a thumbnail and caption without an enclosing panel. The share panel uses 16px padding, a slightly brighter translucent fill and a thin outline. Keep the landing page's marketing copy and screenshots from the updated `Kielbox Cloud (4).html` reference. The recording screenshot is the hero; the library, editor and tools screenshots appear in the gallery. Marketing images are self-hosted by `site/` in `/landing/`; reserve their native 1333 × 800 size and lazy-load the gallery.

### Inputs / Fields

Cloud fields are 28px tall with 10px horizontal padding, 6px corners, a translucent white fill and thin ring. Focus uses the accent ring. Labels use muted text. Share URLs use the mono face. Inline rename supports Enter and Escape.

### Navigation

The compact brand sits in the app header, opposite the authenticated identity. The landing header wraps its links on narrow screens. `/` is the authenticated library; the static marketing page lives in the separate `site/` project at `https://box.kieler.dev`; `/items/:itemId` contains capture details. The separate share origin handles `/s/:token` and unavailable links.

### Capture and share controls

Preserve optimistic favorite feedback, pending controls, search recovery and one-time share URL presentation. Base UI supplies switches, exclusive toggle groups, confirmation dialogs and toasts. Cloud reduced-motion rules disable animation and transitions; landing reduced-motion rules disable smooth scrolling.

## Do's and Don'ts

### Do:

- Do preserve the supplied references as the visual authority.
- Do keep native media controls, keyboard focus and reduced-motion behavior.
- Do include loading, empty, no-results, failed-image and unavailable-share states.

### Don't:

- Don't add a containing card behind library thumbnail captions.
- Don't treat landing screenshots as uploaded cloud captures.
- Don't require runtime JavaScript for landing or public share pages.
