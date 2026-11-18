param([string]$OutputDirectory)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $root 'target/dist' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$target = 'x86_64-pc-windows-msvc'
$wixVersion = '5.0.2'

function Check-Native([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE." }
}

Push-Location $root
try {
    $cargoOutput = & cargo metadata --locked --no-deps --format-version 1
    Check-Native 'Reading Cargo metadata'
    $metadata = $cargoOutput | ConvertFrom-Json
    $package = $metadata.packages | Where-Object name -eq 'filehelm'
    $version = $package.version
    $pattern = '^(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?:-(?<stage>alpha|beta|rc)\.(?<ordinal>[1-9]\d?))?$'
    if ($version -notmatch $pattern) { throw "Unsupported Windows package version $version." }
    $major, $minor, $patch = [int]$Matches.major, [int]$Matches.minor, [int]$Matches.patch
    $stage = $Matches.stage
    $ordinal = [int]$Matches.ordinal
    $offset = switch ($stage) {
        'alpha' { if ($ordinal -gt 29) { throw 'Alpha number exceeds 29.' }; $ordinal }
        'beta'  { if ($ordinal -gt 30) { throw 'Beta number exceeds 30.' }; 29 + $ordinal }
        'rc'    { if ($ordinal -gt 39) { throw 'RC number exceeds 39.' }; 59 + $ordinal }
        default { 99 }
    }
    $build = $patch * 100 + $offset
    if ($major -gt 255 -or $minor -gt 255 -or $build -gt 65535) {
        throw "Version $version does not fit Windows Installer's version fields."
    }
    $msiVersion = "$major.$minor.$build"

    $changelog = Get-Content -LiteralPath 'CHANGELOG.md' -Raw
    $heading = [regex]::Escape("## [$version]")
    $release = [regex]::Match($changelog, "(?ms)^$heading[^\r\n]*\r?\n(.*?)(?=^## |\z)")
    if (-not $release.Success -or -not $release.Groups[1].Value.Trim()) {
        throw "Add a changelog entry for $version before packaging."
    }

    & cargo build --release --locked --target $target
    Check-Native 'Building the x64 release'
    $targetDirectory = $metadata.target_directory
    $executable = Join-Path $targetDirectory "$target/release/filehelm.exe"
    $exeVersion = (Get-Item -LiteralPath $executable).VersionInfo.ProductVersion
    if ($exeVersion -ne $version) { throw "Expected a Filehelm $version executable, got $exeVersion." }

    $toolDirectory = Join-Path $targetDirectory "release-tools/$wixVersion"
    $wix = Join-Path $toolDirectory 'wix.exe'
    if (-not (Test-Path -LiteralPath $wix -PathType Leaf)) {
        & dotnet tool install wix --version $wixVersion --tool-path $toolDirectory
        Check-Native 'Installing the WiX compiler'
    }
    $previous = $env:DOTNET_ROLL_FORWARD
    $env:DOTNET_ROLL_FORWARD = 'Major'
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    $prefix = "filehelm-$version-windows-x64"
    $msi = Join-Path $OutputDirectory "$prefix.msi"
    $zip = Join-Path $OutputDirectory "$prefix-portable.zip"
    try {
        $wixArguments = @('build', 'installer/filehelm.wxs', '-arch', 'x64', '-o', $msi, '-pdbtype', 'none',
            '-d', "Executable=$executable", '-d', "AppIcon=$(Join-Path $root 'assets/filehelm.ico')",
            '-d', "MsiVersion=$msiVersion", '-d', "ReleaseVersion=$version")
        & $wix @wixArguments
        Check-Native 'Building the per-user MSI'
    } finally {
        $env:DOTNET_ROLL_FORWARD = $previous
    }
    Compress-Archive -LiteralPath @($executable, (Join-Path $root 'README.md'), (Join-Path $root 'CHANGELOG.md')) -DestinationPath $zip -CompressionLevel Optimal -Force
    $checksums = foreach ($artifact in @($msi, $zip)) {
        $hash = Get-FileHash -LiteralPath $artifact -Algorithm SHA256
        '{0}  {1}' -f $hash.Hash.ToLowerInvariant(), [IO.Path]::GetFileName($artifact)
    }
    Set-Content -LiteralPath (Join-Path $OutputDirectory 'SHA256SUMS.txt') -Value $checksums -Encoding ascii
    Set-Content -LiteralPath (Join-Path $OutputDirectory 'release-notes.md') -Value $release.Groups[1].Value.Trim() -Encoding utf8
    Write-Host "Filehelm $version packages are in $OutputDirectory"
} finally {
    Pop-Location
}
