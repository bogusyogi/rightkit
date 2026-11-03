import type { CdpParams } from './cdp.js';

const special: Record<string, [string, number, string?]> = {
  Escape: ['Escape', 27], Enter: ['Enter', 13, '\r'], Tab: ['Tab', 9],
  ArrowLeft: ['ArrowLeft', 37], ArrowUp: ['ArrowUp', 38], ArrowRight: ['ArrowRight', 39], ArrowDown: ['ArrowDown', 40],
  Backspace: ['Backspace', 8], Delete: ['Delete', 46], Home: ['Home', 36], End: ['End', 35],
  PageUp: ['PageUp', 33], PageDown: ['PageDown', 34], Space: ['Space', 32, ' '],
  Shift: ['ShiftLeft', 16], Control: ['ControlLeft', 17], Alt: ['AltLeft', 18], Meta: ['MetaLeft', 91],
};
export const modifierBits: Record<string, number> = { Alt: 1, Control: 2, Meta: 4, Shift: 8 };

/** Function keys F1-F24: code "F<n>", Windows virtual key 111 + n (VK_F1 = 112 ... VK_F24 = 135). */
const functionKey = /^F(?:[1-9]|1\d|2[0-4])$/;

/** One printable key of the US layout, reachable unshifted (`base`) and with Shift (`shifted`). */
interface PrintableKey { base: string; shifted: string; code: string; keyCode: number; requiresShift: boolean }
/** [base, shifted, code, Windows virtual key] for each US-layout punctuation key. */
const punctuation: Array<[string, string, string, number]> = [
  ['`', '~', 'Backquote', 192], ['-', '_', 'Minus', 189], ['=', '+', 'Equal', 187],
  ['[', '{', 'BracketLeft', 219], [']', '}', 'BracketRight', 221], ['\\', '|', 'Backslash', 220],
  [';', ':', 'Semicolon', 186], ["'", '"', 'Quote', 222], [',', '<', 'Comma', 188],
  ['.', '>', 'Period', 190], ['/', '?', 'Slash', 191],
];
const shiftedDigits = ')!@#$%^&*(';
/** Keyed by the character the key prints, so "?" and "/" both resolve, the first with Shift forced. */
const printable = new Map<string, PrintableKey>();
for (const [base, shifted, code, keyCode] of punctuation) {
  printable.set(base, { base, shifted, code, keyCode, requiresShift: false });
  printable.set(shifted, { base, shifted, code, keyCode, requiresShift: true });
}
for (let digit = 0; digit <= 9; digit++) {
  const base = String(digit), code = `Digit${digit}`, keyCode = 48 + digit;
  printable.set(base, { base, shifted: shiftedDigits[digit], code, keyCode, requiresShift: false });
  printable.set(shiftedDigits[digit], { base, shifted: shiftedDigits[digit], code, keyCode, requiresShift: true });
}

export function keyDefinition(key: string, modifiers: number): CdpParams {
  const named = special[key];
  let code: string, windowsVirtualKeyCode: number, text: string | undefined, unmodifiedText: string | undefined;
  let effective = modifiers;
  if (named) { [code, windowsVirtualKeyCode, text] = named; unmodifiedText = text; }
  else if (functionKey.test(key)) { code = key; windowsVirtualKeyCode = 111 + Number(key.slice(1)); }
  else if (/^[a-z]$/i.test(key)) {
    code = `Key${key.toUpperCase()}`; windowsVirtualKeyCode = key.toUpperCase().charCodeAt(0);
    key = modifiers & 8 ? key.toUpperCase() : modifiers & 7 ? key.toLowerCase() : key; text = key;
    unmodifiedText = code.slice(3).toLowerCase();
  } else if (printable.has(key)) {
    const symbol = printable.get(key)!;
    code = symbol.code; windowsVirtualKeyCode = symbol.keyCode;
    if (symbol.requiresShift) effective |= 8;
    key = effective & 8 ? symbol.shifted : symbol.base; text = key; unmodifiedText = symbol.base;
  } else if (key === ' ') { code = 'Space'; windowsVirtualKeyCode = 32; text = ' '; unmodifiedText = ' '; }
  else throw new Error(`Unsupported key: ${key}`);
  if (key === 'Space') key = ' ';
  if (effective & (1 | 2 | 4)) text = undefined;
  return { key, code, windowsVirtualKeyCode, modifiers: effective, ...(text ? { text, unmodifiedText } : {}) };
}

