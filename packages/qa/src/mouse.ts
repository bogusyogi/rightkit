import type { CdpParams } from './cdp.js';

export type MouseButton = 'left' | 'right' | 'middle';
export interface MouseMoveOptions { /** Intermediate moves, linear from the current position; default 1 (a single move to the target). */ steps?: number }
export interface MouseButtonOptions { button?: MouseButton; clickCount?: number }
export interface MousePosition { x: number; y: number }
export interface MouseWheelOptions { /** Horizontal scroll delta in CSS pixels; default 0. */ deltaX?: number; /** Vertical scroll delta in CSS pixels; positive scrolls down. Default 0. */ deltaY?: number }
/** Options for drag. `moveDelayMs` is the pause before each move; default 20. */
export interface MouseDragOptions { moveDelayMs?: number }

/** CDP `buttons` bit per button. */
const buttonBits: Record<MouseButton, number> = { left: 1, right: 2, middle: 4 };
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function checkButton(button: string): MouseButton {
  if (!(button in buttonBits)) throw new Error(`Unsupported button: ${button}`);
  return button as MouseButton;
}
function checkCoordinates(x: number, y: number) {
  if (!Number.isFinite(x) || !Number.isFinite(y)) throw new Error('Mouse coordinates must be finite numbers');
}

/**
 * Pointer state for one page: position and held buttons, so a move while a button is held
 * sends the right `buttons` mask. Coordinates are CSS pixels of the viewport, as for `click`.
 */
export class QaMouse {
  private x = 0;
  private y = 0;
  /** Held buttons in press order, with the clickCount each was pressed with. */
  private held = new Map<MouseButton, number>();
  constructor(private send: (method: string, params: CdpParams) => Promise<CdpParams>) {}

  /** Last position this object sent or observed; starts at (0, 0). */
  position(): MousePosition { return { x: this.x, y: this.y }; }

