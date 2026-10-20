import {
  forwardRef,
  type HTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  type TableHTMLAttributes,
  type TdHTMLAttributes,
  type ThHTMLAttributes,
} from "react";
import { cx } from "./ui.js";

/** Horizontal alignment of a column's content, in reading direction (`end` suits numbers and times). */
export type TableAlign = "start" | "center" | "end";

/** The `aria-sort` values a sortable column header can report. */
export type TableSortDirection = "ascending" | "descending" | "none";

const alignClass = (align: TableAlign | undefined) => (align && align !== "start" ? `rk-table__cell--${align}` : undefined);

export interface TableProps extends Omit<TableHTMLAttributes<HTMLTableElement>, "children"> {
  children: ReactNode;
  /** Accessible name (`aria-label`). Give one unless a `TableCaption` names the table or the page does. */
  label?: string;
  /** Tighter rows for diagnostic or log listings. */
  dense?: boolean;
  /** Content is loading or refreshing: `aria-busy`. Show rows or a `TableMessage tone="pending"` meanwhile; the table does not hide them. */
  pending?: boolean;
}

/**
 * Native `<table>` with the shell's tokens. Structure stays native (`TableHead`, `TableBody`, `TableRow`,
 * `TableHeaderCell`, `TableCell`), so headers keep `scope`, the accessibility tree is a real table and the host
 * keeps its own columns, sorting and data. No scroll container is added: wrap it when it can overflow.
 */
export const Table = forwardRef<HTMLTableElement, TableProps>(function Table({ children, label, dense, pending, className, ...rest }, ref) {
  return (
    <table ref={ref} aria-label={label} aria-busy={pending ? true : undefined} className={cx("rk-table", dense && "rk-table--dense", className)} {...rest}>
      {children}
    </table>
  );
});

/** Visible caption, announced as the table's name. Use `className` to hide it visually when a heading already names the table. */
export function TableCaption({ className, ...rest }: HTMLAttributes<HTMLElement>) {
  return <caption className={cx("rk-table__caption", className)} {...rest} />;
}

export function TableHead({ className, ...rest }: HTMLAttributes<HTMLTableSectionElement>) {
  return <thead className={cx("rk-table__head", className)} {...rest} />;
}

export function TableBody({ className, ...rest }: HTMLAttributes<HTMLTableSectionElement>) {
  return <tbody className={cx("rk-table__body", className)} {...rest} />;
}

export interface TableRowProps extends Omit<HTMLAttributes<HTMLTableRowElement>, "onClick"> {
  /** Highlights the row and reports `aria-selected`. Leave undefined for a row that is not selectable. */
  selected?: boolean;
  /** Makes the row a Tab stop that runs the handler on click, Enter and Space. Keys from a control inside the row stay with that control. */
  onActivate?: () => void;
  /** The row cannot be activated: out of the tab order, `aria-disabled`, dimmed, and `onActivate` does not run. */
  disabled?: boolean;
  /** The row's item is in an error state (marks the row; say what failed in a cell, never by colour alone). */
  error?: boolean;
}

export function TableRow({ selected, onActivate, disabled, error, className, onKeyDown, children, ...rest }: TableRowProps) {
  const interactive = onActivate !== undefined && !disabled;
  const handleClick = interactive
    ? (event: ReactMouseEvent<HTMLTableRowElement>) => {
        // A click on a button, link or field inside the row belongs to that control.
        if ((event.target as HTMLElement).closest("button, a, input, select, textarea, [role='button']") !== null) return;
        onActivate?.();
      }
    : undefined;
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLTableRowElement>) => {
    onKeyDown?.(event);
    if (!interactive || event.defaultPrevented || event.target !== event.currentTarget) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onActivate?.();
    }
  };
  return (
    <tr
      className={cx("rk-table__row", onActivate !== undefined && "rk-table__row--interactive", selected && "is-selected", disabled && "is-disabled", error && "is-error", className)}
      tabIndex={interactive ? 0 : undefined}
      aria-selected={selected}
      aria-disabled={disabled ? true : undefined}
      data-error={error ? "true" : undefined}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      {...rest}
    >
      {children}
    </tr>
  );
}

export interface TableHeaderCellProps extends Omit<ThHTMLAttributes<HTMLTableCellElement>, "align" | "onClick"> {
  align?: TableAlign;
  /** Current sort of this column. Setting it (or `onSort`) puts `aria-sort` on the header. */
  sort?: TableSortDirection;
  /** Makes the header a button that asks the app to sort by this column. The app owns the data and the next direction. */
  onSort?: () => void;
}

/** Column header (`scope="col"` unless given). A sortable header is a real button, so Tab, Enter and Space work and the focus ring shows. */
export function TableHeaderCell({ align, sort, onSort, scope = "col", className, children, ...rest }: TableHeaderCellProps) {
  const sortable = onSort !== undefined;
  const direction = sort ?? (sortable ? "none" : undefined);
  return (
    <th scope={scope} aria-sort={direction} className={cx("rk-table__th", alignClass(align), className)} {...rest}>
      {sortable ? (
        <button type="button" className="rk-table__sort" onClick={onSort}>
          <span>{children}</span>
          <span className="rk-table__sort-mark" aria-hidden="true">
            {direction === "ascending" ? "↑" : direction === "descending" ? "↓" : ""}
          </span>
        </button>
      ) : (
        children
      )}
    </th>
  );
}

export interface TableCellProps extends Omit<TdHTMLAttributes<HTMLTableCellElement>, "align"> {
  align?: TableAlign;
  /** Tabular figures, end-aligned (numbers, sizes, times). */
  numeric?: boolean;
  /** Render as a row header (`<th scope="row">`) for the cell that names the row. */
  rowHeader?: boolean;
}

export function TableCell({ align, numeric, rowHeader, className, children, ...rest }: TableCellProps) {
  const classes = cx("rk-table__cell", numeric && "rk-table__cell--numeric", alignClass(numeric && !align ? "end" : align), className);
  return rowHeader ? (
    <th scope="row" className={cx(classes, "rk-table__rowhead")} {...(rest as ThHTMLAttributes<HTMLTableCellElement>)}>
      {children}
    </th>
  ) : (
    <td className={classes} {...rest}>
      {children}
    </td>
  );
}

export interface TableMessageProps {
  /** Number of columns to span: the table's column count. */
  colSpan: number;
  /** `empty` (nothing to show), `pending` (loading, announced politely) or `error` (announced as an alert). */
  tone?: "empty" | "pending" | "error";
  children: ReactNode;
  className?: string;
}

/** A full-width row for the empty, loading and error states of a body. Keeps the headers in place while it explains. */
export function TableMessage({ colSpan, tone = "empty", children, className }: TableMessageProps) {
  return (
    <tr className={cx("rk-table__row", "rk-table__message", `rk-table__message--${tone}`, className)}>
      <td className="rk-table__cell" colSpan={colSpan}>
        <div role={tone === "error" ? "alert" : "status"}>{children}</div>
      </td>
    </tr>
  );
}
