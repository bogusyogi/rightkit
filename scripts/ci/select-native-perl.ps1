# Shared by .github/actions/rk-setup and the publish.yml verify job (Windows, crates runs).
# Moved verbatim from rk-setup so qualification and shared CI select the same native toolchain.
# Git's MSYS Perl is incomplete for OpenSSL Configure. Use the hosted
# image's native Strawberry Perl explicitly, including from bash jobs.
$perl = 'C:\Strawberry\perl\bin\perl.exe'
if (-not (Test-Path -LiteralPath $perl)) {
  throw "Hosted native Strawberry Perl is missing: $perl"
}
& $perl -MLocale::Maketext::Simple -MIPC::Cmd -e 'print "$^X\n"'
if ($LASTEXITCODE -ne 0) { throw 'Native Perl lacks OpenSSL Configure prerequisites' }
"PERL=$perl" >> $env:GITHUB_ENV
(Split-Path -Parent $perl) >> $env:GITHUB_PATH
