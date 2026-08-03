param(
  [Parameter(Mandatory = $true, Position = 0)]
  [string]$FilePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$thumbprint = $env:WINDOWS_CERTIFICATE_THUMBPRINT
if ([string]::IsNullOrWhiteSpace($thumbprint)) {
  throw 'WINDOWS_CERTIFICATE_THUMBPRINT is required.'
}
$thumbprint = ($thumbprint -replace '\s', '').ToUpperInvariant()
if ($thumbprint -notmatch '^[0-9A-F]{40}$') {
  throw 'WINDOWS_CERTIFICATE_THUMBPRINT must be a SHA-1 certificate thumbprint.'
}

$timestampUrl = $env:WINDOWS_TIMESTAMP_URL
if ([string]::IsNullOrWhiteSpace($timestampUrl)) {
  throw 'WINDOWS_TIMESTAMP_URL is required.'
}
try {
  $timestampUri = [Uri]$timestampUrl
} catch {
  throw 'WINDOWS_TIMESTAMP_URL must be a valid absolute HTTPS URL.'
}
if (-not $timestampUri.IsAbsoluteUri -or $timestampUri.Scheme -ne [Uri]::UriSchemeHttps) {
  throw 'WINDOWS_TIMESTAMP_URL must be a valid absolute HTTPS URL.'
}

if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
  throw 'The Tauri signing target does not exist or is not a file.'
}
$resolvedPath = (Resolve-Path -LiteralPath $FilePath).Path

$certificatePath = "Cert:\CurrentUser\My\$thumbprint"
$certificate = Get-Item -LiteralPath $certificatePath -ErrorAction Stop
if (-not $certificate.HasPrivateKey) {
  throw 'The selected Windows signing certificate has no private key.'
}
if ($certificate.NotBefore -gt [DateTime]::Now -or $certificate.NotAfter -le [DateTime]::Now) {
  throw 'The selected Windows signing certificate is not currently valid.'
}
if (-not ($certificate.EnhancedKeyUsageList.ObjectId.Value -contains '1.3.6.1.5.5.7.3.3')) {
  throw 'The selected certificate is not valid for code signing.'
}

$signToolCommand = Get-Command 'signtool.exe' -ErrorAction SilentlyContinue
if ($null -ne $signToolCommand) {
  $signToolPath = $signToolCommand.Source
} else {
  $signToolPath = $null
  $programFilesX86 = ${env:ProgramFiles(x86)}
  if (-not [string]::IsNullOrWhiteSpace($programFilesX86)) {
    $windowsKits = Join-Path $programFilesX86 'Windows Kits\10\bin'
    $sdkCandidates = @(Get-ChildItem -LiteralPath $windowsKits -Filter 'signtool.exe' -File -Recurse -ErrorAction SilentlyContinue |
      Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
      ForEach-Object {
        try {
          $sdkVersion = [Version]$_.Directory.Parent.Name
        } catch {
          $sdkVersion = $null
        }
        if ($null -ne $sdkVersion) {
          [PSCustomObject]@{
            Path = $_.FullName
            Version = $sdkVersion
          }
        }
      })
    $signToolPath = $sdkCandidates |
      Sort-Object Version -Descending |
      Select-Object -First 1 -ExpandProperty Path
  }
}
if ([string]::IsNullOrWhiteSpace($signToolPath)) {
  throw 'signtool.exe was not found in PATH or the Windows 10 SDK.'
}

$signOutput = @(& $signToolPath sign /sha1 $thumbprint /s My /fd SHA256 /tr $timestampUri.AbsoluteUri /td SHA256 $resolvedPath 2>&1)
$signExitCode = $LASTEXITCODE
if ($signExitCode -ne 0) {
  $safeLines = @($signOutput |
    ForEach-Object { [string]$_ } |
    Where-Object { $_ -match 'SignTool Error:|Error information:|Number of errors:' })
  $safeMessage = ($safeLines -join [Environment]::NewLine)
  $safeMessage = $safeMessage -replace [Regex]::Escape($resolvedPath), '<artifact>'
  $safeMessage = $safeMessage -replace [Regex]::Escape($thumbprint), '<thumbprint>'
  if ([string]::IsNullOrWhiteSpace($safeMessage)) {
    $safeMessage = 'No non-sensitive SignTool diagnostic was available.'
  }
  if ($safeMessage.Length -gt 1500) {
    $safeMessage = $safeMessage.Substring(0, 1500) + '...'
  }
  throw "signtool.exe failed with exit code $signExitCode. $safeMessage"
}

$signature = Get-AuthenticodeSignature -LiteralPath $resolvedPath
$actualThumbprint = if ($null -eq $signature.SignerCertificate) { '' } else { $signature.SignerCertificate.Thumbprint }
if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
  throw "Authenticode verification failed with status $($signature.Status)."
}
if ($actualThumbprint -ne $thumbprint) {
  throw 'The signed artifact does not use the requested certificate.'
}
if ($null -eq $signature.TimeStamperCertificate) {
  throw 'The signed artifact does not have a trusted RFC 3161 timestamp.'
}
