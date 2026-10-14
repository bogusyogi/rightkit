// Positive and negative code samples for source fingerprints added to previously declaration-only
// capabilities. Positives are hand-written reimplementations; negatives are normal RightKit usage or
// unrelated code that must never match. Consumed by ownership-fingerprint-samples.test.mjs and ownership.test.mjs.
export const FINGERPRINT_SAMPLES = {
  "fsindex.api.getattrlistbulk": {
    "positive": [
      "let n = unsafe { libc::getattrlistbulk(fd, &mut list as *mut _ as *mut c_void, buf.as_mut_ptr().cast(), buf.len(), 0) };"
    ],
    "negative": [
      "let index = rightkit_fsindex::Index::open(&root)?;",
      "// getattrlistbulk(2) is what the index uses underneath",
      "let name = \"getattrlistbulk\";"
    ]
  },
  "fsindex.api.fsevent-stream": {
    "positive": [
      "let stream = FSEventStreamCreate(kCFAllocatorDefault, callback, &context, paths, kFSEventStreamEventIdSinceNow, 0.3, flags);"
    ],
    "negative": [
      "let mut watcher = rightkit_file_watch::Watcher::new(&root)?;",
      "// FSEventStreamCreate is wrapped by the crate"
    ]
  },
  "jobs.sql.job-table": {
    "positive": [
      "conn.execute_batch(\"CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, status TEXT NOT NULL, attempts INTEGER DEFAULT 0, payload TEXT)\")?;"
    ],
    "negative": [
      "conn.execute_batch(\"CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY, status TEXT, attempts INTEGER)\")?;",
      "conn.execute(\"CREATE TABLE jobs (id TEXT, title TEXT, status TEXT)\", [])?;",
      "let store = rightkit_job_queue::JobStore::open(&db)?;"
    ]
  },
  "jobs.sql.claim-next": {
    "positive": [
      "conn.execute(\"UPDATE jobs SET status = 'running', attempts = attempts + 1 WHERE id = ?1 AND status = 'queued'\", [id])?;"
    ],
    "negative": [
      "conn.execute(\"UPDATE jobs SET title = ?1 WHERE id = ?2\", params)?;",
      "let claimed = store.claim_next(worker_id)?;"
    ]
  },
  "license.api.ed25519-verifying-key": {
    "positive": [
      "let key = VerifyingKey::from_bytes(&PUBKEY).map_err(|_| LicenseError::Malformed)?;"
    ],
    "negative": [
      "let ent = rightkit_license::verify_token(&token, &PUBKEY, \"app\", &device, now)?;",
      "// VerifyingKey::from_bytes is wrapped by verify_token",
      "let key = VerifyingKey::from_bytes(&bytes).map_err(|_| CryptoError::InvalidPublicKey)?;"
    ]
  },
  "license.endpoint.activation": {
    "positive": [
      "const res = await fetch(`https://${host}/v1/licenses/activate`, { method: 'POST', body });",
      "let r = client.post(\"https://coderight.cc/v1/licenses/refresh\").send()?;"
    ],
    "negative": [
      "const client = createLicenseClient({ app: 'viewright', host });",
      "const url = `https://${host}/v1/downloads/latest`;",
      "const r = await apps.request('/v1/licenses/activate', { method: 'POST' });"
    ]
  },
  "logs.invoke.rightkit-commands": {
    "positive": [
      "await invoke('rightkit_logs_write', { events });",
      "const bundle = await invoke<Bundle>(\"rightkit_logs_collect\");"
    ],
    "negative": [
      "const logs = createRightKitLogs({ invoke, app: 'viewright' });",
      "await invoke('open_folder', { path });"
    ]
  },
  "tauri.rs.standard-command-fn": {
    "positive": [
      "#[tauri::command]\nfn secure_update_license_get(app: AppHandle) -> Result<Option<String>, String> {\n    let entry = keyring::Entry::new(\"app\", \"license\").map_err(|e| e.to_string())?;\n    entry.get_password().map(Some).map_err(|e| e.to_string())\n}"
    ],
    "negative": [
      "#[tauri::command]\nfn secure_update_license_get(state: State<Host>) -> Result<Option<License>, String> {\n    rightkit_device::SecureService::new(&state.store, &NoopHook).get().map_err(|e| e.to_string())\n}",
      "fn open_settings_window(app: AppHandle) {}"
    ]
  },
  "tauri.invoke.standard-commands": {
    "positive": [
      "const id = await invoke<string>('secure_device_id_get_or_create');",
      "await invoke(\"secure_update_license_set\", { value });"
    ],
    "negative": [
      "const client = createRightSuiteAppTauriClient('viewright', { invoke });",
      "await invoke('open_folder', { path });"
    ]
  },
  "sidecar.rs.json-line-read": {
    "positive": [
      "reader.read_line(&mut line)?;\nlet msg: Request = serde_json::from_str(&line)?;"
    ],
    "negative": [
      "let msg: Request = rightkit_sidecar::read_json_line(&mut reader, &mut buf, LIMIT)?.unwrap();",
      "reader.read_line(&mut line)?;\nprintln!(\"{line}\");"
    ]
  },
  "sidecar.rs.length-prefix": {
    "positive": [
      "stream.read_exact(&mut len)?;\nlet n = u32::from_be_bytes(len) as usize;"
    ],
    "negative": [
      "let (frame, _) = rightkit_sidecar::length_prefixed::read_frame(&mut r, MAX)?;",
      "let n = u32::from_be_bytes(raw);"
    ]
  },
  "service.api.peer-credentials": {
    "positive": [
      "unsafe { getpeereid(fd, &mut uid, &mut gid) == 0 && uid == geteuid() }",
      "let opt = SO_PEERCRED;"
    ],
    "negative": [
      "let server = rightkit_daemon::Server::bind(&suite, \"engine\", handler)?;",
      "// getpeereid is checked inside the service crate"
    ]
  },
  "service.api.owner-only-pipe-sddl": {
    "positive": [
      "let sddl = wide(&format!(\"D:P(A;;GA;;;{})\", user_tag()?));"
    ],
    "negative": [
      "let h = CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, 0, 1, 4096, 4096, 0, &sa);",
      "use rightkit_daemon::Client;",
      "let pipe = ClientOptions::new().open(name)?;"
    ]
  },
  "service.discovery.service-json": {
    "positive": [
      "let record = dir.join(\"service.json\");",
      "const record = path.join(runtimeDir, 'service.json');"
    ],
    "negative": [
      "let record = rightkit_daemon::service_record_path(&suite, \"engine\");",
      "let log = dir.join(\"service.jsonl\");"
    ]
  },
  "speech.protocol.schema-name": {
    "positive": [
      "if frame.schema_name != \"file_transcription_status\" { return Err(Error::Protocol); }",
      "if response.schema_name == \"engine_health\" {}"
    ],
    "negative": [
      "let client = rightkit_speech_client::SpeechClient::discover()?;",
      "if frame.schema_name == \"other_schema\" {}"
    ]
  },
  "speech.spawn.speech-service-flag": {
    "positive": [
      "Command::new(engine).arg(\"--speech-service\").spawn()?;"
    ],
    "negative": [
      "rightkit_speech_client::spawn_service(&engine, wait)?;",
      "Command::new(engine).arg(\"--version\").output()?;"
    ]
  },
  "search.sql.fts5-query": {
    "positive": [
      "let mut stmt = conn.prepare(\"SELECT rowid, snippet(notes_fts, 0, '<b>', '</b>', '...', 12) FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY bm25(notes_fts)\")?;"
    ],
    "negative": [
      "conn.prepare(\"SELECT rowid FROM message_vec WHERE embedding MATCH ?1 ORDER BY distance\")?;",
      "let sql = rightkit_search::compile(&query, &schema)?;",
      "let mut stmt = conn.prepare(\"SELECT id FROM notes WHERE title LIKE ?1\")?;"
    ]
  },
  "playback.ffmpeg.faststart-copy": {
    "positive": [
      "Command::new(ffmpeg).args([\"-i\", src, \"-c:v\", \"copy\", \"-c:a\", \"aac\", \"-movflags\", \"+faststart\", out]).status()?;",
      "ffmpeg(src).outputOptions('-c:v copy').outputOptions('-movflags +faststart').save(out);"
    ],
    "negative": [
      "rightkit_playback::compatibility_copy(&ffmpeg, &source, &root, token)?;",
      "Command::new(ffmpeg).args([\"-i\", a, \"-vn\", out]).status()?;",
      "Command::new(\"ffmpeg\").args([\"-i\", a, \"-c:v\", \"libx264\", \"-movflags\", \"+faststart\", out]).status()?;"
    ]
  },
  "automation.api.ax-element": {
    "positive": [
      "let app = AXUIElementCreateApplication(pid)"
    ],
    "negative": [
      "let tree = rightkit_automation::observe(&target)?;",
      "// AXUIElementCreateApplication is used inside the crate"
    ]
  },
  "automation.api.cgevent-synth": {
    "positive": [
      "let ev = CGEventCreateKeyboardEvent(src, 0x24, true);",
      "CGEventPost(kCGHIDEventTap, ev);"
    ],
    "negative": [
      "let out = automation.act(&request)?;",
      "let flags = CGEventSourceFlagsState(0);"
    ]
  },
  "automation.api.uia-com": {
    "positive": [
      "let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;"
    ],
    "negative": [
      "let out = automation.act(&request)?;",
      "let uiautomation_enabled = true;"
    ]
  },
  "automation.api.sendinput": {
    "positive": [
      "let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };"
    ],
    "negative": [
      "let out = automation.act(&request)?;",
      "let send_input_mode = true;"
    ]
  },
  "overlay.api.ws-ex-noactivate": {
    "positive": [
      "let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize;"
    ],
    "negative": [
      "rightkit_overlay::platform::make_noactivate(hwnd);",
      "let overlay_rect = Rect { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };"
    ]
  },
  "overlay.api.ns-ignores-mouse": {
    "positive": [
      "panel.ignoresMouseEvents = true",
      "[panel setIgnoresMouseEvents:YES];"
    ],
    "negative": [
      "let panel = RightKitOverlayPanel(contentRect: rect, kind: .heardRightPill)",
      "if panel.ignoresMouseEvents == false {}"
    ]
  },
  "keychain.api.secitem": {
    "positive": [
      "let status = SecItemAdd(item as CFDictionary, nil)",
      "guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }"
    ],
    "negative": [
      "try store.save(account: \"api\", value: token)",
      "// SecItemAdd is wrapped by RightKitAppleCredentials"
    ]
  },
  "app-shell.jsx.drag-region": {
    "positive": [
      "<header data-tauri-drag-region className=\"titlebar\">"
    ],
    "negative": [
      "<AppShell sidebar={<Nav />} titlebar={<TitleBar />}>",
      "// data-tauri-drag-region is provided by the shell"
    ]
  },
  "onboarding.storage.completion-flag": {
    "positive": [
      "localStorage.setItem('app.onboarding.done', '1');",
      "const seen = localStorage.getItem(\"welcome-seen\");"
    ],
    "negative": [
      "localStorage.setItem('sidebar.width', '240');",
      "<Onboarding steps={steps} onComplete={finish} />"
    ]
  },
  "legal.storage.assent-flag": {
    "positive": [
      "const ok = localStorage.getItem('eula-accepted') === '1';",
      "localStorage.setItem('app.terms.accepted', 'true');"
    ],
    "negative": [
      "localStorage.getItem('theme');",
      "<LegalGate manifest={manifest} onAssent={record} />"
    ]
  },
  "platform.js.user-agent-sniff": {
    "positive": [
      "const isMac = navigator.platform.includes('Mac');",
      "const isMac = /Mac|iPhone|iPad/.test(navigator.platform);",
      "const mac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;"
    ],
    "negative": [
      "const platform = getPlatform();",
      "const lang = navigator.language.includes('en');"
    ]
  },
  "theme.math.relative-luminance": {
    "positive": [
      "const lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;",
      "let l = .2126 * r + .7152 * g + .0722 * b;"
    ],
    "negative": [
      "const ratio = contrastRatio(fg, bg);",
      "const alpha = 0.2126;"
    ]
  },
  "theme.runtime.prefers-dark": {
    "positive": [
      "const mq = window.matchMedia('(prefers-color-scheme: dark)');"
    ],
    "negative": [
      "applyBrand(document.documentElement, brand, mode);",
      "const mq = window.matchMedia('(max-width: 600px)');"
    ]
  },
  "updates.api.download-and-install": {
    "positive": [
      "await update.downloadAndInstall((e) => progress(e));",
      "update.download_and_install(|_, _| {}, || {}).await?;"
    ],
    "negative": [
      "await updater.installAvailable();",
      "const policy = createUpdatePolicy({ check });"
    ]
  },
  "qa.cdp.method-call": {
    "positive": [
      "const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });",
      "session.call(\"Runtime.evaluate\", {\"expression\": \"1\"})"
    ],
    "negative": [
      "const evidence = await captureEvidence(session);",
      "socket.send('ping');"
    ]
  },
  "qa.chrome.remote-debugging-flag": {
    "positive": [
      "spawn(chromePath, ['--headless=new', '--remote-debugging-port=0', url]);",
      "Command::new(chrome).arg(\"--remote-debugging-port=9222\").spawn()?;"
    ],
    "negative": [
      "spawn(process.execPath, ['--test']);",
      "Command::new(\"git\").arg(\"status\").output()?;"
    ]
  },
  "qa-chrome.url.chrome-for-testing": {
    "positive": [
      "const res = await fetch('https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json');"
    ],
    "negative": [
      "const { executablePath } = await resolveChrome({ channel: 'stable' });",
      "const res = await fetch('https://example.com/versions.json');"
    ]
  },
  "release.sign.codesign-signtool": {
    "positive": [
      "execFileSync('codesign', ['--force', '--sign', identity, app]);",
      "Command::new(\"signtool\").args([\"sign\", \"/fd\", \"SHA256\", exe]).status()?;",
      "execSync(`codesign --force --sign \"${id}\" \"${app}\"`);"
    ],
    "negative": [
      "execFileSync('codesign', ['--verify', '--strict', app]);",
      "execFileSync('git', ['status']);"
    ]
  },
  "release.notarize.notarytool": {
    "positive": [
      "execFileSync('xcrun', ['notarytool', 'submit', zip, '--wait']);",
      "execSync(`xcrun notarytool submit ${zip}`);"
    ],
    "negative": [
      "execFileSync('xcrun', ['simctl', 'list']);",
      "const note = 'notarytool is run by right-release';"
    ]
  }
};
