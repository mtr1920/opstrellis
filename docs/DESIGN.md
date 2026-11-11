# OpsTrellis design system: "Native"

OpsTrellis should feel like a native macOS app: in the spirit of System Settings, Finder, Raycast and
Linear on macOS. It replaces "Quiet Glass". Users found that centred column wasteful on wide windows,
its 64px header band heavy, and the window un-Mac-like: an opaque sidebar under a plain grey title bar,
and no way to pick a theme.

## Hard rules (checked by `e2e/design_check.mjs`)

1. **No gradients, glows or decorative imagery.** `grep -riE "gradient" src/ --include=*.css --include=*.tsx`
   returns nothing. Glass means the window's native vibrancy plus a translucent toolbar, never blobs or
   orbs. Empty states are a short sentence plus one button.
2. **One accent colour, used sparingly.** It appears only on the primary button, the focus ring (also outlining a drop target), links,
   toggles when on, the selected sidebar icon and selection tint, the chart fill, and the recording dot
   (`--rec`, red). Every other surface is neutral.
3. **Native window and floating glass sidebar.**
   - On macOS the title bar is an overlay, and the whole window shows the native `sidebar` vibrancy
     material. The macOS settings live in `src-tauri/tauri.macos.conf.json`.
   - The sidebar is a floating glass panel in the spirit of macOS Tahoe: inset 8px, radius 16, a
     translucent `--panel` tint with backdrop blur, a bright 1px `--panel-edge` and a soft
     `--panel-shadow`. The traffic lights sit inside it.
   - The content is a second floating panel: radius 16, inset 8px from the top, right and bottom,
     and 8px from the sidebar. Every gap around and between the two panels is 8px. On macOS its fill
     is the translucent `--pane-glass`, and its cards are frosted `--card-glass`.
   - Linux keeps a normal decorated window. The panels float over a solid `--window` colour there.
4. **One toolbar row.** Each page has a single 52px sticky toolbar.
   - It holds the title (left), tabs, and actions (right). The title is level with the sidebar's top
     strip, and the first nav item starts at the toolbar's bottom edge.
   - It is translucent (`--toolbar` with backdrop blur) because content scrolls under it.
   - Toolbar controls are capsules: buttons, segmented controls and the search field use `--capsule`.
5. **Use the width.** Content fills the pane edge to edge with equal 24px gutters on both sides. There
   is no max width and no centred column.
6. **2px grid.** Spacing and radii are even numbers; 1px hairlines are the exception.
7. **Shared edges.** The toolbar title's left edge equals the left edge of every card on the page.
   Text columns inside list rows align between rows.

## Themes

Appearance is a setting: Settings → General → Appearance (System, Light or Dark), which is the first
row Settings shows. A sun/moon button beside Settings in the sidebar flips light ↔ dark from anywhere. It is stored as
`theme` in the Rust settings, with "system" as the default.
- The frontend sets `data-theme="light|dark"` on `<html>`; "system" follows `prefers-color-scheme`.
  It is also kept in `localStorage`, so the next launch paints the right theme before settings load.
- The switch is instant. The page calls the window's `setTheme` itself, so the native vibrancy changes
  in the same frame, without waiting for the settings save. Transitions are suspended for that one
  frame, so every surface changes at once.
- On save, the backend also calls `set_theme` on every window and emits `theme-changed`, so the
  overlay pill follows.

## Tokens

