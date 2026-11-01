// DOCX toolbar icons — Phosphor, 16px, regular weight so they inherit the button's
// muted/copper color via currentColor.
import { ArrowUUpLeft } from "@phosphor-icons/react/dist/icons/ArrowUUpLeft";
import { ArrowUUpRight } from "@phosphor-icons/react/dist/icons/ArrowUUpRight";
import { TextAlignLeft } from "@phosphor-icons/react/dist/icons/TextAlignLeft";
import { TextAlignCenter } from "@phosphor-icons/react/dist/icons/TextAlignCenter";
import { TextAlignRight } from "@phosphor-icons/react/dist/icons/TextAlignRight";
import { TextAlignJustify } from "@phosphor-icons/react/dist/icons/TextAlignJustify";
import { ListBullets } from "@phosphor-icons/react/dist/icons/ListBullets";
import { ListNumbers } from "@phosphor-icons/react/dist/icons/ListNumbers";
import { TextIndent } from "@phosphor-icons/react/dist/icons/TextIndent";
import { TextOutdent } from "@phosphor-icons/react/dist/icons/TextOutdent";
import { Highlighter } from "@phosphor-icons/react/dist/icons/Highlighter";
import { ImageSquare } from "@phosphor-icons/react/dist/icons/ImageSquare";
import { LinkSimple } from "@phosphor-icons/react/dist/icons/LinkSimple";
import { Rows } from "@phosphor-icons/react/dist/icons/Rows";

export const IconUndo = () => <ArrowUUpLeft size={15} weight="regular" />;
export const IconRedo = () => <ArrowUUpRight size={15} weight="regular" />;

export const IconAlignLeft = () => <TextAlignLeft size={15} weight="regular" />;
export const IconAlignCenter = () => <TextAlignCenter size={15} weight="regular" />;
export const IconAlignRight = () => <TextAlignRight size={15} weight="regular" />;
export const IconAlignJustify = () => <TextAlignJustify size={15} weight="regular" />;

export const IconBullet = () => <ListBullets size={15} weight="regular" />;
export const IconNumbered = () => <ListNumbers size={15} weight="regular" />;
export const IconIndent = () => <TextIndent size={15} weight="regular" />;
export const IconOutdent = () => <TextOutdent size={15} weight="regular" />;

export const IconHighlight = () => <Highlighter size={15} weight="regular" />;
export const IconImage = () => <ImageSquare size={15} weight="regular" />;
export const IconLink = () => <LinkSimple size={15} weight="regular" />;
// Outline / document navigation (matches the PDF "outline" toggle intent). No exact Phosphor
// "outline" icon; "Rows" (stacked horizontal bars) reads closest to the original heading-list glyph.
export const IconOutline = () => <Rows size={15} weight="regular" />;
