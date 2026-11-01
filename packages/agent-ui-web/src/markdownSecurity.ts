function hasControlCharacter(value: string): boolean {
  for (const char of value) {
    const code = char.charCodeAt(0);
    if (code <= 31 || code === 127) return true;
  }
  return false;
}

export function safeLinkHref(href?: string): string | undefined {
  if (!href) return undefined;
  const trimmed = href.trim();
  if (!trimmed || hasControlCharacter(trimmed)) return undefined;

  const protocolMatch = trimmed.match(/^([a-z][a-z0-9+.-]*):/i);
  if (!protocolMatch) return undefined;

  const protocol = protocolMatch[1].toLowerCase();
  if (protocol === "http" || protocol === "https") {
    if (!/^https?:\/\//i.test(trimmed)) return undefined;
  } else if (protocol !== "mailto") {
    return undefined;
  }

  try {
    const parsed = new URL(trimmed);
    if (
      parsed.protocol === "http:" ||
      parsed.protocol === "https:" ||
      parsed.protocol === "mailto:"
    ) {
      return trimmed;
    }
  } catch {
    return undefined;
  }
  return undefined;
}
