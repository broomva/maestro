// Brand marks (BRO-1766). The blackhole is a custom-drawn glyph, but it is NOT a UI icon: it rides
// a fixed dark chip in a fixed brand color and its singularity is a *filled* dot, so it cannot
// follow the stroke-icon canon (currentColor stroke, stroke-width 2, no fill) that check:icons
// enforces on packages/ui/src/icons. It lives here, deliberately outside that audited dir.
//
// Drawn in `currentColor` so the surface sets the color — white on the app's dark `--bv-ink` chip
// (`text-[var(--bv-white)]`), or barely-blue ink on a light marketing surface — one component, both.

export interface BlackholeMarkProps {
  /** Rendered size in px (square). Default 16 (inline size on the 24px sidebar chip). */
  size?: number;
  className?: string;
}

/**
 * The Broomva blackhole — an accretion ring (stroked circle) around a singularity (filled dot).
 * Replaces the inline SVG that lived in the app shell and the opaque raster tile it stood in for
 * (BRO-1771 P20: a baseline JPEG painted a pure-#000 square on the light sidebar).
 */
export function BlackholeMark({ size = 16, className }: BlackholeMarkProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      className={className}
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="7.5" stroke="currentColor" strokeWidth="1.75" />
      <circle cx="12" cy="12" r="3" fill="currentColor" />
    </svg>
  );
}
