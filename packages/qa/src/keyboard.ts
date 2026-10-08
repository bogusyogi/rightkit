import type { CdpParams } from './cdp.js';

const special: Record<string, [string, number, string?]> = {
  Escape: ['Escape', 27], Enter: ['Enter', 13, '\r'], Tab: ['Tab', 9],
  ArrowLeft: ['ArrowLeft', 37], ArrowUp: ['ArrowUp', 38], ArrowRight: ['ArrowRight', 39], ArrowDown: ['ArrowDown', 40],
  Backspace: ['Backspace', 8], Delete: ['Delete', 46], Home: ['Home', 36], End: ['End', 35],
  PageUp: ['PageUp', 33], PageDown: ['PageDown', 34], Space: ['Space', 32, ' '],
  Shift: ['ShiftLeft', 16], Control: ['ControlLeft', 17], Alt: ['AltLeft', 18], Meta: ['MetaLeft', 91],
};
export const modifierBits: Record<string, number> = { Alt: 1, Control: 2, Meta: 4, Shift: 8 };
export function keyDefinition(key: string, modifiers: number): CdpParams {
  const named = special[key];
  let code: string, windowsVirtualKeyCode: number, text: string | undefined;
  if (named) [code, windowsVirtualKeyCode, text] = named;
  else if (/^[a-z]$/i.test(key)) {
    code = `Key${key.toUpperCase()}`; windowsVirtualKeyCode = key.toUpperCase().charCodeAt(0);
    key = modifiers & 8 ? key.toUpperCase() : modifiers & 7 ? key.toLowerCase() : key; text = key;
  } else if (/^[0-9]$/.test(key)) {
    code = `Digit${key}`; windowsVirtualKeyCode = key.charCodeAt(0);
    key = modifiers & 8 ? ')!@#$%^&*('[Number(key)] : key; text = key;
  } else if (key === ' ') { code = 'Space'; windowsVirtualKeyCode = 32; text = ' '; }
  else throw new Error(`Unsupported key: ${key}`);
  if (key === 'Space') key = ' ';
  if (modifiers & (1 | 2 | 4)) text = undefined;
  const unmodifiedText = code.startsWith('Key') ? code.slice(3).toLowerCase() : code.startsWith('Digit') ? code.slice(5) : text;
  return { key, code, windowsVirtualKeyCode, modifiers, ...(text ? { text, unmodifiedText } : {}) };
}

export class QaKeyboard {
  constructor(private send: (method: string, params: CdpParams) => Promise<CdpParams>) {}
  /** Single key or chord, e.g. Control+A, Meta+A, Shift+ArrowLeft. */
  async press(chord: string): Promise<void> {
    const parts = chord.split('+');
    if (parts.some((part, i) => i < parts.length - 1 && !modifierBits[part])) throw new Error(`Invalid key chord: ${chord}`);
    keyDefinition(parts.at(-1)!, 0); // Validate before holding any modifier.
    let modifiers = 0;
    const held: string[] = [];
    let failure: unknown;
    try {
      for (const part of parts) {
        modifiers |= modifierBits[part] ?? 0;
        const definition = keyDefinition(part, modifiers);
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
}
