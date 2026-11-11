/** The two options of an on/off `SegmentedControl`. `title` carries the full phrase ("Spell check off")
 *  as the tooltip; the group `label` plus the visible "on" / "off" is the accessible name. */
export function onOff(name: string) {
  return [
    { value: "on" as const, label: "on", title: `${name} on` },
    { value: "off" as const, label: "off", title: `${name} off` },
  ];
}
