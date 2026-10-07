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
Require ($source -match 'UserConsentVerifierInterop\.RequestVerificationForWindowAsync') 'must invoke Windows user-verification API'
Require ($source -match 'result == UserConsentVerificationResult\.Verified') 'only Verified may continue'
Require ($source -match 'KeyCredentialManager\.RequestCreateAsync' -and $source -match 'KeyCredentialCreationOption\.FailIfExists') 'new key-log credentials must use Windows Hello and refuse replacement'
Require ($source -match 'CryptographicPublicKeyBlobType\.X509SubjectPublicKeyInfo' -and $source -match 'KeyCredentialManager\.OpenAsync') 'public key export must read Windows Hello SPKI without signing'
Require ($source -match 'credential\.RequestSignAsync\(writer\.DetachBuffer\(\)\)' -and $source -match 'writer\.WriteBytes\(Encoding\.UTF8\.GetBytes\(canonical\)\)') 'key-log signing must pass canonical payload bytes to one Windows Hello signature request'
Require ($source -match 'RSASignaturePadding\.Pkcs1' -and $source -match 'RSASignaturePadding\.Pss') 'RSA signatures must be self-checked with accepted SHA-256 paddings'
Require ($source -match 'CngProvider\.MicrosoftPlatformCryptoProvider') 'must use Microsoft Platform Crypto Provider'
Require ($source -match 'ExportPolicy = CngExportPolicies\.None') 'private-key export must be disabled'
Require ($source -match 'CngUIProtectionLevels\.ForceHighProtection') 'native per-use provider prompt must be required'
Require ($source -match 'KeyUsage = CngKeyUsages\.Signing') 'key must be signing-only'
Require ($source -match 'CngAlgorithm\.ECDsaP256') 'approval key must use P-256 ECDSA'
Require ($source -match 'DSASignatureFormat\.Rfc3279DerSequence') 'signature must use Node-compatible DER encoding'
Require ($source -match 'PayloadDigest\(current\["payload"\]!\)') 'review must show canonical payload digest'
Require ($source -match 'Signed payload fields:.*?JsonSerializer\.Serialize\(current\["payload"\], PrettyJson\)' -or $source -match 'JsonSerializer\.Serialize\(current\["payload"\], PrettyJson\)') 'native user-verification prompt must carry the complete signed payload'
Require ($source -match 'currentCanonical.*CanonicalJson\(_draft\["payload"\]!\)' -or $source -match 'CanonicalJson\(_draft\["payload"\]!\)') 'signing must compare current payload to reviewed payload'
Require ($source -match 'ECDsaCng\(key\).*?SignHash' -or $source -match 'SignHash\(Convert\.FromHexString\(digestText\)') 'signing must use protected CNG key'
Require ($source -match 'RequestVerificationForWindowAsync\(Handle, reason\)' -and $source -match 'if \(!await VerifyCurrentUserAsync\(') 'enrollment and signing require user verification'
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
Require (([regex]::Matches($helloEnroll, 'RequestCreateAsync\(').Count -eq 1) -and $helloEnroll -notmatch 'VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|ForceHighProtection') 'enrollment must use one Windows Hello key-creation call only'
Require (([regex]::Matches($helloRoot, 'RequestCreateAsync\(').Count -eq 1) -and $helloRoot -notmatch 'VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|SignHash') 'root command must create once and emit unsigned genesis without extra auth/key prompts'
Require ($helloRead -match 'KeyCredentialManager\.OpenAsync' -and $helloRead -notmatch 'RequestSignAsync|RequestCreateAsync|VerifyCurrentUserAsync') 'public key lookup must read existing Hello credential without authentication'
Require (([regex]::Matches($helloSign, 'RequestSignAsync\(').Count -eq 1) -and $helloSign -notmatch 'VerifyCurrentUserAsync|OpenApprovalKey|SignHash|ForceHighProtection') 'key operation must use one native Hello signing API path'
Require ($source -notmatch 'Environment\.GetCommandLineArgs|CommandLine\.Arguments|Console\.ReadLine|GetEnvironmentVariable|\b--sign\b|--rotate') 'no arbitrary signing, rotation, or environment key path may exist'
Require ($project -match 'net10\.0-windows10\.0\.22000\.0' -and $project -match '<UseWindowsForms>true</UseWindowsForms>') 'project must target the supported interactive Windows API set'
Require ($project -match '<PublishSingleFile>true</PublishSingleFile>' -and $project -match '<SelfContained>true</SelfContained>') 'package must be a self-contained single-file native-host build'
Require ($manifest -match 'requestedExecutionLevel level="asInvoker"') 'helper must not request elevation'

# Enforce user-consent ordering in the signing handler: explicit confirmation,
# then Windows user verification, then the first private-key signature call.
$signStart = $source.IndexOf('private async System.Threading.Tasks.Task SignAsync()', [StringComparison]::Ordinal)
$signEnd = $source.IndexOf('private void UpdateSignAvailability()', $signStart, [StringComparison]::Ordinal)
Require ($signStart -ge 0 -and $signEnd -gt $signStart) 'signing handler was not found'
$signBody = $source.Substring($signStart, $signEnd - $signStart)
$confirmAt = $signBody.IndexOf('MessageBox.Show(this,', [StringComparison]::Ordinal)
$consentAt = $signBody.IndexOf('VerifyCurrentUserAsync(', [StringComparison]::Ordinal)
$signAt = $signBody.IndexOf('.SignHash(', [StringComparison]::Ordinal)
Require ($confirmAt -ge 0 -and $consentAt -gt $confirmAt -and $signAt -gt $consentAt) 'signing order must be review, explicit confirmation, OS verification, cryptographic signing'

Write-Output 'Windows ownership approval source conformance passed.'
