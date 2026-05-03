// Shared display / validation helpers used by NetworkDetail's two
// view modes.

import { ApiError } from "./api";

/** First 8 hex chars of a UUID, without dashes. */
export function shortUuid(s: string): string {
  return s.replace(/-/g, "").slice(0, 8);
}

/** Loose CIDR shape check; the server validates strictly. */
export function isCidr(s: string): boolean {
  return /^[0-9a-fA-F:.]+\/\d{1,3}$/.test(s);
}

/** Pull a human-readable message out of any error type the api client
 *  can throw. */
export function fmtErr(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

const BPS_UNITS = ["B/s", "KB/s", "MB/s", "GB/s"];

/** Human-readable bytes-per-second, e.g. `1.2 MB/s`. The threshold is
 *  1000 (decimal SI) so the numbers match the raw byte counters; we
 *  switch to integer formatting once we exceed three digits to keep
 *  the column width stable. */
export function fmtBps(n: number): string {
  if (n < 1) return "0";
  let i = 0;
  let v = n;
  while (v >= 1000 && i < BPS_UNITS.length - 1) {
    v /= 1000;
    i += 1;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${BPS_UNITS[i]}`;
}
