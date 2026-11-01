import mediaTypes from "../generated/media-types.json";

// Extensions come from the Rust table in crates/detect (media_types.rs), so the
// picker can never lag behind what pasting accepts.
const MEDIA_IMAGE_PDF_EXTENSIONS = [
  ...mediaTypes.video,
  ...mediaTypes.audio,
  ...mediaTypes.image,
  ...mediaTypes.document,
];

export const MEDIA_IMAGE_PDF_FILTERS = [
  {
    name: "Media, images & PDFs",
    extensions: MEDIA_IMAGE_PDF_EXTENSIONS,
  },
];
