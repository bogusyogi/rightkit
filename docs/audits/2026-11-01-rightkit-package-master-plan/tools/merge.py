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
    if t=='legion': return 'Legion'
    return 'other'
# Proposed tier for NEW packages: 'private' = product domain logic/presentation (P13 app-specific class), published to a private registry;
# everything else 'public' (generic add-on, MIT/Apache, crates.io/npm).
PRIVATE={'rightkit-code-audit','rightkit-agent-server','@rightkit/agent-ui','rightkit-social-publish','rightkit-social-analytics','rightkit-social-inbox','rightkit-social-mock','@rightkit/social-ui',
 'rightkit-content-plan','rightkit-content-proof','rightkit-mail','@rightkit/mail-ui','@rightkit/workspace-ui','rightkit-autoedit','rightkit-shorts','rightkit-video-project','@rightkit/video-ui',
 'rightkit-prompt-recipes','rightkit-media-pipeline','@rightkit/media-gen-ui','rightkit-dictation','rightkit-text-polish','rightkit-voice-commands','RightKitDictation','@rightkit/dictation-ui',
 'rightkit-ai-accounts','rightkit-chat-bridge','rightkit-launcher','rightkit-clipboard-history','rightkit-desktop-tweaks','rightkit-disk-cleanup','rightkit-app-manager','RightKitNotch','RightKitSystemUI','@rightkit/system-ui',
 '@rightkit/document-workspace','rightkit-document-workspace','@rightkit/markdown-editor','@rightkit/html-editor','@rightkit/docx-editor','rightkit-ml-lab','RightKitAgent','rightkit-agent-demo','@rightkit/library-ui','RightKitLibrary','rightkit-work-router','rightkit-privileged-helper'}
def tier(t): return 'private' if t in PRIVATE else 'public'
APPTEST_RX=r'(tests/scenarios|scenarios-ui|/e2e/|crates/e2e/|-e2e/|qa-e2e/|installed_matrix|engine/tests/|bins/coderight/tests/|scripts/qa-|scripts/sidecar-smoke/|src-tauri/tests/|crates/cutright-engine/tests/|^qa/|hub/qa-e2e/|ios-native/Tests/|UITests/)'

APPTEST_CAP_RX=r'(e2e|journey|E2E|iOS tests|deterministic mock of social)'

# v4: no private tier. Single-app product domain logic & UI stays in the app (P13 app-specific class).
PRODUCT={'rightkit-mail', 'rightkit-agent-demo', 'rightkit-shorts', 'rightkit-voice-commands', 'rightkit-autoedit', 'rightkit-dictation', '@rightkit/system-ui', 'rightkit-text-polish', 'rightkit-desktop-tweaks', '@rightkit/docx-editor', 'rightkit-media-pipeline', 'rightkit-video-project', 'RightKitNotch', 'rightkit-document-workspace', 'rightkit-privileged-helper', '@rightkit/html-editor', '@rightkit/document-workspace', 'rightkit-chat-bridge', 'rightkit-ai-accounts', 'RightKitSystemUI', 'rightkit-content-plan', '@rightkit/video-ui', '@rightkit/mail-ui', '@rightkit/markdown-editor', '@rightkit/media-gen-ui', 'RightKitDictation', 'rightkit-agent-server', 'rightkit-content-proof', 'rightkit-social-publish', 'rightkit-launcher', 'rightkit-code-audit', 'rightkit-clipboard-history', 'rightkit-prompt-recipes', '@rightkit/workspace-ui', 'rightkit-social-inbox', 'rightkit-app-manager', '@rightkit/social-ui', 'rightkit-ml-lab', 'rightkit-disk-cleanup'}
def tier(t): return 'public'
PRODUCT=set()  # v5: single-app capabilities go to RightKit; product-specific = per-app look/composition + declared exceptions (shell)
