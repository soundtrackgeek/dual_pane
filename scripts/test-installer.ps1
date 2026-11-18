param(
    [Parameter(Mandatory)][string]$Msi,
    [Parameter(Mandatory)][string]$ExpectedVersion,
    [string]$PreviousMsi,
    [string]$PortableZip,
    [switch]$AppUpdate
)

$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path -LiteralPath $Msi).Path
if ($PreviousMsi) { $PreviousMsi = (Resolve-Path -LiteralPath $PreviousMsi).Path }
$installation = [IO.Path]::Combine($env:LOCALAPPDATA, 'Programs', 'Filehelm')
$executable = Join-Path $installation 'filehelm.exe'
$menu = [Environment]::GetFolderPath([Environment+SpecialFolder]::Programs)
$shortcut = Join-Path $menu 'Filehelm.lnk'
$registry = 'HKCU:\Software\Filehelm\Installation'
if ((Test-Path -LiteralPath $installation) -or (Test-Path -LiteralPath $shortcut) -or (Test-Path -LiteralPath $registry)) {
    throw 'The installer smoke test requires a user without an existing Filehelm installation.'
}

$logDirectory = Join-Path ([IO.Path]::GetDirectoryName($Msi)) 'installer-logs'
New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null

function Invoke-Installer([string]$Action, [string]$Package, [string]$LogName, [int[]]$Allowed = @(0)) {
    $log = Join-Path $logDirectory "$LogName.log"
    $arguments = "$Action ""$Package"" /qn /norestart REBOOT=ReallySuppress /l*v ""$log"""
    $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $arguments -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -notin $Allowed) {
        throw "Windows Installer returned $($process.ExitCode) for $LogName. See $log."
    }
}

function Test-Installed {
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'The executable was not installed.' }
    if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) { throw 'The Start menu shortcut was not installed.' }
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
    if ($link.TargetPath -ne $executable -or $link.WorkingDirectory.TrimEnd('\') -ne $installation) {
        throw "Unexpected Start menu shortcut target '$($link.TargetPath)' or working directory '$($link.WorkingDirectory)'."
    }
}

function Get-UserDataState {
    $workspace = Get-ItemPropertyValue -LiteralPath 'HKCU:\Software\DualPane' -Name Workspace -ErrorAction SilentlyContinue
    $history = Join-Path $env:LOCALAPPDATA 'DualPane/History/undo-v1.bin'
    [pscustomobject]@{
        Workspace = $workspace
        History = if (Test-Path -LiteralPath $history -PathType Leaf) { (Get-FileHash -LiteralPath $history).Hash }
    } | ConvertTo-Json -Compress
}

function Wait-App([int]$PreviousId = 0) {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $app = Get-Process -Name filehelm -ErrorAction SilentlyContinue |
            Where-Object {
                $_.Id -ne $PreviousId -and [FilehelmSmokeProcess]::Executable($_.Id) -eq $executable
            } |
            Select-Object -First 1
        if ($app) { return $app }
        Start-Sleep -Milliseconds 100
    }
    throw 'The Filehelm process did not reopen.'
}

function Stop-FixtureApp($AppProcess) {
    # These fixture processes run without a workspace. The hosted runner may
    # not expose their native windows to PowerShell's desktop.
    if (-not $AppProcess.HasExited) {
        $AppProcess.Kill()
        if (-not $AppProcess.WaitForExit(10000)) { throw "Fixture $($AppProcess.Id) did not exit." }
    }
}

