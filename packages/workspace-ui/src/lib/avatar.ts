// Coloured-initial avatars for people (contacts, senders). One copy: the mail views import them from here.

/** The first letter of a name or address, upper-cased; "?" for an empty one. */
export const initialOf = (s: string) => (s.trim()[0] || "?").toUpperCase();

const AVATAR_COLORS = [
  "#C46A3D", "#6E8B6B", "#B07A84", "#7E8AA8", "#B59A52", "#8A6BA8", "#5E8C8C", "#A8675B",
];

/** Stable per-person avatar colour (Gmail-style coloured initials). */
export function colorFor(s: string): string {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return AVATAR_COLORS[h % AVATAR_COLORS.length];
}
