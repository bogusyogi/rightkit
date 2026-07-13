import { describe, expect, it } from 'vitest';
import { validateNativeQaContract } from './guard.js';

const validCargo = `
[features]
qa-native = ["dep:tauri-plugin-wdio-webdriver"]

[dependencies]
tauri-plugin-wdio-webdriver = { version = "1", optional = true }
`;

const validRust = `
#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native must never be enabled in release builds");

#[cfg(all(debug_assertions, feature = "qa-native"))]
if std::env::var("RIGHTKIT_QA_NATIVE").as_deref() == Ok("1") {
  builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
}
`;

describe('native QA production-backdoor guard', () => {
  it('accepts an optional debug-only WebDriver feature', () => {
    expect(validateNativeQaContract({
      cargoToml: validCargo,
      rustSource: validRust,
      packageScripts: {
        'qa:native': 'tauri dev --features qa-native',
        'mac:dmg:notarized': 'node scripts/mac-dmg.mjs --notarize',
        'release:patch:win': 'right-release run --tier patch --platform win',
      },
    })).toEqual([]);
  });

  it('rejects a non-optional plugin or any release/build script enabling qa-native', () => {
    const issues = validateNativeQaContract({
      cargoToml: validCargo.replace(', optional = true', ''),
      rustSource: validRust,
      packageScripts: {
        'qa:native': 'tauri dev --features qa-native',
        'release:patch:win': 'tauri build --release --features qa-native',
      },
    });
    expect(issues).toEqual(expect.arrayContaining([
      expect.stringMatching(/optional/i),
      expect.stringMatching(/release:patch:win/),
    ]));
  });
});
