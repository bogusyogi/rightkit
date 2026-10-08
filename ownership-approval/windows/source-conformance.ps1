$ErrorActionPreference = 'Stop'
& node --test (Join-Path $PSScriptRoot '../conformance/native-source-conformance.test.mjs')
exit $LASTEXITCODE
