import type { Target } from "./api";

/**
 * What the middle half of the sample pictures saved, per level (lossless, high, balanced, small),
 * rounded down to multiples of five. Measured with crates/core/examples/forecast.rs.
 */
export const SAMPLE_COUNT = 26;
export const TYPICAL: Record<Target, readonly (readonly [number, number] | null)[]> = {
  keep: [[0, 0], [0, 60], [5, 70], [45, 75]],
  webp: [[0, 20], [0, 60], [15, 80], [55, 95]],
  avif: [null, [0, 60], [25, 95], [60, 95]],
};
