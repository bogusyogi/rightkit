import type { ImageSaveFunction } from "@rightkit/media-ui";

type MutableCurrent<T> = { current: T };
type DocxSaveFunction = () => Promise<ArrayBuffer | null>;

export function syncActiveFormatSavers(
  activePath: string | null,
  docxSaver: MutableCurrent<DocxSaveFunction | null>,
  imageSaver: MutableCurrent<ImageSaveFunction | null>,
  docxSavers: ReadonlyMap<string, DocxSaveFunction>,
  imageSavers: ReadonlyMap<string, ImageSaveFunction>,
) {
  docxSaver.current = activePath ? (docxSavers.get(activePath) ?? null) : null;
  imageSaver.current = activePath ? (imageSavers.get(activePath) ?? null) : null;
}