  /** Move to (x, y), optionally in linear intermediate steps. Dispatches mouseMoved per step. */
  async move(x: number, y: number, options: MouseMoveOptions = {}): Promise<void> {
    const steps = options.steps ?? 1;
    if (!Number.isInteger(steps) || steps < 1) throw new Error('Mouse move steps must be a positive integer');
    checkCoordinates(x, y);
    const [fromX, fromY] = [this.x, this.y];
    const [button, buttons] = this.pressedState();
    for (let step = 1; step <= steps; step++) {
      const nextX = step === steps ? x : fromX + ((x - fromX) * step) / steps;
      const nextY = step === steps ? y : fromY + ((y - fromY) * step) / steps;
      await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: nextX, y: nextY, button, buttons });
      this.x = nextX; this.y = nextY;
    }
  }

  /** Press a button at the current position and keep it held until `up`. */
  async down(options: MouseButtonOptions = {}): Promise<void> {
    const button = checkButton(options.button ?? 'left');
    const clickCount = options.clickCount ?? 1;
    if (!Number.isInteger(clickCount) || clickCount < 1) throw new Error('Mouse clickCount must be a positive integer');
    if (this.held.has(button)) throw new Error(`Mouse ${button} button already pressed`);
    await this.pressAt(this.x, this.y, button, clickCount);
  }

  /** Release a held button at the current position. */
  async up(options: MouseButtonOptions = {}): Promise<void> {
    const button = checkButton(options.button ?? 'left');
    const clickCount = this.held.get(button);
    if (clickCount === undefined) throw new Error(`Mouse ${button} button not pressed`);
    await this.releaseAt(this.x, this.y, button, clickCount);
  }

  /**
   * Press and release at (x, y) with no preceding mouseMoved; the pointer is tracked there. With clickCount n,
   * n press/release pairs are sent, the k-th with clickCount k, as page.click does.
   */
  async click(x: number, y: number, options: MouseButtonOptions = {}): Promise<void> {
    const button = checkButton(options.button ?? 'left');
    const clickCount = options.clickCount ?? 1;
    if (!Number.isInteger(clickCount) || clickCount < 1) throw new Error('Mouse clickCount must be a positive integer');
    checkCoordinates(x, y);
    this.assertIdle();
    for (let count = 1; count <= clickCount; count++) {
      await this.pressAt(x, y, button, count);
      await this.releaseAt(x, y, button, count);
    }
  }

  /** A double click at (x, y): a click, then a press and release with clickCount 2. */
  async doubleClick(x: number, y: number, options: { button?: MouseButton } = {}): Promise<void> {
    const button = checkButton(options.button ?? 'left');
    await this.click(x, y, { button });
    await this.pressAt(x, y, button, 2);
    await this.releaseAt(x, y, button, 2);
  }

  /**
   * Drag through `points`: press the left button at the first point, move to each later point after
   * `moveDelayMs` (default 20) with the button held, then release at the last point. At least two points are
   * needed, and nothing is sent otherwise. If a move fails, the button is released where the pointer last
   * arrived, and the move error is rethrown.
   */
  async drag(points: MousePosition[], options: MouseDragOptions = {}): Promise<void> {
    if (!Array.isArray(points) || points.length < 2) throw new Error('mouse.drag needs at least two points');
    const delay = options.moveDelayMs ?? 20;
    if (!Number.isFinite(delay) || delay < 0) throw new Error('moveDelayMs must be a non-negative finite number');
    for (const point of points) checkCoordinates(point.x, point.y);
    this.assertIdle();
    const [start, ...rest] = points;
    await this.pressAt(start.x, start.y, 'left', 1);
    try {
      for (const point of rest) {
        await sleep(delay);
        await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: point.x, y: point.y, button: 'left', buttons: this.mask() });
        this.x = point.x; this.y = point.y;
      }
    } catch (error) {
      await this.releaseAt(this.x, this.y, 'left', 1).catch(() => {});
      throw error;
    }
    const end = points[points.length - 1];
    await this.releaseAt(end.x, end.y, 'left', 1);
  }

  /**
   * Wheel (scroll) at the tracked position. Sends one mouseWheel event with the given deltas;
   * the pointer does not move. At least one delta must be non-zero.
   */
  async wheel(options: MouseWheelOptions = {}): Promise<void> {
    const deltaX = options.deltaX ?? 0, deltaY = options.deltaY ?? 0;
    if (!Number.isFinite(deltaX) || !Number.isFinite(deltaY)) throw new Error('Mouse wheel deltas must be finite numbers');
    if (deltaX === 0 && deltaY === 0) throw new Error('Mouse wheel needs a non-zero deltaX or deltaY');
    await this.send('Input.dispatchMouseEvent', { type: 'mouseWheel', x: this.x, y: this.y, deltaX, deltaY });
  }

  /** @internal Called by page.click & page.hover after they dispatch at a point. */
  track(x: number, y: number) { this.x = x; this.y = y; }

  /** @internal Page-level click & hover send their own events without a held mask, so refuse to run while held. */
  assertReleased() {
    if (this.held.size) throw new Error('Release page.mouse buttons before page click or hover');
  }

  /** Presses `button` at (x, y), tracks the position, and records the button as held. */
  private async pressAt(x: number, y: number, button: MouseButton, clickCount: number): Promise<void> {
    const buttons = this.mask() | buttonBits[button];
    await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button, buttons, clickCount });
    this.x = x; this.y = y;
    this.held.set(button, clickCount);
  }
  /** Releases `button` at (x, y), tracks the position, and clears the held record. */
  private async releaseAt(x: number, y: number, button: MouseButton, clickCount: number): Promise<void> {
    const buttons = this.mask() & ~buttonBits[button];
    await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button, buttons, clickCount });
    this.x = x; this.y = y;
    this.held.delete(button);
  }
  /** Refuses a click, doubleClick or drag while a button is held: its own press would carry the wrong mask. */
  private assertIdle() {
    if (this.held.size) throw new Error('Release mouse buttons before mouse click, doubleClick or drag');
  }
  private mask(): number {
    let buttons = 0;
    for (const button of this.held.keys()) buttons |= buttonBits[button];
    return buttons;
  }
  private pressedState(): [string, number] {
    const last = [...this.held.keys()].at(-1);
    return [last ?? 'none', this.mask()];
  }
}
