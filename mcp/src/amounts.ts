// Amounts are decimal strings in USD with six places, one to one with micro-USD. Nothing here
// adds, subtracts or compares amounts: an integer is split into text, a number is printed.

export class UpstreamError extends Error {}

/** 1234567 -> "1.234567". */
export function microToUsd(micro: unknown): string {
  if (typeof micro !== "number" || !Number.isSafeInteger(micro) || micro < 0) {
    throw new UpstreamError("an amount was not a whole number of micro-USD");
  }
  const s = String(micro).padStart(7, "0");
  return `${s.slice(0, -6)}.${s.slice(-6)}`;
}

/** A policy limit (a JSON number with at most six places) as the same decimal string. */
export function limitToUsd(v: unknown): string {
  if (typeof v !== "number" || !Number.isFinite(v) || v < 0) {
    throw new UpstreamError("a limit was not a USD amount");
  }
  const text = v.toFixed(6);
  if (Number(text) !== v) throw new UpstreamError("a limit had more than six places");
  return text;
}

/** The caller's `max_usd`: a positive decimal with at most six places. */
export const MAX_USD_PATTERN = "^(?=.*[1-9])(0|[1-9][0-9]{0,8})(\\.[0-9]{1,6})?$";
const MAX_USD = new RegExp(MAX_USD_PATTERN);
export function isPositiveUsd(v: unknown): v is string {
  return typeof v === "string" && MAX_USD.test(v);
}
