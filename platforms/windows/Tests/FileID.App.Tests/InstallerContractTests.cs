using System.Text.RegularExpressions;
using System.Xml.Linq;
using Xunit;

namespace FileID.App.Tests;

public sealed class InstallerContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void Msi_DoesNotAutoLaunchElevatedApp_AndUsesFileIdSupportUrl()
    {
        var product = XDocument.Load(PathInRepo("platforms", "windows", "installer", "FileID.Msi", "Product.wxs"));
        XNamespace wix = "http://wixtoolset.org/schemas/v4/wxs";

        Assert.DoesNotContain(product.Descendants(wix + "CustomAction"),
            element => string.Equals((string?)element.Attribute("Id"), "LaunchFileID", StringComparison.Ordinal));

        var helpLink = product.Descendants(wix + "Property")
            .Single(element => string.Equals((string?)element.Attribute("Id"), "ARPHELPLINK", StringComparison.Ordinal));
        Assert.Equal("https://github.com/WebWorldWide/FileID/issues", (string?)helpLink.Attribute("Value"));
    }

    [Fact]
    public void BurnBundle_ContainsRuntimeAndMsiForEachArchitecture_AndUnelevatedLaunchTarget()
    {
        var bundle = XDocument.Load(PathInRepo("platforms", "windows", "installer", "FileID.Bundle", "Bundle.wxs"));
        XNamespace wix = "http://wixtoolset.org/schemas/v4/wxs";
        XNamespace bal = "http://wixtoolset.org/schemas/v4/wxs/bal";

        var bootstrapper = bundle.Descendants(bal + "WixStandardBootstrapperApplication").Single();
        Assert.Equal(@"[ProgramFiles64Folder]FileID\FileID.exe", (string?)bootstrapper.Attribute("LaunchTarget"));
        Assert.Equal("[ProgramFiles64Folder]FileID", (string?)bootstrapper.Attribute("LaunchWorkingFolder"));

        var packages = bundle.Descendants()
            .Where(element => element.Name == wix + "ExePackage" || element.Name == wix + "MsiPackage")
            .ToDictionary(element => (string)element.Attribute("Id")!, StringComparer.Ordinal);

        Assert.Equal("NativeMachine = 34404", (string?)packages["WindowsAppRuntimeX64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 43620", (string?)packages["WindowsAppRuntimeArm64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 34404", (string?)packages["FileIDx64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 43620", (string?)packages["FileIDArm64"].Attribute("InstallCondition"));
    }

    [Fact]
    public void ProductVersion_IsConsistentAndReleaseTagIsGuarded()
    {
        var version = File.ReadAllText(PathInRepo("platforms", "windows", "VERSION")).Trim();
        var cargo = File.ReadAllText(PathInRepo("platforms", "windows", "src", "engine", "Cargo.toml"));
        var cargoVersion = Regex.Match(cargo, "(?m)^version\\s*=\\s*\"([^\"]+)\"").Groups[1].Value;
        var releaseWorkflow = File.ReadAllText(PathInRepo(".github", "workflows", "release.yml"));

        Assert.Equal("0.1.1", version);
        Assert.Equal(version, cargoVersion);
        Assert.Contains("Verify tag matches product version", releaseWorkflow, StringComparison.Ordinal);
        Assert.Contains("SHA256SUMS.txt", releaseWorkflow, StringComparison.Ordinal);
        Assert.Contains("actions/upload-artifact@v4", releaseWorkflow, StringComparison.Ordinal);
    }

    [Fact]
    public void Msi_RefusesToPackageWithoutNativeInferenceAndPdfRuntimes()
    {
        var project = File.ReadAllText(PathInRepo("platforms", "windows", "installer", "FileID.Msi", "FileID.Msi.wixproj"));

        foreach (var requiredFile in new[]
                 {
                     "onnxruntime.dll",
                     "onnxruntime_providers_shared.dll",
                     "DirectML.dll",
                     "pdfium.dll",
                 })
        {
            Assert.Contains($"$(PublishRoot)\\{requiredFile}", project, StringComparison.Ordinal);
        }
    }

    [Fact]
    public void WindowsAppRuntimeVersion_MatchesBuildAndBootstrapContract()
    {
        var packages = XDocument.Load(PathInRepo("platforms", "windows", "Directory.Packages.props"));
        var appSdkVersion = packages.Descendants("PackageVersion")
            .Single(element => string.Equals((string?)element.Attribute("Include"), "Microsoft.WindowsAppSDK", StringComparison.Ordinal))
            .Attribute("Version")?.Value;
        var publishScript = File.ReadAllText(PathInRepo("platforms", "windows", "build", "publish-bundle.ps1"));
        var program = File.ReadAllText(PathInRepo("platforms", "windows", "src", "FileID.App", "Program.cs"));

        Assert.Equal("1.7.250606001", appSdkVersion);
        Assert.Contains("$WinAppRuntimeVersion = \"1.7.250606001\"", publishScript, StringComparison.Ordinal);
        Assert.Equal(2, Regex.Matches(publishScript,
            "(?m)^\\$WinAppRuntime(?:X64|Arm64)Sha256 = \"[0-9a-f]{64}\"$").Count);
        Assert.Contains("Assert-MicrosoftSignature", publishScript, StringComparison.Ordinal);
        Assert.Contains("Windows App SDK 1.7 runtime", program, StringComparison.Ordinal);
    }

    private static string PathInRepo(params string[] parts)
        => Path.Combine([RepoRoot, .. parts]);

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(directory.FullName, "platforms", "windows")))
            {
                return directory.FullName;
            }
        }
        throw new DirectoryNotFoundException("Could not find the FileID repository root from the test output directory.");
    }
}
