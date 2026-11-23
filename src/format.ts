const number = new Intl.NumberFormat("vi-VN", { maximumFractionDigits: 1 });

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${number.format(v)} ${units[i]}`;
}

/** Percentage saved, rounded down so the app never overstates a result. */
export function saved(before: number, after: number): number {
  if (before <= 0 || after >= before) return 0;
  return Math.floor((1 - after / before) * 100);
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}
