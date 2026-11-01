import { Badge, Button, Notice } from '@rightkit/react-shell/react';
import { IconLock, IconPlay } from '../components/icons';

interface Spec {
  eyebrow: string;
  title: string;
  lede: string;
  locks?: Array<[string, string]>;
  steps: Array<[string, string]>;
  cta: string;
}

export const COMING_SOON: Record<'kdp' | 'production' | 'motion', Spec> = {
  kdp: {
    eyebrow: 'KDP workflow',
    title: 'Color thumbnail',
    lede: 'Turn one line-art page into a colored cover thumbnail. Everything except the coloring material is locked.',
    locks: [['Model', 'NB2'], ['Aspect', '1:1'], ['Resolution', '2K'], ['Prompt', 'KDP colorize v3']],
    steps: [['Pick a source page', 'Line-art images indexed from your KDP folder'], ['Coloring material', 'The only creative choice in this workflow.']],
    cta: 'Colorize thumbnail'
  },
  production: {
    eyebrow: 'Production workflow',
    title: 'Production stack',
    lede: 'Plan a pack of images, clips and voice lines together, with one cost check before anything runs.',
    steps: [['Choose a pack', 'Start from a template or an empty stack'], ['Review the plan', 'Every step shows its model and cost']],
    cta: 'Run preflight'
  },
  motion: {
    eyebrow: 'Create · renders on this machine',
    title: 'Motion',
    lede: 'Deterministic, re-renderable graphics with a preview stage and timeline.',
    steps: [['Pick a template', 'Title card, lower third, captions'], ['Edit on the timeline', 'Tracks, clips and keyframes']],
    cta: 'Render MP4'
  }
};

export function ComingSoon({ id }: { id: keyof typeof COMING_SOON }) {
  const spec = COMING_SOON[id];
  return (
    <div className="content view">
      <section className="work">
        <div className="eyebrow">{spec.eyebrow}</div>
        <h1>{spec.title}</h1>
        <p className="lede">{spec.lede}</p>
        <div className="locks">
          <span className="lock" style={{ background: 'var(--warn-soft)', color: 'var(--warn)' }}>Coming soon</span>
          {spec.locks?.map(([k, v]) => (
            <span className="lock" key={k}><IconLock />{k} <b>{v}</b></span>
          ))}
        </div>
        <div style={{ opacity: 0.55 }}>
          {spec.steps.map(([h, hint], i) => (
            <div className="step" key={h}>
              <div className="num">{i + 1}</div>
              <div className="step-body"><h3>{h}</h3><p className="hint">{hint}</p>
                <Badge>Unavailable</Badge>
              </div>
            </div>
          ))}
        </div>
        <Notice tone="info">This workflow needs engine commands that are not available yet. Use Image, Video and Voice to generate in the meantime.</Notice>
      </section>
      <aside className="run">
        <div className="run-head"><div className="eyebrow">Run</div><h3 style={{ margin: '2px 0 0', fontSize: 15 }}>Not available yet</h3></div>
        <div className="run-body"><div className="preview">Preview appears here<br />once this workflow ships</div></div>
        <div className="run-foot">
          <Button type="button" variant="primary" disabled><IconPlay />{spec.cta}</Button>
          <div className="foot-note">Coming soon</div>
        </div>
      </aside>
    </div>
  );
}