```css
:root {
  --bg: #f7f7f8;                        /* content pane */
  --surface: #ffffff;                   /* cards */
  --surface-2: #f0f0f2;                 /* tracks, kbd, search field */
  --line: rgba(0, 0, 0, 0.09);          /* hairlines; translucent so they read on any surface */
  --line-strong: rgba(0, 0, 0, 0.16);   /* control borders */
  --window: #e6e6ea;                    /* behind the panel where there is no vibrancy */
  --panel: rgba(255, 255, 255, 0.52);   /* floating sidebar glass, + blur(30px) */
  --panel-edge: rgba(255, 255, 255, 0.75);
  --hover: rgba(0, 0, 0, 0.05);
  --selected: rgba(59, 91, 219, 0.12);  /* tinted fill of the selected sidebar / section item */
  --text: #1d1d1f; --text-2: #4f4f56; --muted: #6e6e76;
  --accent: #3b5bdb; --accent-text: #2f4ac0; --accent-soft: #e9edfb;
  --toolbar: rgba(247, 247, 248, 0.8);  /* + backdrop blur */
  --radius: 12px; --radius-sm: 8px; --radius-xs: 4px; --capsule: 14px;
  --ease: cubic-bezier(0.2, 0.8, 0.2, 1); --fast: 0.12s;
  --toolbar-h: 52px; --gutter: 24px;
}
:root[data-theme="dark"] {
  --bg: #1c1c1e; --surface: #252527; --surface-2: #2e2e31;
  --line: rgba(255, 255, 255, 0.08); --line-strong: rgba(255, 255, 255, 0.16);
  --window: #111113; --panel: rgba(46, 46, 50, 0.5); --panel-edge: rgba(255, 255, 255, 0.12); --hover: rgba(255, 255, 255, 0.06); --selected: rgba(120, 146, 255, 0.2);
  --text: #f2f2f4; --text-2: #b8b8c0; --muted: #8e8e96;
  --accent: #5b7cfa; --accent-text: #9db0ff;
  --toolbar: rgba(28, 28, 30, 0.8);
}
```

Glass falls back to solid surfaces without `backdrop-filter` or when `prefers-reduced-transparency` is set.

## Type (the system font, SF Pro, on macOS; Inter elsewhere; tabular numbers on all figures)

| Use | Size / line height / weight |
|---|---|
| Toolbar title | 16 / 20 / 700 |
| Section heading | 14 / 20 / 650 |
| Body, sidebar | 13 / 20 / 400 (labels 500) |
| Small and meta | 12 / 16 / 400–500 |
| Stat value | 28 / 32 / 650 |
| Section nav group label | 12 / 16 / 600, sentence case, `--muted` |

## Layout and controls

- **Sidebar.** A 220px floating panel, padding 0 8px 8px.
  - The top strip is 52px, the toolbar's height, and a window drag region centred on the toolbar title. On macOS it holds
    the traffic lights on the left and the logo and name on the right; on Linux the logo and name
    sit on the left.
  - Nav items are 28px tall with a 2px gap, radius 6, 16px icons in `--text-2` with an 8px gap.
  - The selected item has a `--selected` tinted fill, with its icon in `--accent-text`.
  - The footer holds Settings and a one-line status row (dot, state, shortcut).
- **Toolbar.** 52px, a drag region. The page title is on the left. Page groups (Notes, Library) put a
  segmented control next to it. Actions sit on the right, and the search field shrinks first on
  narrow windows.
- **Cards.** `--surface`, 1px `--line`, radius 12, no shadow. Rows are at least 52px tall with 12px × 16px
  padding. The row text keeps at least 200px, and the control wraps below it if needed. Empty cards
  and their sections are hidden.
- **Controls.** All 28px tall. Buttons are capsules; inputs and selects have radius 8.
  - Primary buttons are solid accent. Secondary buttons are `--surface` with a `--line-strong` border.
    Ghost and icon buttons are transparent with a `--hover` fill.
  - Inputs and selects have a `--line-strong` border and a 2px accent focus ring. The search field is
    a borderless `--surface-2` well.
  - Segmented controls are a 28px recessed `--surface-2` track with a raised 24px thumb.
  - Toggles are 32 × 20.
  - Kbd caps are 20px tall.
- **Lists** (history, dictionary, snippets, actions, apps) highlight the hovered row with `--hover`.
  History row actions float in a small solid toolbar at the row's top right on hover or focus.

## Large screens

