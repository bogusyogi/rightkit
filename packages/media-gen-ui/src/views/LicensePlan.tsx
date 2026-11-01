import { useEffect, useState } from 'react';
import { isPro, type LicenseView } from '@rightkit/license';
import { licenseClient } from '../lib/license';
import { Card } from '@rightkit/react-shell/react';

/** What Pro adds. Keep in step with the gating table in crates/genright-engine/src/entitlement.rs. */
const PRO_FEATURES = [
  'Cloud providers (fal, evolink, heygen, elevenlabs, wavespeed, nanobanana)',
  'Character reference batches'
];
const FREE_FEATURES = ['Local image, video, voice, music and upscale models', 'Library, stock and Motion', 'Cost estimates'];

/** Plan panel above the standard licence rows: Free or Pro, the trial status (unavailable), and the feature split. */
export function LicensePlan() {
  const [view, setView] = useState<LicenseView | null>(null);
  useEffect(() => {
    let alive = true;
    let stop: (() => void) | undefined;
    licenseClient.status().then((v) => { if (alive) setView(v); }, () => { if (alive) setView(null); });
    licenseClient.onChange((v) => { if (alive) setView(v); }).then(
      (off) => { if (alive) stop = off; else off(); },
      () => undefined
    );
    return () => { alive = false; stop?.(); };
  }, []);
  // A display hint only; the engine verifies the licence at every Pro command.
  const pro = isPro(view);
  const plan = !pro ? 'Free' : view?.lifecycle === 'trial' ? 'Pro trial' : 'Pro';
  return (
    <Card flush className="group-box">
      <div className="setting"><span>Plan</span><span style={{ color: 'var(--ink-2)' }}>{plan}</span></div>
      <div className="setting"><span>Free</span><span style={{ color: 'var(--ink-2)' }}>{FREE_FEATURES.join('; ')}</span></div>
      <div className="setting"><span>Pro</span><span style={{ color: 'var(--ink-2)' }}>{PRO_FEATURES.join('; ')}</span></div>
      {/* RightKit ruling 12b1a381 #5: the host app is internal; native trial disabled, no trial endpoint. */}
      {pro ? null : <div className="setting"><span>Pro trial</span><span style={{ color: 'var(--ink-2)' }}>Unavailable</span></div>}
    </Card>
  );
}
