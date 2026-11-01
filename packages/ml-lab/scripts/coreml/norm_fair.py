"""Formatting-fair text normaliser for ASR comparison.

`pico_eval.norm` lowercases and strips punctuation but leaves numbers, symbols
and acronym spacing alone. That is not neutral between engines: an engine that
FORMATS its output (Apple SpeechAnalyzer writes "22", "50%", "ET") is charged
substitutions and deletions against verbatim-style ground truth ("twenty two",
"fifty percent", "E T"), while an engine that emits verbatim tokens (Parakeet)
matches for free. Those are presentation differences, not recognition errors —
and in the "22 degrees C" case the formatted output is arguably the better one.

This normaliser puts both engines in one canonical form before scoring:

  * numbers   -> words ("22" -> "twenty two", "1990" -> "nineteen ninety")
  * symbols   -> words (%, $, &, °, +, =)
  * acronyms  -> unspaced runs of single letters ("e t" -> "et")
  * ordinals  -> words ("3rd" -> "third")
  * hyphens   -> spaces, then punctuation dropped

Digits are converted TO words rather than the reverse because emitting words
from digits is unambiguous, whereas parsing "twenty two hundred" back into a
number needs a real grammar and introduces its own errors.

This measures RECOGNITION. It deliberately erases formatting quality — which is
a genuine product difference (Apple's punctuation and digits are nicer to read),
just not one that belongs in a WER number. Report it separately.
"""
from __future__ import annotations

import re

from num2words import num2words

_SYMBOLS = {
    "%": " percent ",
    "$": " dollars ",
    "&": " and ",
    "°": " degrees ",
    "+": " plus ",
    "=": " equals ",
    "@": " at ",
    "#": " number ",
}

_ORDINAL = re.compile(r"\b(\d+)(st|nd|rd|th)\b")
_NUMBER = re.compile(r"\b\d[\d,]*(?:\.\d+)?\b")
_TAG = re.compile(r"<[^>]+>")
_SINGLE_LETTER_RUN = re.compile(r"\b(?:[a-z] ){1,}[a-z]\b")


def _year_or_cardinal(n: int) -> str:
    """1900-2099 read as a year ("nineteen ninety"), everything else cardinal."""
    if 1900 <= n <= 2099:
        return num2words(n, to="year")
    return num2words(n)


def _sub_number(m: re.Match) -> str:
    raw = m.group(0).replace(",", "")
    try:
        if "." in raw:
            whole, frac = raw.split(".", 1)
            head = _year_or_cardinal(int(whole)) if whole else "zero"
            digits = " ".join(num2words(int(d)) for d in frac)
            return f" {head} point {digits} "
        return f" {_year_or_cardinal(int(raw))} "
    except Exception:
        return f" {raw} "


def _sub_ordinal(m: re.Match) -> str:
    try:
        return f" {num2words(int(m.group(1)), to='ordinal')} "
    except Exception:
        return f" {m.group(0)} "


def norm_fair(t: str) -> str:
    t = t.lower()
    t = _TAG.sub(" ", t)                      # gigaspeech <COMMA>/<QUESTIONMARK>
    for sym, word in _SYMBOLS.items():
        t = t.replace(sym, word)
    t = _ORDINAL.sub(_sub_ordinal, t)
    t = _NUMBER.sub(_sub_number, t)
    t = t.replace("-", " ")                   # num2words emits "twenty-two"
    t = re.sub(r"[^a-z0-9 ]", " ", t)
    t = re.sub(r"\s+", " ", t).strip()
    # Collapse "e t" -> "et" only for runs of SINGLE letters, so spelled-out
    # acronyms match their unspaced form without touching real words.
    t = _SINGLE_LETTER_RUN.sub(lambda m: m.group(0).replace(" ", ""), t)
    return re.sub(r"\s+", " ", t).strip()


def classify(ref: str, hyp_a: str, hyp_b: str) -> str:
    """Why two hypotheses differ from each other, under both normalisers.

    Returns one of:
      'formatting-only'  identical once formatting is canonicalised
      'real'             still differs after canonicalisation
      'same'             identical under both
    """
    from pico_eval import norm

    if norm(hyp_a) == norm(hyp_b):
        return "same"
    if norm_fair(hyp_a) == norm_fair(hyp_b):
        return "formatting-only"
    return "real"
