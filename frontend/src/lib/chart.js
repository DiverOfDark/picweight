/**
 * Small scale helpers for the two hand-rolled SVG charts.
 *
 * Hand-rolled because the whole dashboard needs exactly two chart forms, and a
 * charting library would be a bigger dependency than the code it replaces —
 * and would not know about the tick bezel the rest of the interface is built on.
 */

/** Clamp `value` into `[min, max]`. */
export function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value))
}

/** A linear mapper from a data domain to a pixel range. */
export function linear(domain, range) {
  const [d0, d1] = domain
  const [r0, r1] = range
  const span = d1 - d0
  if (span === 0) return () => (r0 + r1) / 2
  return (value) => r0 + ((value - d0) / span) * (r1 - r0)
}

/**
 * Round a raw step up to 1, 2, 2.5 or 5 × a power of ten, so axis labels land on
 * numbers a person would have chosen.
 */
function niceStep(raw) {
  const magnitude = 10 ** Math.floor(Math.log10(raw))
  const normalized = raw / magnitude
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 2.5 ? 2.5 : normalized <= 5 ? 5 : 10
  return step * magnitude
}

/**
 * Axis ticks covering `[min, max]` with roughly `count` divisions, snapped to
 * readable values. Returns `{ ticks, domain }` — the domain is widened to the
 * outermost ticks so the plot never clips a labelled gridline.
 */
export function niceTicks(min, max, count = 5) {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return { ticks: [0, 1], domain: [0, 1] }
  if (min === max) {
    const pad = Math.abs(min) * 0.1 || 1
    min -= pad
    max += pad
  }
  const step = niceStep((max - min) / Math.max(1, count))
  const start = Math.floor(min / step) * step
  const end = Math.ceil(max / step) * step
  const ticks = []
  // Guard against floating-point drift accumulating over the loop.
  for (let i = 0; start + i * step <= end + step / 1000; i += 1) {
    ticks.push(Number((start + i * step).toPrecision(12)))
  }
  return { ticks, domain: [start, end] }
}

/** Convert a polar reading to SVG coordinates, with 0° at twelve o'clock. */
export function polar(cx, cy, radius, degrees) {
  const radians = ((degrees - 90) * Math.PI) / 180
  return { x: cx + radius * Math.cos(radians), y: cy + radius * Math.sin(radians) }
}

/** An SVG arc path sweeping clockwise from `startDeg` to `endDeg`. */
export function arcPath(cx, cy, radius, startDeg, endDeg) {
  const start = polar(cx, cy, radius, startDeg)
  const end = polar(cx, cy, radius, endDeg)
  const largeArc = Math.abs(endDeg - startDeg) > 180 ? 1 : 0
  return `M ${start.x.toFixed(2)} ${start.y.toFixed(2)} A ${radius} ${radius} 0 ${largeArc} 1 ${end.x.toFixed(2)} ${end.y.toFixed(2)}`
}

/** An SVG polyline path through `points`, skipping any gap in the data. */
export function linePath(points) {
  return points
    .map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x.toFixed(2)} ${point.y.toFixed(2)}`)
    .join(' ')
}
