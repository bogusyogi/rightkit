import { describe, expect, it } from 'vitest';
import { QaMouse } from './mouse.js';

/** Records every CDP command QaMouse sends. `fail` may return an error to reject that command. */
function recorder(fail?: (params: any) => Error | undefined) {
  const calls: Array<{ method: string; params: any }> = [];
  const mouse = new QaMouse(async (method, params) => {
    calls.push({ method, params });
    const error = fail?.(params);
    if (error) throw error;
    return {};
  });
  return { calls, mouse };
}
const sent = (calls: Array<{ params: any }>) => calls.map((call) => call.params);

describe('mouse.click, doubleClick & drag (0.4.8)', () => {
  it('click(x, y) sends press and release at the point and never a mouseMoved', async () => {
    const { calls, mouse } = recorder();
    await mouse.click(12, 34);
    expect(calls).toEqual([
      { method: 'Input.dispatchMouseEvent', params: { type: 'mousePressed', x: 12, y: 34, button: 'left', buttons: 1, clickCount: 1 } },
      { method: 'Input.dispatchMouseEvent', params: { type: 'mouseReleased', x: 12, y: 34, button: 'left', buttons: 0, clickCount: 1 } },
    ]);
    expect(mouse.position()).toEqual({ x: 12, y: 34 });
  });

  it('click takes button and clickCount: one press and release per count, each carrying its own clickCount', async () => {
    const { calls, mouse } = recorder();
    await mouse.click(5, 6, { button: 'right', clickCount: 2 });
    expect(sent(calls)).toEqual([
      { type: 'mousePressed', x: 5, y: 6, button: 'right', buttons: 2, clickCount: 1 },
      { type: 'mouseReleased', x: 5, y: 6, button: 'right', buttons: 0, clickCount: 1 },
      { type: 'mousePressed', x: 5, y: 6, button: 'right', buttons: 2, clickCount: 2 },
      { type: 'mouseReleased', x: 5, y: 6, button: 'right', buttons: 0, clickCount: 2 },
    ]);
  });

  it('doubleClick(x, y) is a click, then a press and release with clickCount 2', async () => {
    const { calls, mouse } = recorder();
    await mouse.doubleClick(7, 8);
    expect(sent(calls)).toEqual([
      { type: 'mousePressed', x: 7, y: 8, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseReleased', x: 7, y: 8, button: 'left', buttons: 0, clickCount: 1 },
      { type: 'mousePressed', x: 7, y: 8, button: 'left', buttons: 1, clickCount: 2 },
      { type: 'mouseReleased', x: 7, y: 8, button: 'left', buttons: 0, clickCount: 2 },
    ]);
  });

  it('click and doubleClick refuse while a button is held, and refuse bad input, without sending', async () => {
    const { calls, mouse } = recorder();
    await mouse.down();
    const before = calls.length;
    await expect(mouse.click(1, 1)).rejects.toThrow('Release mouse buttons');
    await expect(mouse.doubleClick(1, 1)).rejects.toThrow('Release mouse buttons');
    expect(calls).toHaveLength(before);
    await mouse.up();
    await expect(mouse.click(1, 1, { clickCount: 0 })).rejects.toThrow('clickCount');
    await expect(mouse.click(Number.NaN, 1)).rejects.toThrow('finite');
    expect(sent(calls).map((params) => params.type)).toEqual(['mousePressed', 'mouseReleased']);
  });

  it('drag presses at the first point, moves with the left button held, and releases at the last point', async () => {
    const { calls, mouse } = recorder();
    await mouse.drag([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 10, y: 20 }], { moveDelayMs: 0 });
    expect(sent(calls)).toEqual([
      { type: 'mousePressed', x: 0, y: 0, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseMoved', x: 10, y: 0, button: 'left', buttons: 1 },
      { type: 'mouseMoved', x: 10, y: 20, button: 'left', buttons: 1 },
      { type: 'mouseReleased', x: 10, y: 20, button: 'left', buttons: 0, clickCount: 1 },
    ]);
    expect(mouse.position()).toEqual({ x: 10, y: 20 });
  });

  it('drag waits moveDelayMs (default 20 ms) before each move', async () => {
    const { mouse } = recorder();
    const start = Date.now();
    await mouse.drag([{ x: 0, y: 0 }, { x: 5, y: 5 }, { x: 9, y: 9 }]);
    expect(Date.now() - start).toBeGreaterThanOrEqual(35);
  });

  it('drag needs at least two points and sends nothing otherwise', async () => {
    const { calls, mouse } = recorder();
    await expect(mouse.drag([{ x: 1, y: 1 }])).rejects.toThrow('at least two points');
    await expect(mouse.drag([])).rejects.toThrow('at least two points');
    expect(calls).toEqual([]);
  });

  it('drag releases the button where the pointer last arrived when a move fails, then rethrows the move error', async () => {
    const { calls, mouse } = recorder((params) => (params.type === 'mouseMoved' ? new Error('move failed') : undefined));
    await expect(mouse.drag([{ x: 0, y: 0 }, { x: 10, y: 0 }], { moveDelayMs: 0 })).rejects.toThrow('move failed');
    expect(sent(calls).map((params) => params.type)).toEqual(['mousePressed', 'mouseMoved', 'mouseReleased']);
    expect(calls.at(-1)!.params).toEqual({ type: 'mouseReleased', x: 0, y: 0, button: 'left', buttons: 0, clickCount: 1 });
    expect(mouse.position()).toEqual({ x: 0, y: 0 });
  });
});
