import { bytes } from "../api";

/** Wording for a size the scan could not prove would be freed. Same text as Cleanup's estimate note. */
export const ESTIMATE = "An estimate from allocated size on disk, not file size. Hard links and clones can make the real gain smaller.";

/** Any removal figure is allocated size, so it is an upper bound; a partial scan reads as a floor. */
export const upTo = (n: number, partial = false) => `${partial ? "≥ " : "up to "}${bytes(n)}`;

/** Items in the Trash keep their space until the Trash is emptied. */
export const trashFreesNothing = (trash: string) =>
  `Items moved to the ${trash} free no space until the ${trash} is emptied.`;
