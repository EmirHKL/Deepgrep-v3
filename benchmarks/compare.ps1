param(
    [string]$Corpus = (Join-Path $env:USERPROFILE ".cargo\registry\src"),
    [string]$RarePattern = "SkimMatcherV2",
    [string]$CommonPattern = "unsafe",
    [string]$IndexedRegex = "S[a-z]+MatcherV2",
    [int]$Runs = 15
)

$ErrorActionPreference = "Stop"

if (-not (Get-Command rg -ErrorAction SilentlyContinue)) {
    throw "ripgrep (rg) is required"
}
if (-not (Get-Command hyperfine -ErrorAction SilentlyContinue)) {
    throw "hyperfine is required"
}

cargo build --release
$dg = Join-Path $PSScriptRoot "..\target\release\dg.exe"

Write-Host "`nBuilding Deepgrep index..."
& $dg index $Corpus

Write-Host "`nChecking literal result count..."
$dgLiteralLines = (& $dg $RarePattern $Corpus | Measure-Object -Line).Lines
$rgLiteralLines = (& rg $RarePattern $Corpus | Measure-Object -Line).Lines
if ($dgLiteralLines -ne $rgLiteralLines) {
    throw "Literal result mismatch: dg=$dgLiteralLines rg=$rgLiteralLines"
}
Write-Host "Literal result counts match: $dgLiteralLines"

Write-Host "`nChecking indexed regex result count..."
$dgRegexLines = (& $dg $IndexedRegex $Corpus | Measure-Object -Line).Lines
$rgRegexLines = (& rg $IndexedRegex $Corpus | Measure-Object -Line).Lines
if ($dgRegexLines -ne $rgRegexLines) {
    throw "Indexed regex result mismatch: dg=$dgRegexLines rg=$rgRegexLines"
}
Write-Host "Indexed regex result counts match: $dgRegexLines"

Write-Host "`nChecking fallback regex result count..."
$fallbackRegex = "serde|rayon"
$dgFallbackLines = (& $dg $fallbackRegex $Corpus | Measure-Object -Line).Lines
$rgFallbackLines = (& rg $fallbackRegex $Corpus | Measure-Object -Line).Lines
if ($dgFallbackLines -ne $rgFallbackLines) {
    throw "Fallback regex result mismatch: dg=$dgFallbackLines rg=$rgFallbackLines"
}
Write-Host "Fallback regex result counts match: $dgFallbackLines"

$dg = (Resolve-Path $dg).Path
$Corpus = (Resolve-Path $Corpus).Path

Write-Host "`nIndexed rare literal, print all results:"
hyperfine --shell powershell --warmup 3 --runs $Runs `
    "& '$dg' '$RarePattern' '$Corpus'" `
    "& rg '$RarePattern' '$Corpus'"
if ($LASTEXITCODE -ne 0) { throw "Rare literal benchmark failed" }

Write-Host "`nIndexed common literal, print all results:"
hyperfine --shell powershell --warmup 3 --runs $Runs `
    "& '$dg' '$CommonPattern' '$Corpus'" `
    "& rg '$CommonPattern' '$Corpus'"
if ($LASTEXITCODE -ne 0) { throw "Common literal benchmark failed" }

Write-Host "`nIndexed regex with mandatory literal, print all results:"
hyperfine --shell powershell --warmup 3 --runs $Runs `
    "& '$dg' '$IndexedRegex' '$Corpus'" `
    "& rg '$IndexedRegex' '$Corpus'"
if ($LASTEXITCODE -ne 0) { throw "Indexed regex benchmark failed" }

Write-Host "`nRaw literal scan, print all results:"
hyperfine --shell powershell --warmup 3 --runs $Runs `
    "& '$dg' '$RarePattern' '$Corpus' --no-index" `
    "& rg '$RarePattern' '$Corpus'"
if ($LASTEXITCODE -ne 0) { throw "Raw scan benchmark failed" }

Write-Host "`nRaw regex scan, print all results:"
hyperfine --shell powershell --warmup 3 --runs $Runs `
    "& '$dg' '$IndexedRegex' '$Corpus' --no-index" `
    "& rg '$IndexedRegex' '$Corpus'"
if ($LASTEXITCODE -ne 0) { throw "Raw regex benchmark failed" }
