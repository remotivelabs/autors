# verify_cs.ps1 — creates a temporary console project with dotnet and verifies
# that AutorsPInvoke.cs compiles.
# Usage: powershell -File examples/verify_cs.ps1
$ErrorActionPreference = "Stop"

# Locate dotnet: PATH first, then the default install directory
$dotnetExe = $null
$cmd = Get-Command dotnet -ErrorAction SilentlyContinue
if ($cmd) { $dotnetExe = $cmd.Source }
elseif (Test-Path "C:\Program Files\dotnet\dotnet.exe") { $dotnetExe = "C:\Program Files\dotnet\dotnet.exe" }
else { Write-Error "dotnet not found (neither on PATH nor in C:\Program Files\dotnet)" }

# Take the highest SDK version and derive the TFM (>=5 → netX.0; 3.x → netcoreapp3.x)
$sdkLine = (& $dotnetExe --list-sdks | Where-Object { $_ -match '\S' } | Select-Object -Last 1)
$sdkVer = (($sdkLine -split '\s')[0]).Trim()
$parts = $sdkVer.Split('.')
$major = [int]$parts[0]
if ($major -ge 5) { $tfm = "net$major.0" } else { $tfm = "netcoreapp$major.$($parts[1])" }
Write-Host "dotnet SDK $sdkVer -> TFM $tfm"

$tmp = Join-Path $env:TEMP ("autors_cs_" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Copy-Item (Join-Path $PSScriptRoot "AutorsPInvoke.cs") $tmp

    $csproj = @"
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>$tfm</TargetFramework>
    <ImplicitUsings>disable</ImplicitUsings>
    <Nullable>disable</Nullable>
  </PropertyGroup>
</Project>
"@
    Set-Content -Path (Join-Path $tmp "verify.csproj") -Value $csproj -Encoding UTF8

    $program = @"
using System;

internal static class Program
{
    // Compile-only check (building is the acceptance); running requires autors_ffi.dll to be loadable.
    private static int Main()
    {
        Console.WriteLine(Autors.AutorsExample.Demo());
        return 0;
    }
}
"@
    Set-Content -Path (Join-Path $tmp "Program.cs") -Value $program -Encoding UTF8

    $env:DOTNET_CLI_TELEMETRY_OPTOUT = "1"
    $env:DOTNET_NOLOGO = "1"
    & $dotnetExe build $tmp -c Release --nologo
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host "OK: AutorsPInvoke.cs compiles ($tfm)"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
