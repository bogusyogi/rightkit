import { describe, expect, it } from 'vitest';
import {
  ContrastFailure, WHITE, assertAx, boxShadowVisible, contrastReport, contrastRatio, contrastThreshold,
  effectiveBackground, focusRingEvidence, isLargeText, matchAxNodes, parseCssColor, toHex,
  type AxNode, type ContrastProbe,
} from './a11y.js';

const rgb = (r: number, g: number, b: number) => ({ r, g, b, a: 1 });

describe('colour parsing', () => {
  it('reads the forms a browser reports', () => {
    expect(parseCssColor('rgb(12, 34, 56)')).toEqual(rgb(12, 34, 56));
    expect(parseCssColor('rgba(0, 0, 0, 0)')).toEqual({ r: 0, g: 0, b: 0, a: 0 });
    expect(parseCssColor('rgb(10 20 30 / 50%)')).toEqual({ r: 10, g: 20, b: 30, a: 0.5 });
    expect(parseCssColor('#fff')).toEqual(WHITE);
    expect(parseCssColor('#102030')).toEqual(rgb(16, 32, 48));
    expect(parseCssColor('transparent')?.a).toBe(0);
    const c = parseCssColor('color(srgb 1 0.5 0 / 0.25)');
    expect([c?.r, Math.round(c?.g ?? 0), c?.b, c?.a]).toEqual([255, 128, 0, 0.25]);
    for (const bad of ['', 'red', 'oklch(0.5 0.1 20)', 'rgb(1, 2)', '#12', '#ggg']) expect(parseCssColor(bad)).toBeNull();
  });
});

describe('contrast math', () => {
  it('matches the WCAG reference values', () => {
    expect(contrastRatio(rgb(0, 0, 0), WHITE)).toBeCloseTo(21, 9);
    expect(contrastRatio(WHITE, WHITE)).toBeCloseTo(1, 9);
    expect(contrastRatio(rgb(0x76, 0x76, 0x76), WHITE)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(rgb(0x77, 0x77, 0x77), WHITE)).toBeLessThan(4.5);
  });

  it('composites translucent layers before the ratio', () => {
    const half = { r: 0, g: 0, b: 0, a: 0.5 };
    const ratio = contrastRatio(half, WHITE);
    expect(ratio).toBeGreaterThan(3.8);
    expect(ratio).toBeLessThan(4.2);
    expect(toHex(effectiveBackground([{ r: 0, g: 0, b: 0, a: 0 }, half, { r: 0, g: 0, b: 0, a: 0 }]))).toBe('#808080');
    expect(toHex(effectiveBackground([]))).toBe('#ffffff');
    expect(toHex(effectiveBackground([rgb(1, 2, 3), rgb(9, 9, 9)]))).toBe('#010203');
  });

  it('knows large text and the thresholds', () => {
    expect(isLargeText(24, 400)).toBe(true);
    expect(isLargeText(18.66, 700)).toBe(true);
    expect(isLargeText(18.66, 400)).toBe(false);
    expect(contrastThreshold('AA', false)).toBe(4.5);
    expect(contrastThreshold('AA', true)).toBe(3);
    expect(contrastThreshold('AAA', false)).toBe(7);
    expect(contrastThreshold('AAA', true)).toBe(4.5);
    expect(() => contrastThreshold('A' as never, false)).toThrow(TypeError);
  });

  const probe = (color: string, chain: string[], fontSize = 16): ContrastProbe => ({ found: true, color, chain, fontSize, fontWeight: 400, opacity: 1 });

  it('passes, fails and reports', () => {
    expect(contrastReport('#a', probe('rgb(0, 0, 0)', ['rgba(0, 0, 0, 0)', 'rgb(255, 255, 255)'])).ratio).toBe(21);
    const low = probe('rgb(119, 119, 119)', ['rgb(255, 255, 255)']);
    expect(() => contrastReport('#a', low)).toThrow(ContrastFailure);
    expect(contrastReport('#a', low, { large: true }).passes).toBe(true);
    expect(contrastReport('#a', low, { min: 4 }).passes).toBe(true);
    expect(contrastReport('#a', low, { reportOnly: true }).passes).toBe(false);
    expect(contrastReport('#a', probe('rgb(119, 119, 119)', ['rgb(255, 255, 255)'], 28)).large).toBe(true);
    expect(() => contrastReport('#a', { found: false })).toThrow(/no element/);
    expect(() => contrastReport('#a', probe('oklch(0.5 0.1 20)', []))).toThrow(/text colour/);
    expect(() => contrastReport('#a', probe('rgb(0,0,0)', ['lab(1 2 3)']))).toThrow(/background colour/);
  });
});

