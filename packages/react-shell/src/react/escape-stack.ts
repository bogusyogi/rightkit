// Open modals that take Escape, bottom to top. Only the top entry may handle Escape, so stacked dialogs close one at a
// time. Internal to the package: not re-exported from react.tsx.
const stack: object[] = [];

/** Register an owner on top of the stack; the returned function removes it. */
export function pushEscapeOwner(owner: object): () => void {
  stack.push(owner);
  return () => {
    const at = stack.indexOf(owner);
    if (at >= 0) stack.splice(at, 1);
  };
}

/** True when `owner` is the topmost open modal. */
export function isTopEscapeOwner(owner: object): boolean {
  return stack[stack.length - 1] === owner;
}