- **≥ 1440px:**
  - The gutter grows to 32px and body text to 14px. The toolbar title is 18px and section headings
    are 15px.
  - Rows, cards and stats get 20px padding, and stat values are 32px.
  - Insights puts Activity and Where you dictate side by side.
  - The toolbar stays 52px at every width, because the native traffic lights can't move.
- **≥ 1680px:** Settings keeps its reading order top to bottom. Only two short pairs sit side by
  side: Voice input with Text insertion, and Account with About. Paired cards share one height,
  and their rows spread evenly to fill it. Every other section spans the full width.
- **≥ 1920px:** the gutter grows to 40px.

## Motion

Smooth, never showy. Everything uses `--ease`; nothing bounces.
- Interactive controls fade background, border, colour and shadow over `--fast`. Buttons press to 0.98.
- A page's content fades up 4px on open (180 ms). Modals pop in (180 ms); toasts rise 8px (200 ms).
- `prefers-reduced-motion` turns all of it off.

## Screens

- **Home.**
  - A hint card: "Hold <kbd>⌃</kbd><kbd>⇧</kbd><kbd>Space</kbd> to dictate · tap for hands-free".
    Command mode and Paste last are right-aligned and hidden on narrow windows.
  - A stat strip: three equal columns with hairline dividers.
  - History grouped by day, with a 240px search field beside the heading. Each row has a 64px time
    column, the text, and a meta line.
- **Insights.** A stat strip, a heatmap (12px cells, 4px gaps, stepped accent opacities
  0.15/0.35/0.6/1), and category bars (8px accent on a `--surface-2` track).
- **Style.** Tone options are equal-width cards. The selected one has an accent border, an
  `--accent-soft` fill and a check.
- **Settings.**
  - A 200px section nav sits flush with the content pane's left edge, separated by a hairline. It is sticky, and its
    labels align with the toolbar title. Below 1060px it is hidden and the cards take the full width.
  - Groups:
    - App: General (Appearance first)
    - Dictation: Shortcuts, Voice input, Text insertion, Permissions
    - Speech & AI: Transcription, Local models, AI polish, Workspaces
    - Account & privacy: Privacy, Agents, Backup, Account, About (with updates and Logs)
- **Keyboard.**
  - ⌘1–⌘5 (Ctrl on Linux) open the sidebar pages, ⌘, opens Settings, and ⌘F focuses the page's search.
    They match physical keys, so they work on any layout, and they do nothing while a dialog is open.
  - Escape closes a dialog, except while the shortcut recorder is capturing.
- **Overlay pill.**
  - 44px high, radius 22, translucent and blurred with a 1px hairline. It follows the theme: a light
    pill in light mode, a dark pill in dark mode.
  - The width follows the state over 160 ms:

    | State | Width |
    |---|---|
    | Push-to-talk | 240 |
    | Hands-free | 280 |
    | Transcribing | 200 |
    | Outcome | fits text, ≤ 344 |
    | Recording with a workspace chip | + 64 (304 / 344) |
    | Answering an agent's question | 320 (hands-free or with a chip: 344) |

  - An agent's question uses the review card's window and look: "AGENT · Agent asks", the whole
    question wrapped at 12/18 (up to 300 characters, 10 lines), the hint below; an outcome meanwhile
    sits right-aligned in the card's head. Every overlay state is anchored 10px above the window's bottom.

  - The lattice is 2px dots on a 4px pitch. Only their opacity changes.
  - No colour other than the red dot.
  - Workspace chip (recording in a workspace): the folder name in a quiet 16px chip like the CMD badge
    (`--pill-soft` fill, `--pill-dim` text, 11px, at most 56px wide with an ellipsis). The pill grows by 64px
    for it, so the live text keeps its room.
  - Review card (command mode): the pill grown upward into a 420px card, up to 260px high, radius 12, with the same
    glass and hairline. A CMD badge, "Replace selection?" and the length; the rewrite (13/20, at most 9 lines);
    then "↵ Insert" and "Esc Discard" with kbd caps in pill colours.
