$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$source = Get-Content -Raw -LiteralPath (Join-Path $root 'OwnershipApproval.cs')
$project = Get-Content -Raw -LiteralPath (Join-Path $root 'OwnershipApproval.csproj')
$manifest = Get-Content -Raw -LiteralPath (Join-Path $root 'app.manifest')

function Require([bool] $condition, [string] $message) {
  if (-not $condition) { throw "Windows ownership approval source conformance failed: $message" }
}

Require ($source -match 'private static int Main\(string\[\] args\)') 'entry point must parse native command modes'
Require ($source -match '"--enroll"' -and $source -match '"--public-key"' -and $source -match '"--key-operation"' -and $source -match '"--enroll-root"') 'public export, enrollment, key-operation, and root command modes are required'
Require ($source -match 'Path\.IsPathFullyQualified\(output\)' -and $source -match 'Path\.IsPathFullyQualified\(input\)') 'commanded paths must be absolute'
Require ($source -match 'private async System\.Threading\.Tasks\.Task StageKeyOperationAsync' -and $source -match 'Sign reviewed key-log event') 'key-operation must stage typed event for explicit main-window signing action'
Require ($source -match 'private async System\.Threading\.Tasks\.Task ExportPublicKeyToPathAsync') 'public key discovery command must export an existing key without enrollment'
Require ($source -match 'private async System\.Threading\.Tasks\.Task EnrollRootAsync') 'root command must create genesis event in native helper'
Require ($source -match 'ValidateKeyLogPayload' -and $source -match 'rightkit\.ownership-key-log\.v1') 'key-log payload schema/domain must be validated'
Require ($source -match 'signature"] = null' -and $source -match 'KeyCredentialCreationOption\.FailIfExists') 'root event must be unsigned and credential creation must refuse replacement'
Require ($source -match 'signature:null' -or $source -match 'draftObj\["signature"\] is not null') 'key-operation must reject pre-signed drafts'
Require ($source -match 'AtomicWriteExclusive' -and $source -match 'File\.Move\(temp, path\)') 'command outputs must be atomically created without overwrite'
Require ($source -match 'private static string CanonicalJson\(JsonNode\? node\)' -and $source -match 'if \(node is null\) return "null";') 'canonical JSON must encode JSON null values'
Require ($source -notmatch 'Confirm ownership key-log event|Continue to the native key signature') 'key-log command must use main-window review/sign action without modal confirmation'
Require ($source -match 'Environment\.ExitCode = 0' -and $source -match 'Environment\.ExitCode = 1') 'commanded enrollment must report success/failure via exit code'
Require ($source -match 'KeyCredentialManager\.RequestCreateAsync' -and $source -match 'KeyCredentialCreationOption\.FailIfExists') 'new key-log credentials must use Windows Hello and refuse replacement'
Require ($source -match 'CryptographicPublicKeyBlobType\.X509SubjectPublicKeyInfo' -and $source -match 'KeyCredentialManager\.OpenAsync') 'public key export must read Windows Hello SPKI without signing'
Require ($source -match 'credential\.RequestSignAsync\(writer\.DetachBuffer\(\)\)' -and $source -match 'writer\.WriteBytes\(Encoding\.UTF8\.GetBytes\(canonical\)\)') 'key-log signing must pass canonical payload bytes to one Windows Hello signature request'
Require ($source -match 'RSASignaturePadding\.Pkcs1' -and $source -match 'RSASignaturePadding\.Pss') 'RSA signatures must be self-checked with accepted SHA-256 paddings'
Require ($source -notmatch 'ECDsaCng|CngKey|OpenApprovalKey|CreateApprovalKey|VerifyCurrentUserAsync|ForceHighProtection') 'legacy approval key & separate authentication paths must not be selectable'
Require ($source -match 'rsa\.KeySize != 2048' -and $source -match 'SHA256\.HashData\(rsa\.ExportSubjectPublicKeyInfo\(\)\)') 'Hello key must be RSA-2048 with SPKI fingerprint'
Require ($source -match 'PayloadDigest\(current\["payload"\]!\)') 'review must show canonical payload digest'
Require ($source -match 'currentCanonical.*CanonicalJson\(_draft\["payload"\]!\)' -or $source -match 'CanonicalJson\(_draft\["payload"\]!\)') 'signing must compare current payload to reviewed payload'
function MethodBody([string] $startMarker, [string] $endMarker) {
  $start = $source.IndexOf($startMarker, [StringComparison]::Ordinal)
  $end = $source.IndexOf($endMarker, $start, [StringComparison]::Ordinal)
  Require ($start -ge 0 -and $end -gt $start) "method body missing: $startMarker"
  return $source.Substring($start, $end - $start)
}
$helloEnroll = MethodBody 'private async System.Threading.Tasks.Task EnrollKeyCredentialAsync' 'private async System.Threading.Tasks.Task ExportPublicKeyToPathAsync'
$helloRoot = MethodBody 'private async System.Threading.Tasks.Task EnrollRootAsync' 'private async System.Threading.Tasks.Task EnrollKeyCredentialAsync'
$helloRead = MethodBody 'private async System.Threading.Tasks.Task ExportPublicKeyToPathAsync' 'private static KeyCredential RequireCredential'
$helloSign = MethodBody 'private async System.Threading.Tasks.Task SignKeyLogCommandAsync' 'private static void ValidateKeyLogPayload'
$helloStage = MethodBody 'private async System.Threading.Tasks.Task StageKeyOperationAsync' 'private void StageKeyLog'
Require ($helloStage -match 'KeyCredentialManager\.OpenAsync\(KeyLogCredentialName\)' -and $helloStage -match 'RequireCredential\(opened,' -and $helloStage -match 'PublicKeyFingerprint\(PublicKeyPem\(credential\)\)') 'staging must bind signer to same Hello RSA key used by signing'
Require (([regex]::Matches($helloEnroll, 'RequestCreateAsync\(').Count -eq 1) -and $helloEnroll -notmatch 'VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|ForceHighProtection') 'enrollment must use one Windows Hello key-creation call only'
Require (([regex]::Matches($helloRoot, 'RequestCreateAsync\(').Count -eq 1) -and $helloRoot -notmatch 'VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|SignHash') 'root command must create once and emit unsigned genesis without extra auth/key prompts'
Require ($helloRead -match 'KeyCredentialManager\.OpenAsync' -and $helloRead -notmatch 'RequestSignAsync|RequestCreateAsync|VerifyCurrentUserAsync') 'public key lookup must read existing Hello credential without authentication'
Require (([regex]::Matches($helloSign, 'RequestSignAsync\(').Count -eq 1) -and $helloSign -notmatch 'VerifyCurrentUserAsync|OpenApprovalKey|SignHash|ForceHighProtection') 'key operation must use one native Hello signing API path'
Require ($source -notmatch 'Environment\.GetCommandLineArgs|CommandLine\.Arguments|Console\.ReadLine|GetEnvironmentVariable|\b--sign\b|--rotate') 'no arbitrary signing, rotation, or environment key path may exist'
Require ($project -match 'net10\.0-windows10\.0\.22000\.0' -and $project -match '<UseWindowsForms>true</UseWindowsForms>') 'project must target the supported interactive Windows API set'
Require ($project -match '<PublishSingleFile>true</PublishSingleFile>' -and $project -match '<SelfContained>true</SelfContained>') 'package must be a self-contained single-file native-host build'
Require ($manifest -match 'requestedExecutionLevel level="asInvoker"') 'helper must not request elevation'

