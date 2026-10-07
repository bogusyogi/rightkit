$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$source = Get-Content -Raw -LiteralPath (Join-Path $root 'OwnershipApproval.cs')
$project = Get-Content -Raw -LiteralPath (Join-Path $root 'OwnershipApproval.csproj')
$manifest = Get-Content -Raw -LiteralPath (Join-Path $root 'app.manifest')

function Require([bool] $condition, [string] $message) {
  if (-not $condition) { throw "Windows ownership approval source conformance failed: $message" }
}

Require ($source -match 'private static void Main\(\)') 'entry point must have no argument or command mode'
Require ($source -match 'UserConsentVerifierInterop\.RequestVerificationForWindowAsync') 'must invoke Windows user-verification API'
Require ($source -match 'result == UserConsentVerificationResult\.Verified') 'only Verified may continue'
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
Require ($source -notmatch 'Environment\.GetCommandLineArgs|CommandLine\.Arguments|Console\.ReadLine|GetEnvironmentVariable|\b--sign\b|\b--enroll\b') 'no unattended command or environment signing path may exist'
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