describe('focus indicator', () => {
  const base = { focused: true, matches: true, tag: 'button', id: 'go', focusVisible: true, outlineStyle: 'auto', outlineWidth: '1px', outlineColor: 'rgb(16, 16, 16)', outlineOffset: '0px', boxShadow: 'none' };

  it('reads box-shadow only when it paints', () => {
    expect(boxShadowVisible('rgb(0, 95, 204) 0px 0px 0px 2px')).toBe(true);
    expect(boxShadowVisible('rgba(0, 0, 0, 0.3) 0px 1px 4px 0px, rgb(0, 95, 204) 0px 0px 0px 3px')).toBe(true);
    for (const none of ['none', '', 'rgba(0, 0, 0, 0) 0px 0px 0px 2px', 'rgb(0, 95, 204) 0px 0px 0px 0px']) expect(boxShadowVisible(none)).toBe(false);
  });

  it('gives an outline, a shadow or neither', () => {
    const outline = focusRingEvidence(base);
    expect([outline.visible, outline.kind, outline.id]).toEqual([true, 'outline', 'go']);
    const shadow = { ...base, outlineStyle: 'none', outlineWidth: '0px', boxShadow: 'rgb(0, 95, 204) 0px 0px 0px 2px' };
    expect(focusRingEvidence(shadow).kind).toBe('box-shadow');
    expect(focusRingEvidence({ ...shadow, boxShadow: 'none' })).toMatchObject({ visible: false, kind: 'none' });
    expect(focusRingEvidence({ ...base, outlineWidth: '0px' }).visible).toBe(false);
    expect(focusRingEvidence({ ...base, outlineColor: 'rgba(0, 0, 0, 0)' }).visible).toBe(false);
    expect(focusRingEvidence({ focused: false })).toMatchObject({ focused: false, visible: false });
  });
});

describe('role and name assertions', () => {
  const nodes: AxNode[] = [
    { role: 'button', name: 'Save  project', disabled: false },
    { role: 'button', name: '', disabled: true },
    { role: 'textbox', name: 'Title', disabled: false },
    { role: 'heading', name: 'Settings' },
  ];

  it('matches role, collapsed name, substring, named and disabled', () => {
    expect(matchAxNodes(nodes, { role: 'BUTTON' })).toHaveLength(2);
    expect(matchAxNodes(nodes, { role: 'button', name: 'Save project' })).toHaveLength(1);
    expect(matchAxNodes(nodes, { nameContains: 'save' })).toHaveLength(1);
    expect(matchAxNodes(nodes, { role: 'button', named: true })).toHaveLength(1);
    expect(matchAxNodes(nodes, { role: 'button', disabled: true })).toHaveLength(1);
    expect(matchAxNodes(nodes, { name: 'save' })).toHaveLength(0);
  });

  it('counts and explains a miss', () => {
    expect(assertAx(nodes, { role: 'textbox', name: 'Title' })).toHaveLength(1);
    expect(() => assertAx(nodes, { role: 'button', name: 'Delete' })).toThrow(/Delete[\s\S]*textbox:'Title'/);
    expect(() => assertAx(nodes, { role: 'button', maxCount: 1 })).toThrow();
    expect(assertAx(nodes, { role: 'button', minCount: 2 })).toHaveLength(2);
    expect(assertAx(nodes, { role: 'link', minCount: 0, maxCount: 0 })).toHaveLength(0);
    expect(() => assertAx(nodes, { minCount: 1 })).toThrow(TypeError);
  });
});
