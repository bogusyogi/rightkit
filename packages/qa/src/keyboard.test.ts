import { describe, expect, it } from 'vitest';
import { QaKeyboard } from './keyboard.js';

/** Records every CDP command QaKeyboard sends, in order. */
function recorder() {
  const calls: Array<{ method: string; params: any }> = [];
  const keyboard = new QaKeyboard(async (method, params) => { calls.push({ method, params }); return {}; });
  return { calls, keyboard };
}

describe('keyboard.tap (0.4.8)', () => {
  it('sends exactly keyDown then keyUp with the given key, code, virtual key and modifiers', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.tap({ key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, modifiers: 0 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, modifiers: 0 } },
    ]);
  });

  it('sends no text, no rawKeyDown and no modifier presses; the modifier mask rides on both events', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.tap({ key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2 });
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2 } },
    ]);
  });

  it('omits an unset code and virtual key, and refuses an empty key or a bad mask before sending', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.tap({ key: 'x' });
    expect(calls[0].params).toStrictEqual({ type: 'keyDown', key: 'x', modifiers: 0 });
    await expect(keyboard.tap({ key: '' })).rejects.toThrow('tap key must be a non-empty string');
    await expect(keyboard.tap({ key: 'x', modifiers: 16 })).rejects.toThrow('bit mask from 0 to 15');
    expect(calls).toHaveLength(2);
  });
});
