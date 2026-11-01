// Slots for parity-lane panels in the Generate workspace. Each panel file is
// owned by one lane; this file only mounts them.
import { CharacterPanel, CharacterPanelKinds } from './CharacterPanel';
import { ImageQualityPanel, ImageQualityPanelKinds } from './ImageQualityPanel';
import type { StudioPanelProps } from './types';
import { VideoShotPanel, VideoShotPanelKinds } from './VideoShotPanel';

export function StudioPanels(props: StudioPanelProps) {
  return (
    <>
      {ImageQualityPanelKinds.includes(props.kind) ? <ImageQualityPanel {...props} /> : null}
      {VideoShotPanelKinds.includes(props.kind) ? <VideoShotPanel {...props} /> : null}
      {CharacterPanelKinds.includes(props.kind) ? <CharacterPanel {...props} /> : null}
    </>
  );
}
