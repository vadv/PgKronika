---
name: PgKronika
description: A dense forensic cockpit for PostgreSQL and operating-system evidence on one honest time axis.
colors:
  canvas: "#0d1117"
  surface: "#161b22"
  overlay: "#1c2129"
  rule: "#30363d"
  rule-strong: "#444c56"
  text: "#c9d1d9"
  text-strong: "#e6edf3"
  text-muted: "#8b949e"
  evidence: "#58a6ff"
  evidence-strong: "#79c0ff"
  heat-low: "#1f6feb"
  heat-mid: "#3090ff"
  healthy: "#3fb950"
  warning: "#d29922"
  critical: "#f85149"
  critical-strong: "#ff7b72"
typography:
  headline:
    fontFamily: "JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: 1.2
  title:
    fontFamily: "Inter, ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: 1.45
  body:
    fontFamily: "Inter, ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.45
  label:
    fontFamily: "Inter, ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "11px"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "0.04em"
  technical:
    fontFamily: "JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.45
rounded:
  bucket: "2px"
  sm: "4px"
  md: "6px"
  lg: "8px"
spacing:
  1: "4px"
  2: "8px"
  3: "12px"
  4: "16px"
  6: "24px"
components:
  button-compact:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "2px 8px"
  button-compact-hover:
    backgroundColor: "rgb(177 186 196 / 0.08)"
    textColor: "{colors.text-strong}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "2px 8px"
  metric-toggle:
    backgroundColor: "transparent"
    textColor: "{colors.text-muted}"
    typography: "{typography.label}"
    padding: "2px 8px"
  metric-toggle-selected:
    backgroundColor: "rgb(88 166 255 / 0.12)"
    textColor: "{colors.evidence-strong}"
    typography: "{typography.label}"
    padding: "2px 8px"
  matrix-container:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    typography: "{typography.technical}"
    rounded: "{rounded.md}"
    width: "100%"
  table-row-selected:
    backgroundColor: "rgb(88 166 255 / 0.12)"
    textColor: "{colors.text}"
    typography: "{typography.technical}"
    height: "27px"
  temporal-bucket:
    backgroundColor: "{colors.heat-low}"
    rounded: "{rounded.bucket}"
    height: "12px"
    width: "4px"
  status-chip-warning:
    backgroundColor: "rgb(210 153 34 / 0.16)"
    textColor: "{colors.warning}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "2px 8px"
  statements-context:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text-muted}"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "4px 8px"
    height: "24px"
  health-line:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    padding: "4px 8px"
    height: "60px"
---

# Design System: PgKronika

## Overview

**Creative North Star: "The Forensic Cockpit"**

PgKronika is a matte, near-black evidence board built for concentrated incident replay. PostgreSQL and operating-system signals share one time geometry, while rules, labels, quality states, and provenance keep temporal coincidence from masquerading as cause. The interface is compact, technical, and calm even when the evidence is not.

The system favors one continuous analytical field over dashboard-card sprawl. Crisp ruled rows, small labels, monospaced values, restrained evidence blue, and explicit semantic color let the operator scan from window health to workload shape, ranking, lens, search, and detail without decoration displacing evidence. The dark theme is the canonical visual world; the implemented light token map preserves the same semantic roles when an alternate theme is required.

**Key Characteristics:**

- Matte near-black surfaces separated by tonal steps and crisp 1px rules.
- Shared time geometry with provenance, gaps, and uncertainty kept visible.
- Compact sans-serif chrome paired with monospaced technical evidence.
- Restrained blue interaction and temporal cues beside semantic green, amber, and red.
- Viewport-owned desktop density with a ranked matrix as the dominant working surface.

## Colors

The canonical palette is a blue-black neutral field with one cool evidence accent and a deliberately separate semantic status triad.

### Primary

- **Evidence Blue:** Used for selected rows, active metrics, sparklines, baselines, cursors, and low-intensity heat evidence.
- **Bright Evidence Blue:** Used where small active text or a thin cursor needs greater contrast against raised graphite.

### Secondary

- **Healthy Green:** Reserved for complete, healthy, and positive-delta evidence.
- **Warning Amber:** Reserved for incomplete coverage, warnings, and medium heat.
- **Critical Red:** Reserved for critical verdicts, error states, and negative deltas; the brighter companion is used for compact foreground text and peak heat.

### Neutral

