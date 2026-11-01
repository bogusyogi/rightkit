/**
 * Concave bottom-corner curves that fuse the ACTIVE titlebar tab into the reader
 * surface — the horizontal-strip counterpart of @rightkit/react-shell FuseCurves
 * (which bridges an active sidebar ROW's right edge). The strip fuses at both
 * bottom corners instead: left piece = the same square-minus-quarter-disc path
 * verbatim; right piece = the same path mirrored with scaleX(-1). Kept as SVG
 * (not the mockup's radial-gradient pseudo-elements) because SVG antialiases the
 * arc edge identically to the sidebar fuse and never depends on the tab's
 * ::before/::after slots (the inactive-tab separator already uses ::before).
 */
const TAB_FUSE_PATH = "M12 0V12H0A12 12 0 0 0 12 0Z";

export function TabFuseCurves({ size = 12 }: { size?: number }) {
  return (
    <>
      <svg
        data-vr-tab-fuse-curve="left"
        aria-hidden="true"
        focusable="false"
        width={size}
        height={size}
        viewBox="0 0 12 12"
        preserveAspectRatio="none"
        className="pointer-events-none absolute bottom-0"
        style={{ left: -size, fill: "var(--reader-surface)" }}
      >
        <path d={TAB_FUSE_PATH} />
      </svg>
      <svg
        data-vr-tab-fuse-curve="right"
        aria-hidden="true"
        focusable="false"
        width={size}
        height={size}
        viewBox="0 0 12 12"
        preserveAspectRatio="none"
        className="pointer-events-none absolute bottom-0"
        style={{ right: -size, transform: "scaleX(-1)", fill: "var(--reader-surface)" }}
      >
        <path d={TAB_FUSE_PATH} />
      </svg>
    </>
  );
}
