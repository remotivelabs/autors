//! a temporary console project. Skipped when dotnet is not installed (that
//! acceptance case is covered by examples/verify_cs.ps1).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate dotnet: PATH first, then the default Windows install directory.
fn find_dotnet() -> Option<PathBuf> {
    if Command::new("dotnet").arg("--version").output().is_ok() {
        return Some(PathBuf::from("dotnet"));
    }
    let default = Path::new(r"C:\Program Files\dotnet\dotnet.exe");
    default.exists().then(|| default.to_path_buf())
}

/// Highest installed SDK version (e.g. "5.0.416").
fn latest_sdk(dotnet: &Path) -> Option<String> {
    let out = Command::new(dotnet).arg("--list-sdks").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .next_back()
}

/// Derive the TFM from the SDK version: >= 5 → netX.0; 3.x → netcoreapp3.x.
fn tfm_of(sdk: &str) -> Option<String> {
    let mut it = sdk.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next().unwrap_or("0").parse().ok()?;
    Some(if major >= 5 {
        format!("net{major}.0")
    } else {
        format!("netcoreapp{major}.{minor}")
    })
}

#[test]
fn managed_pinvoke_example_compiles() {
    let Some(dotnet) = find_dotnet() else {
        eprintln!("SKIP: dotnet not found, managed example compile check skipped");
        return;
    };
    let sdk = latest_sdk(&dotnet).expect("dotnet --list-sdks failed");
    let tfm = tfm_of(&sdk).expect("cannot derive TFM");

    let tmp = std::env::temp_dir().join(format!("autors_cs_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let cs_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/AutorsPInvoke.cs");
    std::fs::copy(&cs_src, tmp.join("AutorsPInvoke.cs")).unwrap();

    std::fs::write(
        tmp.join("verify.csproj"),
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    \
             <OutputType>Exe</OutputType>\n    <TargetFramework>{tfm}</TargetFramework>\n    \
             <ImplicitUsings>disable</ImplicitUsings>\n    <Nullable>disable</Nullable>\n  \
             </PropertyGroup>\n</Project>\n"
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.join("Program.cs"),
        "using System;\n\ninternal static class Program\n{\n    \
         // Compile-only check (building is the acceptance); running requires autors_ffi.dll to be loadable.\n    \
         private static int Main()\n    {\n        \
         Console.WriteLine(Autors.AutorsExample.Demo());\n        return 0;\n    }\n}\n",
    )
    .unwrap();

    let status = Command::new(&dotnet)
        .args(["build", "-c", "Release", "--nologo"])
        .current_dir(&tmp)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .status()
        .expect("failed to run dotnet build");
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        status.success(),
        "dotnet build failed for AutorsPInvoke.cs (TFM {tfm}, SDK {sdk})"
    );
}
