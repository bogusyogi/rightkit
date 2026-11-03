param(
  [string]$ExpansionRoot = "D:\Claude\heardright\train\corpus_expansion_20260607"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Speech

$terms = @("sifr", "suffer", "zipper", "zapper", "zepto", "safar", "zapp")
$templates = @(
  "{0}",
  "say {0}",
  "the word is {0}",
  "{0} please"
)
$rates = @(-2, 0, 2)
$outRoot = Join-Path $ExpansionRoot "raw\near_phonetic_tts"
$manifest = Join-Path $ExpansionRoot "manifests\near_phonetic_tts_manifest.jsonl"
New-Item -ItemType Directory -Force -Path $outRoot, (Split-Path $manifest) | Out-Null
if (Test-Path $manifest) { Remove-Item -LiteralPath $manifest -Force }

$probe = [System.Speech.Synthesis.SpeechSynthesizer]::new()
$voices = @($probe.GetInstalledVoices() | Where-Object { $_.Enabled })
$probe.Dispose()
if ($voices.Count -eq 0) { throw "No enabled SAPI voices found" }

foreach ($term in $terms) {
  $termDir = Join-Path $outRoot $term
  New-Item -ItemType Directory -Force -Path $termDir | Out-Null
  foreach ($voice in $voices) {
    $voiceName = $voice.VoiceInfo.Name
    $voiceSafe = ($voiceName -replace '[^a-zA-Z0-9]+','_').Trim('_')
    foreach ($rate in $rates) {
      for ($i = 0; $i -lt $templates.Count; $i++) {
        $text = [string]::Format($templates[$i], $term)
        $file = Join-Path $termDir ("{0}_rate{1}_t{2}.wav" -f $voiceSafe, $rate, $i)
        $synth = [System.Speech.Synthesis.SpeechSynthesizer]::new()
        try {
          $synth.SelectVoice($voiceName)
          $synth.Rate = $rate
          $synth.SetOutputToWaveFile($file)
          $synth.Speak($text)
        } finally {
          $synth.Dispose()
        }
        $row = [ordered]@{
          schema_version = 1
          term = $term
          text = $text
          voice = $voiceName
          rate = $rate
          path = $file
          label = 0
          word = ""
          semantic_bucket = "near_phonetic"
          source_bucket = "sapi_tts_near_phonetic"
          false_fire_type = "phonetic_near_match_candidate"
        }
        ($row | ConvertTo-Json -Compress) | Add-Content -LiteralPath $manifest -Encoding utf8
      }
    }
  }
}

"tts_manifest=$manifest"
