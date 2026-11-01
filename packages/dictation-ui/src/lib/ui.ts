// The owner's class joiner (`@rightkit/react-shell/react`); the app keeps no copy.
import { cx } from "@rightkit/react-shell/react";

export { cx };

const focusRing = "focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";

export const ui = {
  // Unified chrome: the shell bg is the coffee surface; the titlebar (full width)
  // and the sidebar (left) BOTH inherit it with no seam between them - one L-shape.
  // The main content is a darker inset well nested into that chrome.
  appShell:
    "grid h-screen w-screen max-w-screen overflow-hidden rounded-[var(--radius-lg)] bg-[var(--sidebar)] [box-shadow:inset_0_0_0_1px_rgba(255,255,255,0.04)] [grid-template-areas:'titlebar_titlebar'_'sidebar_main'] [grid-template-columns:208px_1fr] [grid-template-rows:40px_1fr] max-[720px]:rounded-none max-[720px]:[grid-template-areas:'titlebar'_'main'] max-[720px]:[grid-template-columns:1fr] max-[720px]:[grid-template-rows:40px_1fr]",
  // Canvas: warm espresso content well; rounded top-left corner contrasts with shell.
  main: "[grid-area:main] flex min-w-0 flex-col overflow-hidden bg-[var(--canvas,var(--surface-1))]",
  // One shared page container capped at ~1320px, left-aligned on every page.
  pageContainer: "w-full max-w-[1320px] mr-auto",
  // A page is a fixed full-width hero band + a SEPARATE scroll region below it.
  // The hero does NOT scroll (shrink-0), so the scroll region's native scrollbar
  // starts UNDER the hero's bottom edge and rides the true right corner of
  // <main> (no padding gutter). Content padding lives inside the scroll region.
  // `pageStage` is the non-scrolling flex column that holds hero + scroll;
  // `pageScroll` is the scroll region.
  pageStage: "flex min-h-0 flex-1 flex-col",
  pageScroll:
    "min-h-0 flex-1 overflow-y-auto overscroll-none px-[var(--space-5)] pb-[var(--space-6)] pt-[var(--space-4)] max-[720px]:px-[var(--space-4)]",
  hubLoading:
    "flex h-screen items-center justify-center bg-[var(--hr-ink)] [font-family:var(--font-display)] text-[length:var(--size-xl)] text-[var(--paper)]",

  pageTitle:
    "m-0 mb-[var(--space-2)] [font-family:var(--font-display)] text-[length:var(--size-3xl)] font-semibold leading-[1.12] [letter-spacing:0] [text-wrap:balance]",
  pageTitleLarge: "text-[length:var(--size-3xl)]",
  titleDot: "font-semibold italic text-[var(--accent)]",
  pageSubtitle:
    "m-0 mb-[var(--space-4)] text-[length:var(--size-md)] text-[var(--muted)] [text-wrap:pretty]",

  card: "mb-[var(--space-3)] min-w-0 rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] p-4 [box-shadow:var(--surface-shadow)] transition-[box-shadow,background-color,border-color] duration-150 ease-[var(--ease-out-strong)]",
  // Same surface language as `card` but without the bottom margin, for rows
  // that live inside a flex/grid gap instead of stacking with mb-* spacing
  // (tables, list wrappers). Every bordered surface in the app should trace
  // back to one of these two so no page invents its own radius/border/shadow.
  surface:
    "min-w-0 rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] transition-[background-color,border-color] duration-150 ease-[var(--ease-out-strong)]",
  // Section headers - one clear step larger and stronger than row labels,
  // with consistent spacing.
  sectionHeader:
    "m-0 text-[16px] font-semibold leading-[1.3] text-[var(--paper)] tracking-[-0.01em]",
  // Subheading inside a section: one step below sectionHeader.
  sectionLabel:
    "m-0 text-[length:var(--size-base)] font-semibold text-[var(--muted)]",
  // Round icon badge - the one way an icon gets a background chip (dropzone,
  // empty states, recent-item glyphs). Matches the accent-tinted circle used
  // for Speech-recognition Pro chips and the Transcribe dropzone icon.
  iconBadge:
    "grid shrink-0 place-items-center rounded-full border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[var(--surface-3)] text-[var(--accent)]",
  pageHero:
    "rounded-[var(--radius-lg)] border border-[color-mix(in_srgb,var(--hero-accent),transparent_62%)] bg-[var(--hero-surface)] p-[var(--space-4)] text-[var(--hero-ink)] [box-shadow:0_18px_42px_-32px_rgba(0,0,0,.72),inset_0_1px_0_color-mix(in_srgb,var(--hero-ink),transparent_88%)]",
  pageHeroSplit:
    "grid overflow-hidden rounded-[var(--radius-lg)] border border-[color-mix(in_srgb,var(--hero-accent),transparent_62%)] bg-[var(--hero-surface)] text-[var(--hero-ink)] [box-shadow:0_18px_42px_-32px_rgba(0,0,0,.72),inset_0_1px_0_color-mix(in_srgb,var(--hero-ink),transparent_88%)]",
  pageHeroEyebrow:
    "inline-flex items-center gap-[7px] rounded-[7px] bg-[color-mix(in_srgb,var(--hero-accent),transparent_82%)] px-[10px] py-[5px] text-[length:var(--size-xs)] font-semibold text-[var(--hero-accent)]",
  // Hero copy sits on the dark hero-surface band (PageShell), so it uses the
  // always-light --hero-ink token, not the themed --paper.
  pageHeroText: "text-[color-mix(in_srgb,var(--hero-ink),transparent_12%)]",
  pagePanel:
    "rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_8%)] bg-[var(--surface-2)] p-[var(--space-4)] text-[var(--paper)] [box-shadow:var(--surface-shadow)]",
  pageSubPanel:
    "rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_14%)] bg-[var(--surface-1)] text-[var(--paper)] [box-shadow:inset_0_1px_0_color-mix(in_srgb,white,transparent_92%)]",
  cardTitle:
    "m-0 mb-[var(--space-3)] text-[length:var(--size-md)] font-semibold [letter-spacing:0] [text-wrap:balance]",
  cardCopy: "m-0 text-[var(--muted)] [text-wrap:pretty]",
  cardCopyPaper: "text-[var(--paper)]",
  loadingPanel:
    "mb-[var(--space-4)] rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] p-[var(--space-4)] text-[var(--muted)] [box-shadow:var(--surface-shadow)]",
  empty:
    "flex flex-col items-center gap-[var(--space-2)] rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] px-[var(--space-5)] py-[var(--space-6)] text-center text-[var(--muted)] [box-shadow:var(--surface-shadow)] [text-wrap:pretty]",
  emptyIcon: "h-5 w-5 text-[var(--muted-2)]",

  button: `hr-btn inline-flex min-h-[38px] items-center justify-center gap-[var(--space-2)] rounded-[var(--radius-sm)] border-0 bg-[var(--accent)] px-[var(--space-4)] [font:inherit] font-semibold text-[var(--accent-contrast)] transition-[background-color,transform,box-shadow] duration-150 ease-[var(--ease-out-strong)] hover:bg-[var(--accent-hover)] active:scale-[0.96] disabled:cursor-not-allowed disabled:!bg-[var(--surface-3)] disabled:!text-[var(--muted-2)] disabled:scale-100 disabled:!opacity-100 max-[720px]:min-h-11 ${focusRing}`,
  buttonSecondary:
    "[border-width:1px] border-[color-mix(in_srgb,var(--line),transparent_12%)] bg-[var(--surface-3)] text-[var(--paper)] hover:bg-[var(--surface-hover)]",
  buttonGhost:
    "hr-btn-ghost [border-width:1px] border-[color-mix(in_srgb,var(--line),transparent_10%)] bg-transparent text-[var(--paper)] hover:bg-[var(--surface-hover)]",
  buttonDanger:
    "bg-[var(--danger)] text-white hover:bg-[color-mix(in_srgb,var(--danger),black_15%)]",
  buttonCompact: "min-h-8 px-3 text-[length:var(--size-xs)] max-[720px]:min-h-8",
  iconButton: `inline-grid h-11 w-11 place-items-center rounded-[var(--radius-sm)] border border-transparent bg-transparent text-[var(--paper)] transition-[background-color,border-color,transform,box-shadow] duration-150 ease-[var(--ease-out-strong)] hover:border-[color-mix(in_srgb,var(--line),transparent_10%)] hover:bg-[var(--surface-hover)] active:scale-[0.96] disabled:cursor-not-allowed disabled:opacity-45 disabled:scale-100 ${focusRing}`,
  buttonBetween: "w-full justify-between",

  input:
    "min-h-[38px] rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_12%)] bg-[var(--surface-1)] px-[var(--space-3)] py-[var(--space-2)] [font:inherit] text-[length:var(--size-md)] text-[var(--paper)] transition-[border-color,box-shadow,background-color] duration-150 ease-[var(--ease-out-strong)] hover:border-[color-mix(in_srgb,var(--line),transparent_0%)] focus:outline-none focus:[box-shadow:var(--focus-ring)] disabled:cursor-not-allowed disabled:opacity-60",

  actionRow:
    "flex min-w-0 items-start justify-between gap-[var(--space-3)] border-b border-[rgba(255,255,255,0.04)] py-3 last:border-b-0 max-[720px]:flex-col max-[720px]:items-stretch",
  actionLabel: "pt-[6px] text-[var(--paper)]",
  actionControl:
    "flex min-w-[180px] shrink-0 justify-end pt-[1px] max-[720px]:min-w-0 max-[720px]:justify-start",
  inlineControls:
    "flex flex-wrap items-center justify-end gap-[var(--space-1)] max-[720px]:w-full max-[720px]:justify-start",
  inlineControlsWide: "gap-[var(--space-3)]",
  keyHint:
    "rounded border border-[rgba(255,255,255,0.06)] bg-[var(--surface-1)] px-2 py-0.5 [font-family:var(--font-mono)] text-[length:var(--size-xs)] [font-variant-numeric:tabular-nums]",
  fieldNote:
    "m-0 mt-[var(--space-2)] text-[length:var(--size-xs)] text-[var(--muted)] [text-wrap:pretty]",
  fieldNoteGap: "mb-[var(--space-3)] mt-0",
  fieldNoteAfterLarge: "mt-[var(--space-3)]",

  historyRow:
    "mb-[var(--space-2)] flex items-start justify-between gap-[var(--space-3)] rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] px-[14px] py-3 [box-shadow:var(--surface-shadow)] transition-[background-color,border-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] hover:border-[color-mix(in_srgb,var(--line),transparent_0%)] hover:[box-shadow:var(--surface-shadow-hover)] max-[720px]:flex-col max-[720px]:items-stretch",
  historyText: "m-0 flex-1 select-text [text-wrap:pretty]",
  historyMeta:
    "whitespace-nowrap [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--muted)] [font-variant-numeric:tabular-nums]",
  historyDay:
    "mb-[var(--space-2)] mt-[var(--space-4)] text-[length:var(--size-xs)] font-semibold text-[var(--muted)]",

  muted: "text-[var(--muted)]",
  mutedSm: "text-[length:var(--size-sm)] text-[var(--muted)] [text-wrap:pretty]",
  mutedXs: "text-[length:var(--size-xs)] text-[var(--muted)]",
  dangerText: "text-[var(--danger)]",
  accentText: "text-[var(--accent)]",
  mono: "[font-family:var(--font-mono)]",
  monoPath: "[font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--muted)]",

  statusPill:
    "inline-flex items-center gap-[var(--space-2)] rounded-full border border-[var(--glass-edge)] [background:var(--glass-pill)] px-2.5 py-[3px] [font-family:var(--font-mono)] text-[length:var(--size-xs)] text-[var(--paper)] [font-variant-numeric:tabular-nums] [backdrop-filter:blur(16px)_saturate(1.18)] [-webkit-backdrop-filter:blur(16px)_saturate(1.18)] [box-shadow:inset_0_1px_0_var(--glass-highlight)]",
  statusDot: "h-1.5 w-1.5 rounded-full",
};
