// Markup and handler checks for Table (server render and direct handler calls; real focus and keys are not simulated here).
import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableMessage, TableRow } from "./table.js";

describe("Table markup", () => {
  it("renders a native table with a name, scoped headers and cells", () => {
    const html = renderToStaticMarkup(
      <Table label="Agent activity log">
        <TableHead>
          <TableRow>
            <TableHeaderCell>Time</TableHeaderCell>
            <TableHeaderCell align="end">Size</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          <TableRow>
            <TableCell rowHeader>10:00</TableCell>
            <TableCell numeric>12</TableCell>
          </TableRow>
        </TableBody>
      </Table>,
    );
    expect(html).toContain('<table aria-label="Agent activity log" class="rk-table">');
    expect(html).toContain('<thead class="rk-table__head">');
    expect(html).toContain('<th scope="col" class="rk-table__th">Time</th>');
    expect(html).toContain('<th scope="col" class="rk-table__th rk-table__cell--end">Size</th>');
    expect(html).toContain('<th scope="row" class="rk-table__cell rk-table__rowhead">10:00</th>');
    expect(html).toContain('<td class="rk-table__cell rk-table__cell--numeric rk-table__cell--end">12</td>');
    expect(html).not.toContain("aria-busy");
    expect(html).not.toContain("aria-sort");
  });

  it("dense, pending and className", () => {
    const html = renderToStaticMarkup(<Table dense pending className="mine"><TableBody /></Table>);
    expect(html).toContain('aria-busy="true"');
    expect(html).toContain('class="rk-table rk-table--dense mine"');
  });

  it("a tbody-only table needs no head", () => {
    const html = renderToStaticMarkup(<Table><TableBody><TableRow><TableCell>a</TableCell></TableRow></TableBody></Table>);
    expect(html).not.toContain("<thead");
    expect(html).toContain("<td");
  });

  it("caption", () => {
    expect(renderToStaticMarkup(<Table><TableCaption>Services</TableCaption><TableBody /></Table>)).toContain('<caption class="rk-table__caption">Services</caption>');
  });
});

describe("TableHeaderCell sorting", () => {
  it("reports aria-sort and renders a button only when onSort is given", () => {
    const sorted = renderToStaticMarkup(<table><thead><tr><TableHeaderCell sort="ascending" onSort={() => undefined}>Name</TableHeaderCell></tr></thead></table>);
    expect(sorted).toContain('aria-sort="ascending"');
    expect(sorted).toContain('<button type="button" class="rk-table__sort">');
    const unsorted = renderToStaticMarkup(<table><thead><tr><TableHeaderCell onSort={() => undefined}>Name</TableHeaderCell></tr></thead></table>);
    expect(unsorted).toContain('aria-sort="none"');
    const plain = renderToStaticMarkup(<table><thead><tr><TableHeaderCell>Name</TableHeaderCell></tr></thead></table>);
    expect(plain).not.toContain("<button");
    expect(plain).not.toContain("aria-sort");
    const passive = renderToStaticMarkup(<table><thead><tr><TableHeaderCell sort="descending">Name</TableHeaderCell></tr></thead></table>);
    expect(passive).toContain('aria-sort="descending"');
    expect(passive).not.toContain("<button");
  });

  it("the sort button calls onSort", () => {
    const onSort = vi.fn();
    const th = TableHeaderCell({ onSort, children: "Name" });
    const button = (th.props as { children: { props: { onClick: () => void } } }).children;
    button.props.onClick();
    expect(onSort).toHaveBeenCalledTimes(1);
  });
});

describe("TableRow states", () => {
  const key = (name: string, target: object, currentTarget: object = target) => {
    const event = { key: name, target, currentTarget, defaultPrevented: false, preventDefault: vi.fn() };
    return event;
  };

  it("a plain row is not focusable and has no selection state", () => {
    const html = renderToStaticMarkup(<table><tbody><TableRow><TableCell>a</TableCell></TableRow></tbody></table>);
    expect(html).toContain('<tr class="rk-table__row">');
    expect(html).not.toContain("tabindex");
  });

  it("an activatable row is a Tab stop; selected, disabled and error show in markup", () => {
    const go = () => undefined;
    const active = renderToStaticMarkup(<table><tbody><TableRow onActivate={go} selected><TableCell>a</TableCell></TableRow></tbody></table>);
    expect(active).toContain('tabindex="0"');
    expect(active).toContain('aria-selected="true"');
    expect(active).toContain("rk-table__row--interactive is-selected");
    const off = renderToStaticMarkup(<table><tbody><TableRow onActivate={go} disabled><TableCell>a</TableCell></TableRow></tbody></table>);
    expect(off).not.toContain("tabindex");
    expect(off).toContain('aria-disabled="true"');
    expect(off).toContain("is-disabled");
    const bad = renderToStaticMarkup(<table><tbody><TableRow error><TableCell>a</TableCell></TableRow></tbody></table>);
    expect(bad).toContain('data-error="true"');
    expect(bad).toContain("is-error");
  });

  it("Enter and Space activate once; other keys and keys from inner controls do not", () => {
    const onActivate = vi.fn();
    const row = TableRow({ onActivate, children: null });
    const onKeyDown = row.props.onKeyDown as (e: unknown) => void;
    const self = {};
    const enter = key("Enter", self);
    onKeyDown(enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    onKeyDown(key(" ", self));
    expect(onActivate).toHaveBeenCalledTimes(2);
    onKeyDown(key("a", self));
    onKeyDown(key("Enter", {}, self));
    expect(onActivate).toHaveBeenCalledTimes(2);
  });

  it("a disabled row never activates", () => {
    const onActivate = vi.fn();
    const row = TableRow({ onActivate, disabled: true, children: null });
    expect(row.props.onClick).toBeUndefined();
    const self = {};
    (row.props.onKeyDown as (e: unknown) => void)(key("Enter", self));
    expect(onActivate).not.toHaveBeenCalled();
  });

  it("a click activates, except one that lands on a control inside the row", () => {
    const onActivate = vi.fn();
    const row = TableRow({ onActivate, children: null });
    const onClick = row.props.onClick as (e: unknown) => void;
    onClick({ target: { closest: () => null } });
    expect(onActivate).toHaveBeenCalledTimes(1);
    onClick({ target: { closest: () => ({}) } });
    expect(onActivate).toHaveBeenCalledTimes(1);
  });

  it("a caller's onKeyDown runs first and can veto activation", () => {
    const onActivate = vi.fn();
    const row = TableRow({ onActivate, onKeyDown: (e) => e.preventDefault(), children: null });
    const self = {};
    const event = { key: "Enter", target: self, currentTarget: self, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
    (row.props.onKeyDown as (e: unknown) => void)(event);
    expect(onActivate).not.toHaveBeenCalled();
  });
});

describe("TableMessage", () => {
  it("spans the columns; error is an alert, the others a status", () => {
    const empty = renderToStaticMarkup(<table><tbody><TableMessage colSpan={4}>Nothing yet</TableMessage></tbody></table>);
    expect(empty).toMatch(/colspan="4"/i);
    expect(empty).toContain('role="status"');
    expect(empty).toContain("rk-table__message--empty");
    const bad = renderToStaticMarkup(<table><tbody><TableMessage colSpan={2} tone="error">Failed</TableMessage></tbody></table>);
    expect(bad).toContain('role="alert"');
    expect(bad).toContain("rk-table__message--error");
  });
});
