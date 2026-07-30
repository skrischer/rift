# Design contract

> How UI design surfaces are produced and reviewed inside the planning cycle.
> Read by `/loopkit:design` and the planner's Step-3 design-surface step for the
> tool specifics (medium, where designs live, reviewer, handoff form). Normative
> for the design step's mechanics; non-normative for code.

## Medium

Paper (`app.paper.design`) via the Paper MCP server — designs are composed on a
2D canvas as artboards. No local design source files (no committed `.fig` /
`.sketch`); the shared Paper file is the durable home for every rift surface.

- File: `rift` (Paper file id `01KTZZQ3CGGMPQXSTRVFBS5CTY`), Page 1.
- Design system: **Catppuccin Mocha** palette (base `#1E1E2E`, mantle `#181825`,
  crust `#11111B`, surface `#313244`, text `#CDD6F4` / subtext `#A6ADC8`, accent
  blue `#89B4FA`, mauve `#CBA6F7`, green `#A6E3A1`, peach `#FAB387`, red
  `#F38BA8`); fonts **Inter** (labels / prose / buttons) + **JetBrains Mono**
  (paths / values / terminal / meta). No design tokens are defined in the file —
  read exact values from the existing artboards via `get_computed_styles` /
  `get_jsx`, never from a screenshot.

## Where designs live

One artboard (or a small set) per phase/surface, named `<Phase/feature> — <what>`
(e.g. `Phase 47 — Project-optional session flows`). Exploratory / sparring
artboards carry a `(sparring)` / `(exploratory)` suffix and are explicitly NOT
the durable contract. The shipped product artboards — `Cockpit — IDE`,
`Connection — Startup`, `Explorer — Redesign`, `rift — Session management`,
`rift — Session flows`, `Styleguide` — are the visual contract new work extends
and must match.

## Reviewer

- The **human** (the developer) reviews the design interactively during planning
  — the sparring / correction dialog IS the review, folded into the
  spec-acceptance gate. No separate design gate.
- The `paper-reviewer` agent optionally compares a Paper artboard against the
  live DOM once the surface is implemented — a post-implementation parity check,
  not a planning gate.

## Handoff form

The durable artifact is the **Paper artboard itself**, referenced from the spec
by file + artboard name (and URL). The spec's Prior decisions / Verification
cite the artboard as the visual contract; the implementer reads the artboard for
exact layout and values. No design file is committed into the repo — the Paper
file is the single source. A static PNG export into `docs/design-assets/` is
optional (offline review only), never required.

## Rules

- No emojis in any design (`docs/constitution.md`: "No emojis in code or UI").
  Icons are inline SVG.
- Match the existing system before inventing: reuse the Styleguide's palette,
  type scale, radii, and component shapes.
