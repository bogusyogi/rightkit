$ErrorActionPreference = 'Stop'
$source = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'ownership-approval.swift')

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

Write-Output 'Mac auth source conformance: PASS (root=one genesis signature; enrollment=one presence signature; approval=reused LAContext + one signature).'