- **Void Canvas:** The root analytical field beneath every surface.
- **Raised Graphite:** The shared substrate for the Health Line, controls, sticky cells, and ranked matrix.
- **Overlay Slate:** The highest tonal surface for transient overlays.
- **Grid Rule / Strong Grid Rule:** Quiet structural separators and emphasized sticky headers or hover boundaries.
- **Steel Text / Evidence White / Muted Slate:** The body, emphasized, and secondary text hierarchy.

### Named Rules

**The Evidence-Not-Decoration Rule.** Blue marks selection, interaction, or temporal evidence; it does not become an ambient panel fill or ornamental gradient.

**The Semantic Status Rule.** Green, amber, and red keep their health meanings and are reinforced by text, glyphs, counts, or hatching so color is never the only carrier.

## Typography

**Display Font:** JetBrains Mono (with system monospace fallbacks)  
**Body Font:** Inter (with system sans-serif fallbacks)  
**Label/Mono Font:** Inter for chrome labels; JetBrains Mono for identifiers, measures, timestamps, and technical payloads

**Character:** Inter keeps dense controls compact and neutral. JetBrains Mono makes changing measurements, query identifiers, database roles, counts, and time boundaries align as evidence rather than read as prose.

### Hierarchy

- **Headline** (600, 20px, 1.2): The compact Health Line score; this size is not a general page-hero treatment.
- **Title** (600, 16px, 1.45): Page and surface titles where the normal data scale needs one clear step up.
- **Body** (400, 13px, 1.45): Default UI copy, explanations, and normal chrome.
- **Technical** (400, 13px, 1.45): Table values and machine-facing evidence.
- **Label** (600, 11px, 0.04em, uppercase when naming a region): Dense headers, column labels, quality captions, and persistent chrome.

### Named Rules

**The Two-Voice Rule.** Use Inter for interface language and JetBrains Mono for values whose alignment, provenance, or machine identity matters.

## Layout

Desktop is a height-owned flex column rather than a scrolling document. At 761px and wider, the root fills the viewport and hides root overflow; analytical regions own their own scrolling. The content stack uses an 8px gap and 8px-by-12px inset, a fixed 60px Health Line, and a compact 72px screen-context band before the evidence surface. Non-Statements analytical centers use a fixed 156px evidence band above the ranked table.

The Statements surface is deliberately wider than compact desktop viewports: the matrix has a 1,420px minimum width, a 272px sticky identity column, a timeline of at least 620px, and 96 buckets whose minimum cells are 4px by 12px with 1px gaps. Matrix rows are 27px high and general ranked rows are 28px high; virtualization and the matrix's own overflow preserve density without extending the page.

At 760px and below, the viewport lock releases into a vertical triage flow, persistent desktop navigation yields, and temporal rows use 48 buckets. Mobile preserves evidence meaning and drill-down, but does not pretend the full desktop forensic board can be compressed unchanged.

**The Viewport Ownership Rule.** On desktop, keep investigation-critical context and the ranked evidence surface inside the first viewport; put overflow inside the matrix or detail surface, never on the root page.

## Elevation & Depth

The system is flat by default. Canvas, raised surface, and overlay tones establish hierarchy, while 1px rules make dense rows and controls legible. The pop shadow (`0 8px 24px rgb(0 0 0 / 0.45), 0 0 0 1px rgb(240 246 252 / 0.06)`) belongs to overlays only; everyday panels, rows, and the Health Line remain unshadowed. Focus uses a 2px evidence-blue ring as an accessibility state, not as depth.

### Shadow Vocabulary

- **Overlay Pop** (`0 8px 24px rgb(0 0 0 / 0.45), 0 0 0 1px rgb(240 246 252 / 0.06)`): Transient layers over the dark cockpit.
- **Light Overlay Pop** (`0 8px 24px rgb(140 149 159 / 0.3), 0 0 0 1px rgb(27 31 36 / 0.06)`): The equivalent transient-layer separation in the alternate light theme.

### Named Rules

**The Structural Depth Rule.** Use tonal layering and rules for persistent surfaces; reserve cast shadows for overlays that truly sit above the evidence plane.

## Shapes

Corners are gently squared: 4px for controls and focus targets, 6px for primary evidence containers, and 8px only for larger overlay or shell forms. Temporal buckets tighten to 2px so adjacent samples read as a continuous signal. Borders are crisp and usually 1px; small radii never turn dense controls into pills.

