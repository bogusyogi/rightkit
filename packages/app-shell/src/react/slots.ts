/**
 * Extra class names for named regions of a visual component, merged after the shell's own `rk-*`
 * classes. Restyle an app's regions without forking markup: `slots={{ header: "my-title" }}`.
 */
export type SlotClasses<K extends string> = Readonly<Partial<Record<K, string>>>;
