import { describe, expect, it } from "vitest";
import { mediaClock, mediaErrorMessage } from "./MediaPlayer.js";
import { clampCrop, defaultCrop } from "./image/cropMath.js";
import {
  exportDimensionsFor,
  hasImageEdits,
  initialImageEdit,
  scaleEditToSource,
  outputScaleForResize,
  sameImageEdit,
} from "./image/imageEdit.js";
import {
  defaultImageSaveName,
  editableImageOutputForPath,
  ensureImageOutputPath,
} from "./image/imageSave.js";

// Pure-logic contract checks. Interactive behaviour (real decode, remux
// fallback, crop/rotate/text export pixels) is covered by test/browser-e2e.mjs
// in headless Chrome.

describe("mediaClock", () => {
  it("formats h:mm:ss / m:ss and clamps bad input", () => {
    expect(mediaClock(0)).toBe("0:00");
    expect(mediaClock(65)).toBe("1:05");
    expect(mediaClock(3661)).toBe("1:01:01");
    expect(mediaClock(Number.NaN)).toBe("0:00");
    expect(mediaClock(-5)).toBe("0:00");
  });
});

describe("mediaErrorMessage", () => {
  it("maps element error codes to user text", () => {
    const el = { error: { code: 4 } } as unknown as HTMLMediaElement;
    expect(mediaErrorMessage(el)).toMatch(/unsupported codec/i);
    expect(mediaErrorMessage({ error: null } as unknown as HTMLMediaElement)).toMatch(
      /could not load/i,
    );
  });
});

describe("crop math", () => {
  const image = { width: 800, height: 600 };
  it("clamps crops inside the image", () => {
    expect(clampCrop({ x: -10, y: -10, width: 10000, height: 900 }, image)).toEqual({
      x: 0,
      y: 0,
      width: 800,
      height: 600,
    });
    expect(clampCrop({ x: 790, y: 590, width: 200, height: 200 }, image)).toEqual({
      x: 600,
      y: 400,
      width: 200,
      height: 200,
    });
  });
  it("defaultCrop honors aspect within bounds", () => {
    const crop = defaultCrop(image, 1);
    expect(crop.width).toBe(crop.height);
    expect(crop.width).toBeLessThanOrEqual(image.width);
  });
});

describe("export sizing", () => {
  it("computes output dimensions for percent/width/height modes", () => {
    const src = { width: 800, height: 600 };
    expect(exportDimensionsFor(src, { mode: "original" })).toEqual(src);
    expect(exportDimensionsFor(src, { mode: "percent", value: 50 })).toEqual({
      width: 400,
      height: 300,
    });
    expect(exportDimensionsFor(src, { mode: "width", value: 400 })).toEqual({
      width: 400,
      height: 300,
    });
    expect(exportDimensionsFor(src, { mode: "height", value: 300 })).toEqual({
      width: 400,
      height: 300,
    });
    expect(outputScaleForResize(src, { mode: "percent", value: 900 })).toBe(8);
  });
});

describe("edit recipe", () => {
  it("tracks dirty state and structural equality", () => {
    const edit = initialImageEdit();
    expect(hasImageEdits(edit)).toBe(false);
    const rotated = { ...edit, rotation: 90 };
    expect(hasImageEdits(rotated)).toBe(true);
    expect(sameImageEdit(edit, rotated)).toBe(false);
    expect(sameImageEdit(edit, { ...edit })).toBe(true);
    const rotated360 = { ...edit, rotation: 360 };
    expect(hasImageEdits(rotated360)).toBe(false);
  });
});

describe("save naming", () => {
  it("derives editable output names and paths", () => {
    expect(editableImageOutputForPath("a/b/photo.JPEG")).toBe("jpeg");
    expect(editableImageOutputForPath("a/b/scan.tiff")).toBeNull();
    expect(defaultImageSaveName("/x/photo.png")).toBe("photo-edited.png");
    expect(defaultImageSaveName("/x/photo.jpg")).toBe("photo-edited.jpg");
    expect(ensureImageOutputPath("/x/scan.tiff", "png")).toBe("/x/scan.tiff.png");
    expect(ensureImageOutputPath("/x/photo.png", "webp")).toBe("/x/photo.png");
  });
});

describe("scaleEditToSource", () => {
  it("maps preview-pixel crop and text size onto the original", () => {
    const edit = {
      ...initialImageEdit(),
      crop: { x: 50, y: 0, width: 50, height: 25 },
      text: [
        { id: "t", text: "x", x: 0.5, y: 0.5, width: 20, size: 10, color: "#fff", bold: false, angle: 0 },
      ],
    };
    const out = scaleEditToSource(edit, { width: 100, height: 50 }, { width: 400, height: 200 });
    expect(out.crop).toEqual({ x: 200, y: 0, width: 200, height: 100 });
    expect(out.text[0]).toMatchObject({ size: 40, width: 80, x: 0.5, y: 0.5 });
    expect(scaleEditToSource(edit, { width: 400, height: 200 }, { width: 400, height: 200 })).toBe(edit);
  });
});