/** Splits "Control+A" into parts. A literal plus key is the last part: "+" and "Control++". */
function splitChord(chord: string): string[] {
  if (chord === '+') return ['+'];
  if (chord.endsWith('++')) return [...chord.slice(0, -2).split('+'), '+'];
  return chord.split('+');
}

/** A raw key for `tap`. `modifiers` is the mask modifierBits uses: Alt 1, Control 2, Meta 4, Shift 8. */
export interface KeyTapOptions extends KeyTextOptions { key: string; code?: string; windowsVirtualKeyCode?: number; modifiers?: number }
/**
 * Text a key press produces. It is sent on the keyDown only (never on the keyUp), so Enter can be `"\r"` and Space `" "`.
 * Without it, the text comes from the key as press and tap already derive it.
 */
export interface KeyTextOptions { text?: string; unmodifiedText?: string }
/** Options for `keyboard.imeSetComposition`: the selection inside the composition text, and the optional text range it replaces. */
export interface ImeCompositionOptions { selectionStart: number; selectionEnd: number; replacementStart?: number; replacementEnd?: number }

function checkKeyText(options: KeyTextOptions, what: string): void {
  for (const field of ['text', 'unmodifiedText'] as const) {
    const value = options[field];
    if (value !== undefined && typeof value !== 'string') throw new Error(`${what} ${field} must be a string`);
  }
}

