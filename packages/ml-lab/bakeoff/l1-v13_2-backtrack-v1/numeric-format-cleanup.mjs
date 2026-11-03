const ONES = new Map(Object.entries({
  zero: 0, one: 1, two: 2, three: 3, four: 4, five: 5, six: 6,
  seven: 7, eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12,
  thirteen: 13, fourteen: 14, fifteen: 15, sixteen: 16,
  seventeen: 17, eighteen: 18, nineteen: 19,
}));
const TENS = new Map(Object.entries({
  twenty: 20, thirty: 30, forty: 40, fifty: 50,
  sixty: 60, seventy: 70, eighty: 80, ninety: 90,
}));
const CARDINAL = [...ONES.keys(), ...TENS.keys()]
  .sort((left, right) => right.length - left.length)
  .join("|");
const SIGNED_CARDINAL = new RegExp(
  `\\b(negative|positive)\\s+(${CARDINAL})(?:[- ](one|two|three|four|five|six|seven|eight|nine))?\\b`,
  "gi",
);

function signedCardinalValue(sign, head, tail) {
  const base = ONES.get(head) ?? TENS.get(head);
  const value = base + (tail ? ONES.get(tail) : 0);
  return `${sign.toLowerCase() === "negative" ? "-" : "+"}${value}`;
}

function overlaps(left, right) {
  return left.start < right.end && left.end > right.start;
}

function collectEdits(text) {
  const edits = [];
  const offer = edit => {
    if (edit.before === edit.after || edits.some(existing => overlaps(existing, edit))) return;
    edits.push(edit);
  };

  for (const match of text.matchAll(SIGNED_CARDINAL)) {
    offer({
      start: match.index,
      end: match.index + match[0].length,
      before: match[0],
      after: signedCardinalValue(match[1], match[2].toLowerCase(), match[3]?.toLowerCase()),
      rule: "signed_cardinal",
    });
  }

  for (const match of text.matchAll(/(?<![\w.])([+-]?\d+)\.\s+(\d+)(?!\w)/g)) {
    offer({
      start: match.index,
      end: match.index + match[0].length,
      before: match[0],
      after: `${match[1]}.${match[2]}`,
      rule: "spaced_decimal",
    });
  }

  for (const match of text.matchAll(/\b([B-HJ-Z])\s+(\d+)\b/g)) {
    offer({
      start: match.index,
      end: match.index + match[0].length,
      before: match[0],
      after: `${match[1]}${match[2]}`,
      rule: "spaced_alphanumeric_token",
    });
  }

  for (const match of text.matchAll(/(?<![\w.:/-])([+-]?)(\d{10,})(?![\w.:/-])/g)) {
    if (/\d\s+$/.test(text.slice(0, match.index))) continue;
    if (match[2].startsWith("0")) continue;
    offer({
      start: match.index,
      end: match.index + match[0].length,
      before: match[0],
      after: `${match[1]}${match[2].replace(/\B(?=(\d{3})+(?!\d))/g, ",")}`,
      rule: "long_integer_grouping",
    });
  }

  return edits.sort((left, right) => left.start - right.start || left.end - right.end);
}

function applyEdits(text, edits) {
  let delivered = text;
  for (const edit of [...edits].sort((left, right) => right.start - left.start)) {
    delivered = `${delivered.slice(0, edit.start)}${edit.after}${delivered.slice(edit.end)}`;
  }
  return delivered;
}

export function cleanupNumericFormatting(output) {
  const edits = collectEdits(output);
  return {
    status: edits.length ? "formatted_numeric_spans" : "unchanged",
    delivered: applyEdits(output, edits),
    edits,
  };
}
