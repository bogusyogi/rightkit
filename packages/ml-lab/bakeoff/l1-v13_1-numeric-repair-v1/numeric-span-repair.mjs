const SMALL = new Map(Object.entries({
  zero: 0, one: 1, two: 2, three: 3, four: 4, five: 5, six: 6,
  seven: 7, eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12,
  thirteen: 13, fourteen: 14, fifteen: 15, sixteen: 16,
  seventeen: 17, eighteen: 18, nineteen: 19,
}));
const TENS = new Map(Object.entries({
  twenty: 20, thirty: 30, forty: 40, fifty: 50,
  sixty: 60, seventy: 70, eighty: 80, ninety: 90,
}));
const SCALES = new Map(Object.entries({
  thousand: 1_000,
  million: 1_000_000,
  billion: 1_000_000_000,
  trillion: 1_000_000_000_000,
}));
const NUMBER_WORDS = new Set([
  ...SMALL.keys(),
  ...TENS.keys(),
  "hundred",
  ...SCALES.keys(),
  "and",
]);
const ENUMERATION_WORDS = [
  "first", "second", "third", "fourth", "fifth", "sixth",
  "seventh", "eighth", "ninth", "tenth",
];

function numberWordsToDigits(text) {
  const words = text.toLowerCase().split(/[\s-]+/).filter(word => word !== "and");
  if (!words.length || words.some(word => !NUMBER_WORDS.has(word))) return null;
  let total = 0;
  let group = 0;
  for (const word of words) {
    if (SMALL.has(word)) group += SMALL.get(word);
    else if (TENS.has(word)) group += TENS.get(word);
    else if (word === "hundred") group = Math.max(1, group) * 100;
    else if (SCALES.has(word)) {
      total += Math.max(1, group) * SCALES.get(word);
      group = 0;
    }
  }
  return String(total + group);
}

function hasSpokenEnumerationEvidence(source) {
  const lower = source.toLowerCase();
  const ordinalCount = ENUMERATION_WORDS.filter(word => new RegExp(`\\b${word}\\b`).test(lower)).length;
  const countIntroduction = /\b(?:2|3|4|5|6|7|8|9|10|two|three|four|five|six|seven|eight|nine|ten)\s+(?:things|points|items|steps|rules|reasons|findings)\b/.test(lower);
  return ordinalCount >= 2 || countIntroduction;
}

export function generatedListMarkerRanges(source, output) {
  if (!hasSpokenEnumerationEvidence(source)) return [];
  const candidates = [];
  const pattern = /^(\s*)(\d+)([.)])\s+\S/gm;
  for (const match of output.matchAll(pattern)) {
    const start = match.index + match[1].length;
    candidates.push({
      start,
      end: start + match[2].length,
      value: Number(match[2]),
      punctuation: match[3],
    });
  }
  if (candidates.length < 2) return [];
  const structurallySequential = candidates[0].value === 1 && candidates.every((marker, index) =>
    index === 0 || marker.value === candidates[index - 1].value + 1
  );
  if (!structurallySequential) return [];
  return candidates.map(({ start, end }) => ({ start, end }));
}

function overlapsAny(start, end, ranges) {
  return ranges.some(range => start < range.end && end > range.start);
}

export function extractDigitAtoms(text, ignoredRanges = []) {
  const atoms = [];
  const pattern = /\b(?:[A-Z]\s*)?\d(?:\d|[.,:]\s*(?=\d)|\s+(?=\d))*\b/g;
  for (const match of text.matchAll(pattern)) {
    const start = match.index;
    const end = start + match[0].length;
    if (overlapsAny(start, end, ignoredRanges)) continue;
    atoms.push({
      start,
      end,
      text: match[0],
      digits: match[0].replace(/\D/g, ""),
      kind: "digit",
    });
  }
  return atoms;
}

function nextLexicalWord(text, offset) {
  return /[A-Za-z]+/.exec(text.slice(offset))?.[0].toLowerCase() || "";
}

function requiredSourceAtoms(source) {
  const atoms = extractDigitAtoms(source);
  return atoms.filter((atom, index) => {
    const next = atoms[index + 1];
    if (!next) return true;
    const between = source.slice(atom.end, next.start);
    if (/\b(?:actually(?:\s+make\s+that)?|no[,.]?\s+wait|i\s+mean|scratch\s+that|sorry)\b/i.test(between)) {
      return false;
    }
    const betweenWords = between.match(/[A-Za-z]+/g) || [];
    const repeatedFalseStart = atom.digits === next.digits
      && betweenWords.length <= 4
      && nextLexicalWord(source, atom.end) === nextLexicalWord(source, next.end);
    return !repeatedFalseStart;
  });
}