export class QaKeyboard {
  constructor(private send: (method: string, params: CdpParams) => Promise<CdpParams>) {}
  /**
   * Single key or chord, e.g. Control+A, Meta+A, Shift+ArrowLeft, F3, ?, Control+/.
   * Function keys F1-F24 and US-layout printable keys (shifted ones included) are supported.
   */
  async press(chord: string, options: KeyTextOptions = {}): Promise<void> {
    checkKeyText(options, 'press');
    const parts = splitChord(chord);
    if (parts.some((part, i) => i < parts.length - 1 && !modifierBits[part])) throw new Error(`Invalid key chord: ${chord}`);
    keyDefinition(parts.at(-1)!, 0); // Validate before holding any modifier.
    let modifiers = 0;
    const held: string[] = [];
    let failure: unknown;
    try {
      for (const [index, part] of parts.entries()) {
        modifiers |= modifierBits[part] ?? 0;
        const definition = keyDefinition(part, modifiers);
        // The explicit text goes on the final key's keyDown only; the keyUp below never carries text.
        if (index === parts.length - 1) applyKeyText(definition, options);
        await this.send('Input.dispatchKeyEvent', { type: definition.text ? 'keyDown' : 'rawKeyDown', ...definition });
        held.push(part);
      }
    } catch (error) { failure = error; } finally {
      for (const part of held.reverse()) {
        modifiers &= ~(modifierBits[part] ?? 0);
        try {
          await this.send('Input.dispatchKeyEvent', { type: 'keyUp', ...keyDefinition(part, modifiers), text: undefined, unmodifiedText: undefined });
        } catch (error) { failure ??= error; }
      }
    }
    if (failure) throw failure;
  }
  /**
   * One key press: keyDown, then keyUp, both with the same key, code, Windows virtual key and modifier mask.
   * Unlike press, it sends no rawKeyDown and no key-down or key-up for a modifier. Text is sent only when `text`
   * or `unmodifiedText` is given, and then on the keyDown only.
   */
  async tap(options: KeyTapOptions): Promise<void> {
    const { key, code, windowsVirtualKeyCode, modifiers = 0, text, unmodifiedText } = options;
    if (typeof key !== 'string' || !key) throw new Error('tap key must be a non-empty string');
    if (!Number.isInteger(modifiers) || modifiers < 0 || modifiers > 15) throw new Error('tap modifiers must be a bit mask from 0 to 15');
    if (windowsVirtualKeyCode !== undefined && !Number.isInteger(windowsVirtualKeyCode)) throw new Error('tap windowsVirtualKeyCode must be an integer');
    checkKeyText({ text, unmodifiedText }, 'tap');
    const event: CdpParams = { key, modifiers };
    if (code !== undefined) event.code = code;
    if (windowsVirtualKeyCode !== undefined) event.windowsVirtualKeyCode = windowsVirtualKeyCode;
    const down: CdpParams = { ...event };
    if (text !== undefined) down.text = text;
    if (unmodifiedText !== undefined) down.unmodifiedText = unmodifiedText;
    await this.send('Input.dispatchKeyEvent', { type: 'keyDown', ...down });
    await this.send('Input.dispatchKeyEvent', { type: 'keyUp', ...event });
  }
  /**
   * A `char` event carrying `text`, as a keyboard produces for one character: `Input.dispatchKeyEvent` with
   * type `char`. Used by inputs that read characters rather than key codes.
   */
  async char(text: string): Promise<void> {
    if (typeof text !== 'string' || !text) throw new Error('keyboard.char text must be a non-empty string');
    await this.send('Input.dispatchKeyEvent', { type: 'char', key: text, text, unmodifiedText: text });
  }
  /** Inserts `text` at the focused editable element without a key press and without a selector (`Input.insertText`). */
  async insertText(text: string): Promise<void> {
    if (typeof text !== 'string') throw new Error('keyboard.insertText text must be a string');
    await this.send('Input.insertText', { text });
  }
  /**
   * Sets the IME composition text (`Input.imeSetComposition`). `selectionStart` and `selectionEnd` select inside
   * `text`; `replacementStart` and `replacementEnd`, when given, name the existing text the composition replaces.
   */
  async imeSetComposition(text: string, options: ImeCompositionOptions): Promise<void> {
    if (typeof text !== 'string') throw new Error('imeSetComposition text must be a string');
    const { selectionStart, selectionEnd, replacementStart, replacementEnd } = options ?? ({} as ImeCompositionOptions);
    for (const [name, value] of [['selectionStart', selectionStart], ['selectionEnd', selectionEnd]] as const) {
      if (!Number.isInteger(value) || value < 0 || value > text.length) throw new Error(`imeSetComposition ${name} must be an integer from 0 to the text length`);
    }
    if (selectionStart > selectionEnd) throw new Error('imeSetComposition selectionStart must not exceed selectionEnd');
    const params: CdpParams = { text, selectionStart, selectionEnd };
    if (replacementStart !== undefined) {
      if (!Number.isInteger(replacementStart) || replacementStart < 0) throw new Error('imeSetComposition replacementStart must be a non-negative integer');
      params.replacementStart = replacementStart;
    }
    if (replacementEnd !== undefined) {
      if (!Number.isInteger(replacementEnd) || replacementEnd < 0) throw new Error('imeSetComposition replacementEnd must be a non-negative integer');
      params.replacementEnd = replacementEnd;
    }
    await this.send('Input.imeSetComposition', params);
  }
  /** Ends the IME composition with `text` as the committed result (`Input.insertText`). */
  async imeCommit(text: string): Promise<void> { await this.insertText(text); }
}

/** Applies explicit text to a key definition, keeping keyUp text-free. A definition without text stays a rawKeyDown. */
function applyKeyText(definition: CdpParams, options: KeyTextOptions): void {
  if (options.text !== undefined) definition.text = options.text;
  if (options.unmodifiedText !== undefined) definition.unmodifiedText = options.unmodifiedText;
}
