import type { ReactEventHandler } from "react";
import ReactCrop, { type PercentCrop } from "react-image-crop";
import "react-image-crop/dist/ReactCrop.css";
import type { CropRect } from "./imageEdit.js";
import type { ImageSize } from "./imageViewerState.js";
import { clampCrop } from "./cropMath.js";

interface Props {
  src: string;
  imageSize: ImageSize;
  crop: CropRect;
  aspect: number | null;
  filter: string;
  onImageLoad: ReactEventHandler<HTMLImageElement>;
  onChange: (crop: CropRect) => void;
}

export function ImageCropTool({
  src,
  imageSize,
  crop,
  aspect,
  filter,
  onImageLoad,
  onChange,
}: Props) {
  const percentCrop = toPercentCrop(crop, imageSize);
  return (
    <ReactCrop
      crop={percentCrop}
      aspect={aspect ?? undefined}
      keepSelection
      minWidth={24}
      minHeight={24}
      ruleOfThirds
      onChange={(_, percent) => onChange(fromPercentCrop(percent, imageSize))}
      className="rk-iv-crop"
      style={{ width: imageSize.width, height: imageSize.height }}
    >
      <img
        src={src}
        alt=""
        className="rk-iv-img"
        style={{ filter, width: imageSize.width, height: imageSize.height }}
        draggable={false}
        onLoad={onImageLoad}
      />
    </ReactCrop>
  );
}

function toPercentCrop(crop: CropRect, image: ImageSize): PercentCrop {
  return {
    unit: "%",
    x: (crop.x / image.width) * 100,
    y: (crop.y / image.height) * 100,
    width: (crop.width / image.width) * 100,
    height: (crop.height / image.height) * 100,
  };
}

function fromPercentCrop(crop: PercentCrop, image: ImageSize): CropRect {
  return clampCrop(
    {
      x: (crop.x / 100) * image.width,
      y: (crop.y / 100) * image.height,
      width: (crop.width / 100) * image.width,
      height: (crop.height / 100) * image.height,
    },
    image,
  );
}