# Explicit review confirmation must precede one Hello private-key operation.
$signStart = $source.IndexOf('private async System.Threading.Tasks.Task SignAsync()', [StringComparison]::Ordinal)
$signEnd = $source.IndexOf('private void UpdateSignAvailability()', $signStart, [StringComparison]::Ordinal)
Require ($signStart -ge 0 -and $signEnd -gt $signStart) 'signing handler was not found'
$signBody = $source.Substring($signStart, $signEnd - $signStart)
$confirmAt = $signBody.IndexOf('MessageBox.Show(this,', [StringComparison]::Ordinal)
$signAt = $signBody.IndexOf('.RequestSignAsync(', [StringComparison]::Ordinal)
Require ($confirmAt -ge 0 -and $signAt -gt $confirmAt -and ([regex]::Matches($signBody, 'RequestSignAsync\(').Count -eq 1)) 'signing order must be review, explicit confirmation, one Windows Hello signature'
Require ($signBody -match 'RequireCredential\(hello,' -and $signBody -match 'PublicKeyFingerprint\(pem\)') 'exception signing must refuse absent Hello key & use its RSA fingerprint'

$draftBody = MethodBody 'private static void ValidateDraft(' 'private static void RequirePattern('
$payloadKeys = @([regex]::Matches([regex]::Match($draftBody, 'var payloadKeys = new\[\] \{([^}]+)\}').Groups[1].Value, '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
function NativePattern([string] $field) {
  $match = [regex]::Match($draftBody, 'RequirePattern\(payload, ' + $field + ', "([^"]+)"')
  Require $match.Success "missing native pattern for $field"
  return $match.Groups[1].Value.Replace('\\', '\')
}
$fingerprintPattern = NativePattern '"fingerprint"'
$hexPattern = NativePattern '"policyDigest"'
$floorPattern = NativePattern 'floor'
$requestPattern = NativePattern '"requestId"'
foreach ($guard in @('root["schema"]?.GetValue<int>() != 2', 'root["signature"] is not null', 'SetEquals(payloadKeys)',
  'RequireText(payload, "reason", 4000)', 'RequireText(payload, "owner", 256)', 'RequireText(payload, "repositoryIdentity", 512)',
  'issued > DateTime.UtcNow.Date || expiry <= issued || (expiry - issued).TotalDays > 90', 'DateTime.TryParseExact(payload["issued"]', 'DateTime.TryParseExact(payload["expires"]',
  'occurrences.Count is < 1 or > 256', 'SetEquals(new[] { "path", "kind", "hash" })', 'path.Split', 'path.Contains',
  'is not ("source" or "dependency")', 'RequirePattern(occurrence, "hash", "^[a-f0-9]{64}$", 64)', '!pairs.Add((path, occurrence["hash"]!.GetValue<string>()))',
  'Regex.IsMatch(payload["fingerprint"]!.GetValue<string>(), "^[a-f0-9]{64}$"', '"^[A-Za-z]:|', '"^https://[^/?#]*@"',
  '!Uri.TryCreate(url, UriKind.Absolute', 'uri.Scheme != Uri.UriSchemeHttps', '!string.IsNullOrEmpty(uri.UserInfo)', 'tracking["verified"]?.GetValue<bool>() != true')) {
  Require ($draftBody.Contains($guard)) "missing native v2 refusal guard: $guard"
}
Require ($source.Contains('!match.Success || match.Length != text.Length')) 'native regex matching must consume whole token'

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

Write-Output 'Windows ownership approval source conformance passed.'
