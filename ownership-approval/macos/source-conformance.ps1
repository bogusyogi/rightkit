$ErrorActionPreference = 'Stop'
$source = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'ownership-approval.swift')
$infoPlist = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'Info.plist')
$entitlements = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'ownership-approval.entitlements')
$buildScript = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'build.sh')

function Require([bool] $ok, [string] $message) {
  if (-not $ok) { throw "Mac auth source conformance failed: $message" }
}
function Section([string] $start, [string] $end) {
  $from = $source.IndexOf($start, [StringComparison]::Ordinal)
  $to = $source.IndexOf($end, $from + $start.Length, [StringComparison]::Ordinal)
  Require ($from -ge 0 -and $to -gt $from) "missing source section $start"
  return $source.Substring($from, $to - $from)
}
function Count([string] $text, [string] $pattern) {
  return ([regex]::Matches($text, $pattern)).Count
}

$enrollRoot = Section 'private func enrollRoot()' 'private func enroll()'
$enroll = Section 'private func enroll()' 'private func signLoadedDraft()'
$signUi = Section 'private func signLoadedDraft()' 'private func showError('
$signEvent = Section 'private func signKeyLog(' 'private func exportKey('
$signedHelper = Section 'private func signedKeyLog(' 'private func requireEnrollmentPresence('
$presenceHelper = Section 'private func requireEnrollmentPresence(' 'private func jsQuote('
$keychainQuery = Section 'private func keychainQuery(' 'private func ownershipKeychainAccessGroup('
$keychainPreflight = Section 'private func keychainPreflight()' 'private func publicKeyExport('
$persistPublicKey = Section 'private func persistPublicKey(' 'private func loadExistingPublicKey('

Require ((Count $enrollRoot 'authContext\(reason') -eq 1) 'root mode must create one LAContext'
Require ((Count $enrollRoot 'SecureEnclave\.P256\.Signing\.PrivateKey') -eq 1) 'root mode must create one Secure Enclave key'
Require ((Count $enrollRoot 'signedKeyLog\(payload, with: key\)') -eq 1) 'root mode must perform one genesis signature'
Require ((Count $enrollRoot 'requireEnrollmentPresence\(') -eq 0) 'root mode must not add a second proof signature'

Require ((Count $enroll 'authContext\(reason') -eq 1) 'additional enrollment must create one LAContext'
Require ((Count $enroll 'SecureEnclave\.P256\.Signing\.PrivateKey') -eq 1) 'additional enrollment must create one Secure Enclave key'
Require ((Count $enroll 'requireEnrollmentPresence\(privateKey\)') -eq 1) 'additional enrollment must force one presence signature before storage/export'

Require ((Count $signUi 'signKeyLog\(loaded, context: authContext\(reason') -eq 1) 'key-log approval must pass one LAContext into signing'
Require ((Count $signUi 'let context = authContext\(reason') -eq 1) 'legacy approval must create one LAContext'
Require ((Count $signEvent 'keychainQuery\(returnData: true, context: context\)') -eq 1) 'signer retrieval must reuse that LAContext'
Require ((Count $signEvent 'signedKeyLog\(payload, with: key\)') -eq 1) 'signing path must produce one event signature'
Require ((Count $signedHelper 'key\.signature\(for:') -eq 1) 'event helper must perform exactly one private-key operation'
Require ((Count $presenceHelper 'key\.signature\(for:') -eq 1) 'enrollment presence helper must perform exactly one private-key operation'
Require ($source -notmatch 'evaluatePolicy\s*\(') 'do not add separate LA policy authentication'

Require ($keychainQuery -match 'kSecUseDataProtectionKeychain[^\n]*true') 'private-key lookup must explicitly target the data-protection keychain'
Require ($keychainQuery -match 'kSecAttrAccessGroup') 'private-key lookup must pin the signed helper access group'
Require ($keychainPreflight -match 'kSecUseDataProtectionKeychain[^\n]*true') 'preflight must query the data-protection keychain'
Require ($keychainPreflight -match 'kSecAttrAccessGroup[^\n]*group') 'preflight must explicitly query the helper access group'
Require ($keychainPreflight -match 'interactionNotAllowed\s*=\s*true') 'preflight must forbid authentication UI'
Require ($keychainPreflight -match 'errSecMissingEntitlement') 'preflight must diagnose missing signing entitlement'
Require ((Count $persistPublicKey 'kSecUseDataProtectionKeychain[^\n]*true') -eq 1) 'public-key metadata add must use data-protection keychain'
Require ((Count $persistPublicKey 'kSecAttrAccessGroup') -eq 1) 'public-key metadata add must use helper access group'
Require ((Count $enrollRoot 'kSecUseDataProtectionKeychain[^\n]*true') -eq 1) 'root key representation add must use data-protection keychain'
Require ((Count $enroll 'kSecUseDataProtectionKeychain[^\n]*true') -eq 1) 'enrolled key representation add must use data-protection keychain'
Require ($source -match 'ownershipBundleIdentifier\s*=\s*"com\.orthiclabs\.rightkit\.ownership-approval"') 'helper bundle identifier must remain stable'
Require ($source -match 'ownershipKeychainGroup\s*=\s*"6KLGD3LLKF\.com\.orthiclabs\.rightkit\.ownership-approval"') 'keychain access group must match Developer ID application identifier'
Require ($infoPlist -match '<key>CFBundleIdentifier</key>\s*<string>com\.orthiclabs\.rightkit\.ownership-approval</string>') 'app bundle ID must match keychain access group'
Require ($infoPlist -match '<key>CFBundleExecutable</key>\s*<string>ownership-approval</string>') 'app bundle executable must match native helper'
Require ($entitlements -match '<key>com\.apple\.application-identifier</key>\s*<string>6KLGD3LLKF\.com\.orthiclabs\.rightkit\.ownership-approval</string>') 'signing entitlement must grant only app-identifier default group'
Require ($entitlements -notmatch 'keychain-access-groups|com\.apple\.security\.application-groups') 'do not add unverified restricted sharing entitlements'
Require ($source -match 'try keychainPreflight\(\)[\s\S]*?let app = NSApplication\.shared') 'signed keychain preflight must run before UI or native authentication'
Require ($buildScript -match 'swiftc -parse-as-library -O') 'native builder must compile on the Mac host'
Require ($buildScript -match 'signer="\$release_tools/sign-macos\.mjs"[\s\S]*"\$bundled_node" "\$signer"') 'native builder must use the shared macOS signer'
Require ($buildScript -match '--keychain-preflight[\s\S]*--crypto-self-test[\s\S]*--key-log-self-test') 'builder must run silent preflight and software-only checks'

Write-Output 'Mac auth source conformance: PASS (data-protection keychain + silent entitlement preflight; root=one genesis signature; enrollment=one presence signature; approval=reused LAContext + one signature).'
