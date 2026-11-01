# Proposed catalogue entries: what was merged (2026-10-12)

These three files are the research input and are kept unchanged. The entries that satisfied the merge rule were written into `../models.json` with these changes: `reconciled: false`, `max_len` where the notes give a character limit, and derived price bases in place of caller-stated ones. Notes: `docs/reviews/2026-10-12-catalogue-proposals-*.md`.

Merge rule: the estimate never depends on a number the caller states. A price basis is read from an input that is sent to the provider (text length, `width` x `height`, the count of input URLs), or from a flat price.

## Merged (15)

| Proposal file | Entry | Price basis |
| --- | --- | --- |
| object-swap-and-veo-4k | `fal/fal-ai/ltx-2/retake-video` | $0.10 per second of the sent `duration` |
| object-swap-and-veo-4k | `fal/fal-ai/veo3.1` | per second by `resolution` ($0.20 at 720p/1080p, $0.40 at 4k), silent |
| image-models | `fal/fal-ai/flux-2/lora` | $0.021 per megapixel of the sent `width` x `height`; x2.5 whenever `loras` are sent |
| image-models | `fal/fal-ai/flux-2/lora/edit` | as above plus 1 MP per input image URL sent |
| image-models | `fal/bytedance/seedream/v5/lite/text-to-image`, `.../edit` | flat $0.035 |
| image-models | `fal/fal-ai/hidream-o1-image` | $0.01 per megapixel of the sent `width` x `height` (no reference inputs) |
| audio-upscale-transcription | `fal/google/gemini-3.8-flash-tts` | $0.045 per 1000 characters of the sent prompt |
| audio-upscale-transcription | `fal/fal-ai/inworld-tts-2` | $0.01 per 1000 characters, 2000 characters max |
| audio-upscale-transcription | `wavespeed/inworld/realtime-tts-2` | $0.05 per 1000 characters, 2000 characters max |
| audio-upscale-transcription | `fal/fal-ai/lyria3`, `wavespeed/google/lyria-3.5/music`, `.../lyria-3-pro/music`, `.../lyria-3-clip/music` | flat per audio or run |
| audio-upscale-transcription | `fal/fal-ai/stable-audio-3/small/sfx/text-to-audio` | flat $0.0206 |

## Not merged

- `fal-ai/flux-2-max/edit`: input images count as processed megapixels, their sizes are unreadable before submit (remote https URLs only) and fal publishes no count cap.
- `fal-ai/hidream-o1-image/edit`: the notes do not say whether reference images count toward billed megapixels, and their sizes are unreadable before submit.
- Everything the notes list as "not proposed" (pixverse swap, Reve 2.1, MAI-Image-2.5-Pro, Seedream 5.0 Pro, WaveSpeed hosts of FLUX.2 max edit and HiDream edit, Cartesia, Suno, Udio, Topaz Starlight, transcription models) stays unmerged for the reasons in the notes.
