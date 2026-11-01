// Pill glyphs — Phosphor, same family as the rest of the app. The pill CSS
// (.hr-ico svg / .hr-btn svg in pillStyles.ts) sizes them and fills with
// currentColor; play uses the fill weight for a solid triangle.
import {
  Check,
  X,
  Square,
  ArrowRight,
  Copy,
  House,
  List,
  Microphone,
  Play,
  Warning,
  type Icon as PhosphorIcon,
} from "@phosphor-icons/react";

const ICONS: Record<string, PhosphorIcon> = {
  check: Check,
  x: X,
  stop: Square,
  cancel: X,
  send: ArrowRight,
  copy: Copy,
  hub: House,
  list: List,
  mic: Microphone,
  play: Play,
  warning: Warning,
};

export function Icon({
  name,
}: {
  name: "check" | "x" | "stop" | "cancel" | "send" | "copy" | "hub" | "list" | "mic" | "play" | "warning";
}) {
  const Cmp = ICONS[name];
  // stop matches Swift's stop.fill (solid amber square); outline read as an
  // "empty yellow frame" against the dark pill.
  return <Cmp weight={name === "play" || name === "stop" ? "fill" : "bold"} aria-hidden="true" />;
}
