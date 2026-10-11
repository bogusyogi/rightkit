import type { CdpParams } from './cdp.js';

export type MouseButton = 'left' | 'right' | 'middle';
export interface MouseMoveOptions { /** Intermediate moves, linear from the current position; default 1 (a single move to the target). */ steps?: number }
export interface MouseButtonOptions { button?: MouseButton; clickCount?: number }
export interface MousePosition { x: number; y: number }
export interface MouseWheelOptions { /** Horizontal scroll delta in CSS pixels; default 0. */ deltaX?: number; /** Vertical scroll delta in CSS pixels; positive scrolls down. Default 0. */ deltaY?: number }

/** CDP `buttons` bit per button. */
const buttonBits: Record<MouseButton, number> = { left: 1, right: 2, middle: 4 };

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
    const buttons = this.mask() | buttonBits[button];
    await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: this.x, y: this.y, button, buttons, clickCount });
    this.held.set(button, clickCount);
  }

  /** Release a held button at the current position. */
  async up(options: MouseButtonOptions = {}): Promise<void> {
    const button = checkButton(options.button ?? 'left');
    const clickCount = this.held.get(button);
    if (clickCount === undefined) throw new Error(`Mouse ${button} button not pressed`);
    const buttons = this.mask() & ~buttonBits[button];
    await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: this.x, y: this.y, button, buttons, clickCount });
    this.held.delete(button);
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
