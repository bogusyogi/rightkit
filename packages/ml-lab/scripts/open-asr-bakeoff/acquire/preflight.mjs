const roster = [
  ["CohereLabs/cohere-transcribe-03-2026", "b1eacc2686a3d08ceaae5f24a88b1d519620bc09", "cohere-transcribe"],
  ["ibm-granite/granite-speech-4.1-2b", "de575db64086f84fdc79da4932d1076e965bc546", "granite-speech-4.1-2b"],
  ["Qwen/Qwen3-ASR-1.7B-hf", "057a3b044fcd31c433e7971ab40d68d20e7eae6d", "qwen3-asr-1.7b"],
  ["AutoArk-AI/ARK-ASR-0.6B", "45776b56d58cdfb2e2eb632f7e110f38684633e0", "ark-asr-0.6b"],
  ["AutoArk-AI/ARK-ASR-3B", "1e28271b79edc97635783bea65abc89195a09ed3", "ark-asr-3b"],
  ["bosonai/higgs-audio-v3-stt", "2ffd1aa39f5a1266931e405cba12e404a9f994b2", "higgs-audio-v3-stt"],
  ["OpenMOSS-Team/MOSS-Transcribe-preview-2B", "c98175cb20e48bd9be4e95f6c85f2af18899f780", "moss-transcribe-2b"],
  ["nvidia/canary-qwen-2.5b", "b1469e1bba1cfe140205529c79c434ca47180960", "canary-qwen-2.5b"],
];

const token = process.env.HF_TOKEN;

for (const [repo, rev, key] of roster) {
  const url = `https://huggingface.co/api/models/${repo}/tree/${rev}?recursive=1`;
  try {
    const res = await fetch(url, { headers: token ? { Authorization: `Bearer ${token}` } : {} });
    if (!res.ok) {
      console.log(`${key} | ${repo}@${rev} | HTTP ${res.status} ${res.statusText}`);
      continue;
    }
    const data = await res.json();
    let total = 0;
    let count = 0;
    for (const e of data) {
      if (e.type === "file" && typeof e.size === "number") { total += e.size; count++; }
    }
    console.log(`${key} | ${repo}@${rev} | files=${count} | bytes=${total} | GB=${(total/1e9).toFixed(2)}`);
  } catch (err) {
    console.log(`${key} | ${repo}@${rev} | ERROR ${err.message}`);
  }
}