function Invoke-AppUpdate {
    $stage = [IO.Directory]::CreateTempSubdirectory('filehelm-update-')
    $helper = Join-Path $stage.FullName 'filehelm-updater.exe'
    $package = Join-Path $stage.FullName 'update.msi'
    Copy-Item -LiteralPath $executable -Destination $helper
    Copy-Item -LiteralPath $Msi -Destination $package
    $app = Start-Process -FilePath $executable -ArgumentList '--no-workspace' -PassThru
    $info = [Diagnostics.ProcessStartInfo]::new($helper)
    $digest = (Get-FileHash -LiteralPath $package).Hash.ToLowerInvariant()
    foreach ($argument in @('--apply-filehelm-update', "$($app.Id)", $executable, $digest, '--no-workspace')) {
        $info.ArgumentList.Add($argument)
    }
    $updater = [Diagnostics.Process]::Start($info)
    try {
        Stop-FixtureApp $app
        if (-not $updater.WaitForExit(60000)) { throw 'The update helper did not finish.' }
        $reopened = Wait-App $app.Id
        try {
            for ($attempt = 0; $attempt -lt 50 -and (Test-Path -LiteralPath $stage.FullName); $attempt++) {
                Start-Sleep -Milliseconds 100
            }
            if (Test-Path -LiteralPath $stage.FullName) { throw 'The reopened app did not clear the staging folder.' }
        } finally {
            Stop-FixtureApp $reopened
        }
        Write-Host 'The update helper upgraded and reopened the MSI-installed application.'
    } finally {
        if (-not $updater.HasExited) { $updater.Kill(); $updater.WaitForExit() }
        Stop-FixtureApp $app
    }
}

if ($AppUpdate -and -not $PreviousMsi) { throw 'AppUpdate requires a previous MSI that contains the update helper.' }
if ($AppUpdate -and -not ('FilehelmSmokeProcess' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class FilehelmSmokeProcess {
    [DllImport("kernel32.dll")]
    private static extern IntPtr OpenProcess(uint access, bool inherit, int processId);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref int size);
    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);
    public static string Executable(int id) {
        IntPtr handle = OpenProcess(0x1000, false, id);
        if (handle == IntPtr.Zero) return "";
        try {
            var path = new StringBuilder(32768);
            int size = path.Capacity;
            return QueryFullProcessImageName(handle, 0, path, ref size) ? path.ToString() : "";
        } finally {
            CloseHandle(handle);
        }
    }
}
'@
}
$userData = Get-UserDataState
$cleanup = @()
try {
    if ($PreviousMsi) {
        $cleanup += $PreviousMsi
        Invoke-Installer '/i' $PreviousMsi 'install-previous'
        Test-Installed
    }
    $cleanup = @($Msi) + $cleanup
    if ($AppUpdate) { Invoke-AppUpdate } else { Invoke-Installer '/i' $Msi 'install' }
    Test-Installed
    if ((Get-Item -LiteralPath $executable).VersionInfo.ProductVersion -ne $ExpectedVersion) {
        throw 'The installed executable has the wrong product version.'
    }
    if ((Get-UserDataState) -cne $userData) { throw 'Installation changed workspace or undo data.' }

    if ($PortableZip) {
        $portable = [IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $PortableZip).Path)
        try {
            foreach ($entry in 'filehelm.exe', 'README.md', 'CHANGELOG.md') {
                if ($null -eq $portable.GetEntry($entry)) { throw "$entry is missing from the portable ZIP." }
            }
            $stream = $portable.GetEntry('filehelm.exe').Open()
            try {
                $sha256 = [Security.Cryptography.SHA256]::Create()
                try { $hash = [Convert]::ToHexString($sha256.ComputeHash($stream)) } finally { $sha256.Dispose() }
                if ($hash -ne (Get-FileHash -LiteralPath $executable).Hash) { throw 'The ZIP and MSI contain different binaries.' }
            } finally {
                $stream.Dispose()
            }
        } finally {
            $portable.Dispose()
        }
    }

    Invoke-Installer '/x' $Msi 'uninstall'
    if (Test-Path -LiteralPath $shortcut) { throw 'Uninstall left the Start menu shortcut.' }
    if (Test-Path -LiteralPath $installation) { throw 'Uninstall left the application directory.' }
    if (Test-Path -LiteralPath $registry) { throw 'Uninstall left installation registration.' }
    if ((Get-UserDataState) -cne $userData) { throw 'Uninstall changed workspace or undo data.' }
    if ($PreviousMsi) { Invoke-Installer '/x' $PreviousMsi 'verify-previous-product-removed' @(1605) }
    $cleanup = @()
    Write-Host 'Installer smoke test passed.'
} finally {
    foreach ($package in $cleanup) {
        Invoke-Installer '/x' $package ("cleanup-" + [IO.Path]::GetFileNameWithoutExtension($package)) @(0, 1605)
    }
}
