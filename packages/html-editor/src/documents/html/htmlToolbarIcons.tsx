// HTML editor toolbar icons — Phosphor, sized/weighted to match the DOCX toolbar's icon set
// (src/documents/docx/toolbarIcons.tsx) so both document toolbars read as one system. Local to
// the html/ lane (does not import the docx icon module) so the two document domains stay
// independent even though the visual language matches.
import { ImageSquare } from "@phosphor-icons/react/dist/icons/ImageSquare";
import { LinkSimple } from "@phosphor-icons/react/dist/icons/LinkSimple";
import { LinkBreak } from "@phosphor-icons/react/dist/icons/LinkBreak";
import { PencilSimple } from "@phosphor-icons/react/dist/icons/PencilSimple";
import { ArrowsClockwise } from "@phosphor-icons/react/dist/icons/ArrowsClockwise";
import { Highlighter } from "@phosphor-icons/react/dist/icons/Highlighter";
import { X } from "@phosphor-icons/react/dist/icons/X";
import { Check } from "@phosphor-icons/react/dist/icons/Check";
import { CaretUp } from "@phosphor-icons/react/dist/icons/CaretUp";
import { CaretDown } from "@phosphor-icons/react/dist/icons/CaretDown";
import { DotsThreeVertical } from "@phosphor-icons/react/dist/icons/DotsThreeVertical";
import { ArrowsOut } from "@phosphor-icons/react/dist/icons/ArrowsOut";
import { ArrowsIn } from "@phosphor-icons/react/dist/icons/ArrowsIn";
import { Trash } from "@phosphor-icons/react/dist/icons/Trash";

export const IconInsertImage = () => <ImageSquare size={15} weight="regular" />;
export const IconReplaceImage = () => <ArrowsClockwise size={15} weight="regular" />;
export const IconAddLink = () => <LinkSimple size={15} weight="regular" />;
export const IconEditLink = () => <PencilSimple size={15} weight="regular" />;
export const IconRemoveLink = () => <LinkBreak size={15} weight="regular" />;
export const IconHighlight = () => <Highlighter size={15} weight="regular" />;
export const IconCancel = () => <X size={15} weight="regular" />;
export const IconDone = () => <Check size={15} weight="regular" />;

export const IconMoveUp = () => <CaretUp size={15} weight="regular" />;
export const IconMoveDown = () => <CaretDown size={15} weight="regular" />;
export const IconMore = () => <DotsThreeVertical size={15} weight="regular" />;
export const IconWrap = () => <ArrowsOut size={13} weight="regular" />;
export const IconUnwrap = () => <ArrowsIn size={13} weight="regular" />;
export const IconDelete = () => <Trash size={13} weight="regular" />;