**The Ruled Board Rule.** Preserve straight runs, aligned row edges, and modest corner radii; rounded-card mosaics do not belong in this cockpit.

## Components

### Buttons

- **Shape:** Compact, gently squared controls (4px radius) with 2px-by-8px internal padding.
- **Primary:** Evidence actions use raised graphite, steel text, and a 1px grid rule rather than a large filled call-to-action.
- **Hover / Focus:** Hover shifts to the neutral wash over 120ms ease-out and strengthens the border; keyboard focus uses the shared 2px evidence-blue ring.
- **Metric Toggle:** Unselected text is muted on a transparent segment; `aria-pressed` selection uses the blue active wash and bright evidence text.

### Chips

- **Style:** Compact label-sized text on subtle semantic washes, with a small radius and optional border.
- **State:** Warning, critical, and healthy chips retain explicit words or counts; a colored dot or tint is supporting evidence, never the label itself.

### Cards / Containers

- **Corner Style:** Primary evidence containers use the medium 6px radius.
- **Background:** Raised graphite over the void canvas; overlay slate is reserved for transient layers.
- **Shadow Strategy:** Flat for persistent surfaces; see the Structural Depth Rule.
- **Border:** One crisp grid rule, strengthened only for headers, focus, or hover.
- **Internal Padding:** Built from the 4px rhythm, usually 8px or 12px.

### Inputs / Fields

- **Style:** Dark canvas inside a 1px grid rule with compact body text and a 4px radius.
- **Focus:** The shared evidence-blue focus ring replaces decorative glow.
- **Error / Disabled:** Use explicit status text plus semantic foreground; unavailable and missing states remain muted but readable.

### Navigation

Navigation is a compact text rail, grouped by investigation domain. The active destination uses evidence blue and a thin underline or active wash; unavailable destinations remain visibly present with a muted or dotted treatment instead of disappearing. Keyboard shortcuts and state labels are part of the chrome, not supplemental decoration.

### Health Line

The Health Line is a fixed 60px signature surface that combines score, evidence-quality summary, verdict ribbon, load trace, events, selection, cursor, and time readout on one horizontal axis. Calm buckets recede, warning and critical buckets saturate, gaps use a ruled and hatched substrate, and a forming tail is visibly hatched. Keep provenance and source-quality states adjacent to the score.

### Ranked Time Matrix

The ranked matrix is the dominant evidence container. Sticky headers and identity cells maintain orientation; numeric values align in monospace; selection uses a pale blue wash plus a 2px inset evidence bar; hover uses a neutral wash. Zero, missing data, not-retained data, and unavailable classifications must remain visually and semantically distinct.

### Temporal Bucket Row

Rows use evenly distributed, tightly spaced buckets with a blue-to-amber-to-red heat sequence. Missing buckets use a muted substrate, the baseline is a dashed muted rule, and the active cursor is a solid bright-blue rule outlined against the raised surface.

### Named Rules

**The One-Axis Rule.** Temporal components may layer PostgreSQL, OS, event, and workload evidence only when they share an explicit time geometry and retain their own provenance.

**The State-Is-Evidence Rule.** Loading, warming, partial, empty, missing, unavailable, selected, and expired states receive intentional treatments; none may collapse into a silent blank cell.

## Do's and Don'ts

### Do:

- **Do** keep desktop investigation-critical context inside the viewport and place long evidence inside component-owned scrolling regions.
- **Do** use the 4px spacing rhythm, 1px ruled separators, and compact 11–13px text scale to preserve expert density.
- **Do** pair semantic color with labels, counts, markers, or patterns, especially for warnings, critical states, gaps, and selection.
- **Do** keep query identifiers, timestamps, measurements, and database or role metadata monospaced.
- **Do** preserve zero, missing, not-retained, partial, and unavailable as distinct evidence states.

### Don't:

- **Don't** split one investigation surface into a mosaic of decorative dashboard cards.
- **Don't** use gradients, large accent fields, or generic blue decoration where no evidence or interaction is being encoded.
- **Don't** promote amber, red, or green to brand decoration; they are reserved for semantic status.
- **Don't** add generous marketing-scale whitespace, hero typography, or oversized controls to the operational cockpit.
- **Don't** imply causality through proximity, shared color, or a merged score when the implementation only establishes temporal coincidence.