function extractWordAtoms(text, occupied) {
  const words = [...text.matchAll(/[A-Za-z]+(?:-[A-Za-z]+)*/g)].map(match => ({
    start: match.index,
    end: match.index + match[0].length,
    lower: match[0].toLowerCase(),
  }));
  const atoms = [];
  for (let index = 0; index < words.length;) {
    if (!NUMBER_WORDS.has(words[index].lower) || words[index].lower === "and") {
      index++;
      continue;
    }
    let endIndex = index + 1;
    while (endIndex < words.length) {
      const between = text.slice(words[endIndex - 1].end, words[endIndex].start);
      if (!/^[\s-]+$/.test(between) || !NUMBER_WORDS.has(words[endIndex].lower)) break;
      endIndex++;
    }
    while (endIndex > index + 1 && words[endIndex - 1].lower === "and") endIndex--;
    const start = words[index].start;
    const end = words[endIndex - 1].end;
    const raw = text.slice(start, end);
    const digits = numberWordsToDigits(raw);
    if (!overlapsAny(start, end, occupied) && digits !== null) {
      atoms.push({ start, end, text: raw, digits, kind: "word" });
    }
    index = Math.max(index + 1, endIndex);
  }
  return atoms;
}

function selectOutputAtoms(sourceAtoms, output, outputDigitAtoms) {
  const candidates = [...outputDigitAtoms, ...extractWordAtoms(output, outputDigitAtoms)]
    .sort((left, right) => left.start - right.start || left.end - right.end);
  if (candidates.length < sourceAtoms.length) return null;
  const sourceCount = sourceAtoms.length;
  const outputCount = candidates.length;
  const dp = Array.from({ length: sourceCount + 1 }, () =>
    Array(outputCount + 1).fill(Number.POSITIVE_INFINITY)
  );
  const take = Array.from({ length: sourceCount + 1 }, () =>
    Array(outputCount + 1).fill(false)
  );
  for (let outputIndex = 0; outputIndex <= outputCount; outputIndex++) dp[0][outputIndex] = 0;
  for (let sourceIndex = 1; sourceIndex <= sourceCount; sourceIndex++) {
    for (let outputIndex = 1; outputIndex <= outputCount; outputIndex++) {
      dp[sourceIndex][outputIndex] = dp[sourceIndex][outputIndex - 1];
      const sourcePosition = sourceAtoms[sourceIndex - 1].start
        / Math.max(1, sourceAtoms.at(-1).end);
      const outputPosition = candidates[outputIndex - 1].start / Math.max(1, output.length);
      const mismatch = sourceAtoms[sourceIndex - 1].digits === candidates[outputIndex - 1].digits ? 0 : 4;
      const wordPenalty = candidates[outputIndex - 1].kind === "word" ? 0.1 : 0;
      const cost = dp[sourceIndex - 1][outputIndex - 1]
        + mismatch
        + Math.abs(sourcePosition - outputPosition)
        + wordPenalty;
      if (cost < dp[sourceIndex][outputIndex]) {
        dp[sourceIndex][outputIndex] = cost;
        take[sourceIndex][outputIndex] = true;
      }
    }
  }
  if (!Number.isFinite(dp[sourceCount][outputCount])) return null;
  const selected = [];
  let sourceIndex = sourceCount;
  let outputIndex = outputCount;
  while (sourceIndex > 0 && outputIndex > 0) {
    if (take[sourceIndex][outputIndex]) {
      selected.push(candidates[outputIndex - 1]);
      sourceIndex--;
      outputIndex--;
    } else outputIndex--;
  }
  if (sourceIndex !== 0) return null;
  selected.reverse();
  const selectedSet = new Set(selected);
  if (outputDigitAtoms.some(atom => !selectedSet.has(atom))) return null;
  return selected;
}

function applyReplacements(text, replacements) {
  let result = text;
  for (const replacement of [...replacements].sort((left, right) => right.start - left.start)) {
    result = result.slice(0, replacement.start) + replacement.text + result.slice(replacement.end);
  }
  return result;
}

