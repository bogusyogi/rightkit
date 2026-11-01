# alias: merged-away name -> (final name, reason)
ALIAS={
 'rightkit-agent-runtime':('rightkit-agent-engine','always-on agent task contracts & agent loop are one engine'),
 'rightkit-remote-control':('rightkit-agent-gateway','remote agent tools + approvals/audit = same gateway as MailRight agent methods'),
 'rightkit-work-router':('rightkit-agent-orchestration','capability routing, budgets, escalation & handoff are orchestration'),
 'rightkit-claude-accounts':('rightkit-ai-accounts','Claude/Codex accounts, session sync & usage meters'),
 'rightkit-ai-usage':('rightkit-ai-accounts','Claude/Codex accounts, session sync & usage meters'),
 'rightkit-social-accounts':('rightkit-social-publish','accounts are publish prerequisites'),
 'rightkit-link-tracking':('rightkit-social-analytics','links & visits are analytics'),
 'rightkit-content-briefs':('rightkit-content-plan','briefs, variants, hooks, drafts & calendar'),
 'rightkit-content-calendar':('rightkit-content-plan','briefs, variants, hooks, drafts & calendar'),
 'rightkit-text-gate':('rightkit-review','deterministic text gate is a review check'),
 'rightkit-source-detect':('rightkit-media-fetch','classification is the fetch front door'),
 'rightkit-stock-media':('rightkit-media-fetch','stock acquisition is media fetch'),
 'rightkit-text-enrich':('rightkit-library','enrichment runs on library ingest'),
 'rightkit-media-probe':('rightkit-ffmpeg','pure-Rust probe beside ffprobe'),
 'rightkit-local-image':('rightkit-local-gen','local generative runtimes'),
 'rightkit-lipsync':('rightkit-local-gen','local generative runtimes'),
 'rightkit-music-gen':('rightkit-local-gen','local generative runtimes'),
 'rightkit-upscale':('rightkit-local-gen','local generative runtimes'),
 'rightkit-voice-library':('rightkit-tts','voice library belongs with TTS'),
 'rightkit-timeline-interchange':('rightkit-timeline','FCPXML/OTIO export of the timeline'),
 'rightkit-edit-rules':('rightkit-autoedit','rules drive autoedit'),
 'rightkit-media-control':('rightkit-automation','system media keys are OS automation'),
 'rightkit-service-host':('rightkit-daemon','service lifecycle/health belongs to daemon host'),
 'rightkit-specs':('rightkit-evidence','specs & coverage are evidence artefacts'),
 '@rightkit/mesh-ui':('@rightkit/settings-ui','devices/pairing are settings sections'),
 '@rightkit/social-client':('@rightkit/social-ui','one social/content ops UI package'),
 '@rightkit/content-ui':('@rightkit/social-ui','one social/content ops UI package'),
 '@rightkit/queue-ui':('@rightkit/library-ui','ingest queue + library'),
 '@rightkit/overlay-ui':('@rightkit/dictation-ui','dictation pill, polish & vocabulary UI'),
 '@rightkit/text-polish-ui':('@rightkit/dictation-ui','dictation pill, polish & vocabulary UI'),
 '@rightkit/timeline-ui':('@rightkit/video-ui','one video editor UI package'),
 'RightKitLibraryUI':('RightKitLibrary','library w/ SwiftUI target'),
 'RightKitRemoteUI':('RightKitAgent','phone remote control of agents'),
'RightKitSend':('rightkit-send','Swift binding target of the existing send crate'),
 'RightKitExtract':('rightkit-web-extract','Swift extractor target of web-extract'),
}
def final(t): return ALIAS.get(t,(t,''))[0]
def kind(t):
    if t.startswith('@rightkit/'): return 'npm'
    if t.startswith('RightKit'): return 'Swift'
    if t.startswith('rightkit-'): return 'crate'
    return 'other'
