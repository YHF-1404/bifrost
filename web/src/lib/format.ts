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