function sentenceSpans(text) {
  const spans = [];
  let start = 0;
  for (const match of text.matchAll(/[.!?]+(?:["')\]]+)?(?=\s|$)|\n+/g)) {
    const end = match.index + match[0].length;
    if (text.slice(start, end).trim()) spans.push({ start, end, text: text.slice(start, end) });
    start = end;
  }
  if (text.slice(start).trim()) spans.push({ start, end: text.length, text: text.slice(start) });
  return spans;
}

function lexicalTokens(text) {
  return new Set((text.toLowerCase().match(/[a-z]+/g) || []).filter(word => !NUMBER_WORDS.has(word)));
}

function similarity(leftText, rightText) {
  const left = lexicalTokens(leftText);
  const right = lexicalTokens(rightText);
  const union = new Set([...left, ...right]);
  if (!union.size) return 0;
  let intersection = 0;
  for (const word of left) if (right.has(word)) intersection++;
  return intersection / union.size;
}

function sentenceFallback(source, output) {
  const ignoredRanges = generatedListMarkerRanges(source, output);
  const sourceSentences = sentenceSpans(source);
  const outputSentences = sentenceSpans(output);
  const sourceNumeric = sourceSentences.filter(sentence => requiredSourceAtoms(sentence.text).length);
  const outputNumeric = outputSentences.filter(sentence => {
    const localRanges = ignoredRanges
      .filter(range => range.start >= sentence.start && range.end <= sentence.end)
      .map(range => ({ start: range.start - sentence.start, end: range.end - sentence.start }));
    return extractDigitAtoms(sentence.text, localRanges).length;
  });
  const affectedSource = sourceNumeric.length ? sourceNumeric : sourceSentences;
  const affectedOutput = outputNumeric.length ? outputNumeric : outputSentences;
  const replacements = [];
  const used = new Set();
  for (const sourceSentence of affectedSource) {
    let best = null;
    for (const outputSentence of affectedOutput) {
      if (used.has(outputSentence)) continue;
      const score = similarity(sourceSentence.text, outputSentence.text);
      if (!best || score > best.score) best = { outputSentence, score };
    }
    if (!best) continue;
    used.add(best.outputSentence);
    replacements.push({
      start: best.outputSentence.start,
      end: best.outputSentence.end,
      text: `${best.outputSentence.text.match(/^\s*/)[0]}${sourceSentence.text.trim()}`,
    });
  }
  for (const outputSentence of outputNumeric) {
    if (used.has(outputSentence)) continue;
    let best = null;
    for (const sourceSentence of sourceSentences) {
      const score = similarity(sourceSentence.text, outputSentence.text);
      if (!best || score > best.score) best = { sourceSentence, score };
    }
    if (best) replacements.push({
      start: outputSentence.start,
      end: outputSentence.end,
      text: `${outputSentence.text.match(/^\s*/)[0]}${best.sourceSentence.text.trim()}`,
    });
  }
  return applyReplacements(output, replacements);
}

export function numericAtomSignature(source, output = null) {
  if (output === null) return requiredSourceAtoms(source).map(atom => atom.digits);
  const ignoredRanges = generatedListMarkerRanges(source, output);
  return extractDigitAtoms(output, ignoredRanges).map(atom => atom.digits);
}

export function repairNumericMaterial(source, output) {
  const ignoredRanges = generatedListMarkerRanges(source, output);
  const sourceAtoms = requiredSourceAtoms(source);
  const outputDigitAtoms = extractDigitAtoms(output, ignoredRanges);
  if (JSON.stringify(numericAtomSignature(source)) === JSON.stringify(numericAtomSignature(source, output))) {
    return { status: "accepted_numeric_equivalent", delivered: output, repairs: [], ignored_list_markers: ignoredRanges };
  }
  const selected = selectOutputAtoms(sourceAtoms, output, outputDigitAtoms);
  if (selected?.length === sourceAtoms.length) {
    const replacements = [];
    for (let index = 0; index < sourceAtoms.length; index++) {
      const sourceAtom = sourceAtoms[index];
      const outputAtom = selected[index];
      if (outputAtom.kind === "digit" && outputAtom.digits === sourceAtom.digits) continue;
      replacements.push({
        source_atom: sourceAtom.text,
        output_atom: outputAtom.text,
        source_digits: sourceAtom.digits,
        output_digits: outputAtom.digits,
        start: outputAtom.start,
        end: outputAtom.end,
        text: sourceAtom.text,
      });
    }
    const delivered = applyReplacements(output, replacements);
    if (JSON.stringify(numericAtomSignature(source)) === JSON.stringify(numericAtomSignature(source, delivered))) {
      return {
        status: replacements.length ? "repaired_numeric_atoms" : "accepted_numeric_equivalent",
        delivered,
        repairs: replacements.map(({ start, end, text, ...repair }) => repair),
        ignored_list_markers: ignoredRanges,
      };
    }
  }
  const delivered = sentenceFallback(source, output);
  const valid = JSON.stringify(numericAtomSignature(source))
    === JSON.stringify(numericAtomSignature(source, delivered));
  return {
    status: valid ? "repaired_numeric_sentences" : "repair_failed",
    delivered,
    repairs: [],
    ignored_list_markers: ignoredRanges,
  };
}
