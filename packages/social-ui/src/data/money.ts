// Exact money text. Amounts are integer micro-units (1,000,000 = 1 currency unit). Formatting works on
// the decimal digits of the integer, so no float arithmetic is used. The data layer rejects any amount
// that is not a safe integer before it reaches a screen (see mapping.ts, "Suggestions and spend").

const MICRO_PER_UNIT_DIGITS = 6;

/** Thousands grouping for a plain integer text, for example "18450" to "18,450". A leading "-" is kept. */
export function groupDigits(text: string): string {
  const sign = text.startsWith("-") ? "-" : "";
  const body = sign === "" ? text : text.slice(1);
  return sign + body.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/**
 * Micro-units as currency text with six decimals, for example 2500000 to "2.500000" and -400000 to
 * "-0.400000". Callers add the currency code. Exact for every safe integer.
 */
export function formatMicro(micro: number): string {
  const negative = micro < 0;
  const digits = String(Math.abs(micro)).padStart(MICRO_PER_UNIT_DIGITS + 1, "0");
  const whole = digits.slice(0, -MICRO_PER_UNIT_DIGITS);
  const fraction = digits.slice(-MICRO_PER_UNIT_DIGITS);
  return `${negative ? "-" : ""}${groupDigits(whole)}.${fraction}`;
}

/**
 * Thousandths as exact decimal text, for example 1500 to "1.5", 250 to "0.25" and 4000 to "4". Works on the
 * digits of the integer, so no float is used. Callers pass safe integers only.
 */
export function formatMilli(milli: number): string {
  const negative = milli < 0;
  const digits = String(Math.abs(milli)).padStart(4, "0");
  const whole = String(Number(digits.slice(0, -3)));
  const fraction = digits.slice(-3).replace(/0+$/, "");
  return `${negative ? "-" : ""}${groupDigits(whole)}${fraction === "" ? "" : `.${fraction}`}`;
}

/** The "YYYY-MM" month of a UTC instant, the form the allowance and report commands take. */
export function monthOfInstant(utc: string): string {
  return utc.slice(0, 7);
}
