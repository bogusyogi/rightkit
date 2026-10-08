$ErrorActionPreference = 'Stop'
$source = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'ownership-approval.swift')
$entitlements = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'ownership-approval.entitlements')
$buildScript = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'build.sh')
$impostor = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'keychain-storage-impostor.swift')
$infoPlist = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'Info.plist')
function Require([bool] $ok, [string] $message) { if (-not $ok) { throw "Mac auth source conformance failed: $message" } }
function Section([string] $start, [string] $end) { $from = $source.IndexOf($start, [StringComparison]::Ordinal); $to = $source.IndexOf($end, $from + $start.Length, [StringComparison]::Ordinal); Require ($from -ge 0 -and $to -gt $from) "missing source section $start"; return $source.Substring($from, $to - $from) }
function Count([string] $text, [string] $pattern) { return ([regex]::Matches($text, $pattern)).Count }
$enrollRoot = Section 'private func enrollRoot()' 'private func enroll()'
$enroll = Section 'private func enroll()' 'private func signLoadedDraft()'
$signUi = Section 'private func signLoadedDraft()' 'private func showError('
$signEvent = Section 'private func signKeyLog(' 'private func exportKey('
$signedHelper = Section 'private func signedKeyLog(' 'private func requireEnrollmentPresence('
$presenceHelper = Section 'private func requireEnrollmentPresence(' 'private func jsQuote('
$keychainQuery = Section 'private func keychainQuery(' 'private func keychainPreflight('
$keychainPreflight = Section 'private func keychainPreflight(' 'private func keychainStorageSelfTest('
$storageProbe = Section 'private func keychainStorageSelfTest(' 'private func publicKeyExport('
$persistPublicKey = Section 'private func persistPublicKey(' 'private func loadExistingPublicKey('
Require ((Count $enrollRoot 'authContext\(reason') -eq 1) 'root mode must create one LAContext'
Require ((Count $enrollRoot 'SecureEnclave\.P256\.Signing\.PrivateKey') -eq 1) 'root mode must create one Secure Enclave key'
Require ((Count $enrollRoot 'signedKeyLog\(payload, with: key\)') -eq 1) 'root mode must perform one genesis signature'
Require ((Count $enrollRoot 'requireEnrollmentPresence\(') -eq 0) 'root mode must not add a second proof signature'
Require ((Count $enroll 'authContext\(reason') -eq 1) 'enrollment must create one LAContext'
Require ((Count $enroll 'SecureEnclave\.P256\.Signing\.PrivateKey') -eq 1) 'enrollment must create one Secure Enclave key'
Require ((Count $enroll 'requireEnrollmentPresence\(privateKey\)') -eq 1) 'enrollment must force one presence signature before storage/export'
Require ((Count $signUi 'signKeyLog\(loaded, context: authContext\(reason') -eq 1) 'key-log approval must pass one LAContext into signing'
Require ((Count $signUi 'let context = authContext\(reason') -eq 1) 'legacy approval must create one LAContext'
Require ((Count $signEvent 'SecureEnclave\.P256\.Signing\.PrivateKey\(dataRepresentation: data, authenticationContext: context\)') -eq 1) 'Secure Enclave restore must reuse operation LAContext'
Require ((Count $signEvent 'signedKeyLog\(payload, with: key\)') -eq 1) 'signing path must produce one event signature'
Require ((Count $signedHelper 'key\.signature\(for:') -eq 1) 'event helper must perform exactly one private-key operation'
Require ((Count $presenceHelper 'key\.signature\(for:') -eq 1) 'enrollment presence helper must perform exactly one private-key operation'
Require ($source -notmatch 'evaluatePolicy\s*\(') 'do not add separate LA policy authentication'
Require ($source -match 'SecStaticCodeCheckValidity[\s\S]*kSecCSStrictValidate' -and $source -match 'certificate leaf\[subject\.OU\]' -and $source -match '1\.2\.840\.113635\.100\.6\.1\.13' -and $source -match '6KLGD3LLKF') 'require strict valid Developer ID signature, app ID & expected team'
Require ($source -match 'Bundle\.main\.bundleIdentifier == ownershipBundleIdentifier') 'require stable current bundle ID'
Require ($source -match 'SecTrustedApplicationCreateFromPath\(nil') 'trusted-app ACL must bind current signed helper'
Require ($source -match 'SecAccessCreate[\s\S]*\[trusted\] as CFArray') 'build legacy Keychain ACL from explicit trusted helper'
Require ($source -match 'SecKeychainCopyDefault[\s\S]*SecKeychainGetStatus') 'resolve & require unlocked default user Keychain'
Require ($source -match 'kSecMatchSearchList as String: \[try defaultUserKeychain\(\)\]') 'private key lookup must search only explicit default user Keychain'
Require ($source -match 'kSecUseKeychain as String: try defaultUserKeychain\(\)') 'Keychain adds must target explicit default user Keychain'
Require ($source -notmatch 'kSecUseDataProtectionKeychain|kSecAttrAccessGroup|ownershipKeychainGroup') 'do not use data-protection keychain or access groups'
Require ($source -match 'kSecAttrAccess as String: try signedHelperAccess\(\)') 'storage must use signed-helper legacy ACL'
Require ($source -notmatch 'kSecAttrAccessControl as String: try accessControl\(privateKey: false\)') 'do not use SecAccessControl on storage item'
Require ($source -match 'flags\.insert\(\.privateKeyUsage\)') 'Secure Enclave key must retain privateKeyUsage'
Require ($source -match 'SecAccessControlCreateWithFlags[\s\S]*flags') 'Secure Enclave key must retain userPresence access control'
Require ($keychainQuery -notmatch 'kSecUseAuthenticationContext') 'legacy storage lookup must not bind LAContext or suppress SE presence UI'
Require ($keychainPreflight -match 'requireSignedHelperIdentity\(\)[\s\S]*defaultUserKeychain\(\)') 'preflight must validate identity & Keychain before UI'
Require ($source -match 'SecKeychainSetUserInteractionAllowed\(false\)[\s\S]*SecKeychainSetUserInteractionAllowed\(previous\.boolValue\)') 'scope Keychain interaction suppression & restore previous state'
Require ($storageProbe -match 'keychainPreflight\(printPass: false\)' -and $storageProbe -match 'SecItemAdd' -and $storageProbe -match 'SecItemCopyMatching' -and $storageProbe -match 'runStorageProbe\(impostorPath' -and $storageProbe -match 'SecItemDelete' -and $storageProbe -match 'errSecItemNotFound') 'storage probe must create/read/deny/delete & verify absence of only a sentinel'
Require ($storageProbe -match 'signedHelperAccess\(\)' -and $storageProbe -match 'defaultUserKeychain\(\)') 'storage probe must reuse signed ACL & default Keychain'
Require ($storageProbe -match 'successorPath' -and $storageProbe -match 'READ_ALLOWED') 'storage probe must support signed-helper update continuity'
Require ($storageProbe -match 'errSecInteractionNotAllowed' -and $storageProbe -match 'errSecAuthFailed') 'storage probe must accept only known silent ACL denial statuses'
Require ($source -match 'private func keychainStorageReadProbe' -and $source -match 'UUID sentinel coordinates' -and $source -match 'SecKeychainOpen' -and $source -match 'READ_ALLOWED') 'updated-helper read mode must accept UUID sentinel only & never print data'
Require ($source -match 'finished\.wait\(timeout: \.now\(\) \+ 10\)') 'child probes must have a bounded timeout'
Require ($impostor -match 'SecKeychainSetUserInteractionAllowed\(false\)' -and $impostor -match 'SecKeychainSetUserInteractionAllowed\(previous\.boolValue\)') 'impostor probe must scope silent mode'
Require ($impostor -match 'SecItemCopyMatching') 'impostor probe must perform read-only sentinel lookup'
Require ($impostor -match 'SecKeychainGetStatus' -and $impostor -match 'kSecUnlockStateStatus' -and $impostor -match 'KEYCHAIN_UNLOCKED' -and $impostor -match 'SecItemCopyMatching') 'impostor must prove explicit keychain is open & unlocked before sentinel query'
Require ($entitlements -match '<dict\s*/>') 'entitlements plist must be empty'
Require ($entitlements -notmatch 'keychain-access-groups|application-identifier|application-groups|team-identifier') 'do not require restricted entitlements'
Require ($infoPlist -match '<key>CFBundleIdentifier</key>\s*<string>com\.orthiclabs\.rightkit\.ownership-approval</string>') 'app bundle ID must remain stable'
Require ($buildScript -match '\[\[ \$# -ne 2 \]\]') 'builder must accept exactly Node & app path'
Require ($buildScript -match 'swiftc -parse-as-library -O') 'native builder must compile on Mac host'
Require ($buildScript -match 'keychain-storage-impostor\.swift') 'builder must include unrelated executable ACL probe'
Require ($buildScript -match 'signer="\$release_tools/sign-macos\.mjs"[\s\S]*"\$bundled_node" "\$signer"') 'native builder must use shared macOS signer'
Require ($buildScript -match '--keychain-preflight[\s\S]*--keychain-storage-self-test[\s\S]*--crypto-self-test[\s\S]*--key-log-self-test') 'builder must require signed preflight, ACL probe & software checks'
Require ($buildScript -notmatch 'provisionprofile|security cms') 'builder must not require or embed provisioning profile'
Require ($buildScript -match 'Keychain preflight: PASS') 'builder must require affirmative preflight output'
$draftBody = Section 'private func validateDraft(' 'private func accessControl('
$payloadKeys = @([regex]::Matches((Section 'private let payloadKeys' 'private enum ApprovalError'), '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
function NativePattern([string] $field) {
  $match = [regex]::Match($draftBody, $field + '\.range\(of: "([^"]+)"')
  Require $match.Success "missing native pattern for $field"
  return $match.Groups[1].Value.Replace('\\', '\')
}
$fingerprintPattern = NativePattern 'fingerprint'
$hexPattern = NativePattern 'policyDigest'
$floorPattern = NativePattern 'value'
$requestPattern = NativePattern 'request'
foreach ($guard in @('schema.doubleValue == 2', 'CFGetTypeID(schema) != CFBooleanGetTypeID()', 'outer["signature"] is NSNull', 'Set(payload.keys) == Set(payloadKeys)',
  'requireString(payload, "reason", max: 4000)', 'requireString(payload, "owner", max: 256)', 'requireString(payload, "repositoryIdentity", max: 512)',
  'issuedDate <= Date(), date > issuedDate, date.timeIntervalSince(issuedDate) <= 90 * 86400', 'ISO8601DateFormatter().string(from: issuedDate).hasPrefix(issued)', 'ISO8601DateFormatter().string(from: date).hasPrefix(expiry)',
  '(1...256).contains(occurrences.count)', 'Set(occurrence.keys) == Set(["path", "kind", "hash"])', 'path.split', 'path.contains',
  '["source", "dependency"].contains(kind)', 'hash.range(of: "^[a-f0-9]{64}$"', 'pairs.insert([path, hash]).inserted',
  'fingerprint.range(of: "^[a-f0-9]{64}$", options: .regularExpression) == nil', 'path.range(of: "^[A-Za-z]:", options: .regularExpression) == nil',
  'URLComponents(string: url)', 'components.scheme == "https"', 'components.user == nil, components.password == nil', 'CFGetTypeID(verified) == CFBooleanGetTypeID(), verified.boolValue')) {
  Require ($draftBody.Contains($guard)) "missing native v2 refusal guard: $guard"
}
Require ((Count $draftBody '== \w+\.startIndex\.\.<\w+\.endIndex') -eq 7) 'native regex matching must consume whole token'
Require ($signUi -match '"schema": 2') 'exception signing must emit schema 2'

# Static fallback: extract native keys/regexes, pin refusal guards, then project
# those shape rules over every shared vector. This does not execute native code.
function ExactKeys($object, [string[]] $keys) {
  if ($object -isnot [pscustomobject]) { return $false }
  $names = @($object.PSObject.Properties.Name)
  return $names.Count -eq $keys.Count -and @($keys | Where-Object { $names -cnotcontains $_ }).Count -eq 0
}
function TextField($object, [string] $key, [int] $limit = 4096) {
  $value = $object.$key
  return $value -is [string] -and -not [string]::IsNullOrWhiteSpace($value) -and $value.Length -le $limit
}
function FullMatch($value, [string] $pattern) {
  if ($value -isnot [string]) { return $false }
  $match = [regex]::Match($value, $pattern)
  return $match.Success -and $match.Length -eq $value.Length
}
function TestDraft($record) {
  if (-not (ExactKeys $record @('schema', 'payload', 'signature')) -or $record.schema -is [bool] -or $record.schema -ne 2 -or $null -ne $record.signature) { return $false }
  $p = $record.payload
  if (-not (ExactKeys $p $payloadKeys) -or -not (TextField $p 'repositoryIdentity' 512) -or -not (TextField $p 'reason' 4000) -or -not (TextField $p 'owner' 256)) { return $false }
  if (-not (FullMatch $p.fingerprint $fingerprintPattern) -or (FullMatch $p.fingerprint $hexPattern) -or -not (FullMatch $p.policyDigest $hexPattern) -or -not (FullMatch $p.requestId $requestPattern)) { return $false }
  foreach ($floor in @('minimumScannerVersion', 'minimumCatalogVersion')) {
    if (-not (TextField $p $floor ([int]::MaxValue)) -or -not (FullMatch $p.$floor $floorPattern)) { return $false }
  }
  $issued = [datetime]::MinValue; $expires = [datetime]::MinValue
  if (-not (FullMatch $p.issued '^[0-9]{4}-[0-9]{2}-[0-9]{2}$') -or -not (FullMatch $p.expires '^[0-9]{4}-[0-9]{2}-[0-9]{2}$')) { return $false }
  if (-not [datetime]::TryParseExact($p.issued, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::None, [ref] $issued) -or
      -not [datetime]::TryParseExact($p.expires, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::None, [ref] $expires)) { return $false }
  if ($issued -gt [datetime]::UtcNow.Date -or $expires -le $issued -or ($expires - $issued).TotalDays -gt 90) { return $false }
  if ($p.occurrences -isnot [array] -or $p.occurrences.Count -lt 1 -or $p.occurrences.Count -gt 256) { return $false }
  $pairs = @()
  foreach ($o in $p.occurrences) {
    if (-not (ExactKeys $o @('path', 'kind', 'hash')) -or $o.path -isnot [string] -or [string]::IsNullOrEmpty($o.path) -or @('source', 'dependency') -cnotcontains $o.kind -or -not (FullMatch $o.hash $hexPattern)) { return $false }
    if ($o.path.StartsWith('/') -or $o.path.Contains('\') -or $o.path -match '^[A-Za-z]:|[*?\[\]{}]' -or @($o.path.Split('/') | Where-Object { $_ -ceq '' -or $_ -ceq '.' -or $_ -ceq '..' }).Count -gt 0) { return $false }
    $pair = ConvertTo-Json -InputObject @($o.path, $o.hash) -Compress
    if ($pairs -ccontains $pair) { return $false }; $pairs += $pair
  }
  $t = $p.trackingRef; $uri = $null
  if (-not (ExactKeys $t @('id', 'url', 'verified')) -or $t.id -isnot [string] -or $t.id.Length -eq 0 -or $t.verified -isnot [bool] -or -not $t.verified -or $t.url -isnot [string]) { return $false }
  return [uri]::TryCreate($t.url, [UriKind]::Absolute, [ref] $uri) -and $uri.Scheme -ceq 'https' -and $uri.Host.Length -gt 0 -and $uri.UserInfo.Length -eq 0 -and $t.url -notmatch '^https://[^/?#]*@'
}
$expectedKeys = @('repositoryIdentity', 'fingerprint', 'policyDigest', 'minimumScannerVersion', 'minimumCatalogVersion', 'occurrences', 'reason', 'owner', 'issued', 'expires', 'requestId', 'trackingRef')
Require ($payloadKeys.Count -eq $expectedKeys.Count -and @($expectedKeys | Where-Object { $payloadKeys -cnotcontains $_ }).Count -eq 0) 'native payload keys must match v2 exactly'
Require ($fingerprintPattern -ceq '^[a-z0-9][a-z0-9._-]{1,127}$' -and $hexPattern -ceq '^[a-f0-9]{64}$' -and $floorPattern -ceq '^[0-9]+(?:\.[0-9]+)*$' -and $requestPattern -ceq '^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$') 'native token patterns must match v2'
$vectors = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot '../conformance/v2-vectors.json') | ConvertFrom-Json
Require ($vectors.schema -eq 2 -and $vectors.valid -is [array] -and $vectors.invalid -is [array] -and $vectors.valid.Count -gt 0 -and $vectors.invalid.Count -gt 0) 'shared vectors must contain valid & invalid v2 records'
foreach ($vector in $vectors.valid) { Require (TestDraft $vector.record) "valid v2 vector refused: $($vector.name)" }
foreach ($vector in $vectors.invalid) { Require (-not (TestDraft $vector.record)) "invalid v2 vector accepted: $($vector.name)" }
Write-Output "V2 native source-projected vectors: PASS ($($vectors.valid.Count) valid accepted; $($vectors.invalid.Count) invalid refused)."

Write-Output 'Mac auth source conformance: PASS (signed-helper default-user ACL; empty entitlements; scoped UUID storage probe; root=one signature; enrollment=one presence signature; approval=reused LAContext + one signature).'
