// Web Audio chime for new mail — no asset; plays even when the app is focused
// and the OS toast is suppressed. Pure (touches only the Web Audio API).

/** A short two-note chime for new mail. */
export function playChime(): void {
  try {
    const Ctx =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    const ac = new Ctx();
    const now = ac.currentTime;
    (
      [
        [880, 0],
        [1180, 0.12],
      ] as const
    ).forEach(([freq, at]) => {
      const o = ac.createOscillator();
      const g = ac.createGain();
      o.type = "sine";
      o.frequency.value = freq;
      o.connect(g);
      g.connect(ac.destination);
      g.gain.setValueAtTime(0.0001, now + at);
      g.gain.exponentialRampToValueAtTime(0.18, now + at + 0.02);
      g.gain.exponentialRampToValueAtTime(0.0001, now + at + 0.22);
      o.start(now + at);
      o.stop(now + at + 0.24);
    });
    window.setTimeout(() => ac.close().catch(() => {}), 600);
  } catch {
    /* audio unavailable */
  }
}
