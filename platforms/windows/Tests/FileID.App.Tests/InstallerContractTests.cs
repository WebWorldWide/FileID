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

        var bundleElement = bundle.Descendants(wix + "Bundle").Single();
        Assert.Equal(@"$(var.AssetsRoot)\FileID.ico", (string?)bundleElement.Attribute("IconSourceFile"));

        var bootstrapper = bundle.Descendants(bal + "WixStandardBootstrapperApplication").Single();
        Assert.Equal(@"[ProgramFiles64Folder]FileID\FileID.exe", (string?)bootstrapper.Attribute("LaunchTarget"));
        Assert.Equal("[ProgramFiles64Folder]FileID", (string?)bootstrapper.Attribute("LaunchWorkingFolder"));
        Assert.Equal("hyperlinkSidebarLicense", (string?)bootstrapper.Attribute("Theme"));
        Assert.Equal(@"theme\sidebar.png", (string?)bootstrapper.Attribute("LogoSideFile"));
        Assert.Equal(@"theme\FileIDTheme.wxl", (string?)bootstrapper.Attribute("LocalizationFile"));
        Assert.Equal("license.rtf", (string?)bootstrapper.Attribute("LicenseUrl"));
        var licensePayload = bundle.Descendants(wix + "Payload")
            .Single(element => string.Equals((string?)element.Attribute("Name"), "license.rtf", StringComparison.Ordinal));
        Assert.Equal(@"theme\license.rtf", (string?)licensePayload.Attribute("SourceFile"));
        Assert.DoesNotContain("http", (string?)bootstrapper.Attribute("LicenseUrl") ?? string.Empty, StringComparison.OrdinalIgnoreCase);

        var packages = bundle.Descendants()
            .Where(element => element.Name == wix + "ExePackage" || element.Name == wix + "MsiPackage")
            .ToDictionary(element => (string)element.Attribute("Id")!, StringComparer.Ordinal);

        Assert.Equal("NativeMachine = 34404", (string?)packages["WindowsAppRuntimeX64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 43620", (string?)packages["WindowsAppRuntimeArm64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 34404", (string?)packages["FileIDx64"].Attribute("InstallCondition"));
        Assert.Equal("NativeMachine = 43620", (string?)packages["FileIDArm64"].Attribute("InstallCondition"));
    }

    [Fact]
    public void MsiAndBundle_HaveBrandedAccessibleInstallerAssets()
    {
        var product = XDocument.Load(PathInRepo("platforms", "windows", "installer", "FileID.Msi", "Product.wxs"));
        XNamespace wix = "http://wixtoolset.org/schemas/v4/wxs";
        XNamespace ui = "http://wixtoolset.org/schemas/v4/wxs/ui";

        Assert.Equal("WixUI_Minimal", (string?)product.Descendants(ui + "WixUI").Single().Attribute("Id"));
        var variables = product.Descendants(wix + "WixVariable")
            .ToDictionary(element => (string)element.Attribute("Id")!, StringComparer.Ordinal);
        Assert.EndsWith(@"\license.rtf", (string?)variables["WixUILicenseRtf"].Attribute("Value"), StringComparison.Ordinal);
        Assert.EndsWith(@"\banner.bmp", (string?)variables["WixUIBannerBmp"].Attribute("Value"), StringComparison.Ordinal);
        Assert.EndsWith(@"\dialog.bmp", (string?)variables["WixUIDialogBmp"].Attribute("Value"), StringComparison.Ordinal);

        Assert.Equal((165, 400), ReadPngDimensions(PathInRepo(
            "platforms", "windows", "installer", "FileID.Bundle", "theme", "sidebar.png")));
        Assert.Equal((493, 58), ReadBmpDimensions(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "theme", "banner.bmp")));
        Assert.Equal((493, 312), ReadBmpDimensions(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "theme", "dialog.bmp")));

        var localization = File.ReadAllText(PathInRepo(
            "platforms", "windows", "installer", "FileID.Bundle", "theme", "FileIDTheme.wxl"));
        var license = File.ReadAllText(PathInRepo(
            "platforms", "windows", "installer", "FileID.Bundle", "theme", "license.rtf"));
        Assert.Contains("No cloud. No telemetry. No account required.", localization, StringComparison.Ordinal);
        Assert.Contains("Apache License", license, StringComparison.Ordinal);
        Assert.Contains("Version 2.0, January 2004", license, StringComparison.Ordinal);
    }

    [Fact]
    public void ProductVersion_IsConsistentAndReleaseTagIsGuarded()
    {
        var version = File.ReadAllText(PathInRepo("platforms", "windows", "VERSION")).Trim();
        var cargo = File.ReadAllText(PathInRepo("platforms", "windows", "src", "engine", "Cargo.toml"));
        var cargoVersion = Regex.Match(cargo, "(?m)^version\\s*=\\s*\"([^\"]+)\"").Groups[1].Value;
        var releaseWorkflow = File.ReadAllText(PathInRepo(".github", "workflows", "release.yml"));
        var msiProject = File.ReadAllText(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "FileID.Msi.wixproj"));
        var versionVerifier = File.ReadAllText(PathInRepo(
            "platforms", "windows", "build", "verify-version.ps1"));

        Assert.Equal("0.1.4", version);
        Assert.Equal(version, cargoVersion);
        Assert.Contains("Verify tag matches product version", releaseWorkflow, StringComparison.Ordinal);
        Assert.Contains("SHA256SUMS.txt", releaseWorkflow, StringComparison.Ordinal);
        Assert.Matches("actions/upload-artifact@[0-9a-f]{40} # v4", releaseWorkflow);
        Assert.Contains("verify-version.ps1", releaseWorkflow, StringComparison.Ordinal);
        Assert.Contains("verify-version.ps1", msiProject, StringComparison.Ordinal);
        Assert.DoesNotContain("_CargoVersionParsed", msiProject, StringComparison.Ordinal);
        Assert.Contains("Could not parse the engine package version", versionVerifier, StringComparison.Ordinal);
    }

    [Fact]
    public void Msi_UsesOneCrossArchitectureUpgradeFamilyWithDistinctComponents()
    {
        var project = File.ReadAllText(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "FileID.Msi.wixproj"));
        var product = XDocument.Load(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "Product.wxs"));
        var generator = File.ReadAllText(PathInRepo(
            "platforms", "windows", "installer", "FileID.Msi", "Generate-Components.ps1"));
        XNamespace wix = "http://wixtoolset.org/schemas/v4/wxs";

        Assert.Contains("<UpgradeCode>1B5E7FA0-4C42-4A6E-9A12-7F9AE9E1B5C4</UpgradeCode>", project, StringComparison.Ordinal);
        Assert.DoesNotContain("UpgradeCodeX64", project, StringComparison.Ordinal);
        Assert.DoesNotContain("UpgradeCodeArm64", project, StringComparison.Ordinal);
        var majorUpgrade = product.Descendants(wix + "MajorUpgrade").Single();
        Assert.Equal("yes", (string?)majorUpgrade.Attribute("AllowSameVersionUpgrades"));
        Assert.Equal("afterInstallInitialize", (string?)majorUpgrade.Attribute("Schedule"));
        Assert.Contains("-Architecture &quot;$(Platform)&quot;", project, StringComparison.Ordinal);
        Assert.Contains("$Architecture|$($rel.ToLowerInvariant())", generator, StringComparison.Ordinal);
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
        Assert.Equal(2, Regex.Count(publishScript,
            "^\\$WinAppRuntime(?:X64|Arm64)Sha256 = \"[0-9a-f]{64}\"\\r?$",
            RegexOptions.Multiline));
        Assert.Contains("Assert-MicrosoftSignature", publishScript, StringComparison.Ordinal);
        Assert.Contains("Windows App SDK 1.7 runtime", program, StringComparison.Ordinal);
    }

    [Fact]
    public void WindowsWorkflow_RunsBothTestProjects_AndOnlyRetriesTheFormatProbeRace()
    {
        var workflow = File.ReadAllText(PathInRepo(".github", "workflows", "windows-app.yml"));

        Assert.Contains(
            "dotnet test FileID.IpcSchema.Tests/FileID.IpcSchema.Tests.csproj",
            workflow,
            StringComparison.Ordinal);
        Assert.Contains(
            "dotnet test FileID.App.Tests/FileID.App.Tests.csproj",
            workflow,
            StringComparison.Ordinal);
        Assert.DoesNotContain("Test-Path Tests", workflow, StringComparison.Ordinal);
        Assert.Contains("-p:Platform=x64", workflow, StringComparison.Ordinal);
        Assert.Contains("-p:RuntimeIdentifier=win-x64", workflow, StringComparison.Ordinal);
        Assert.Contains("$formatExit -eq 4", workflow, StringComparison.Ordinal);
        Assert.Contains("*Unable to locate dotnet CLI*", workflow, StringComparison.Ordinal);
        Assert.Contains("exit $formatExit", workflow, StringComparison.Ordinal);
    }

    private static (int Width, int Height) ReadPngDimensions(string path)
    {
        var bytes = File.ReadAllBytes(path);
        Assert.True(bytes.Length >= 24);
        Assert.Equal(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }, bytes[..8]);
        return (
            System.Buffers.Binary.BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(16, 4)),
            System.Buffers.Binary.BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(20, 4)));
    }

    private static (int Width, int Height) ReadBmpDimensions(string path)
    {
        var bytes = File.ReadAllBytes(path);
        Assert.True(bytes.Length >= 26);
        Assert.Equal((byte)'B', bytes[0]);
        Assert.Equal((byte)'M', bytes[1]);
        return (
            System.Buffers.Binary.BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(18, 4)),
            Math.Abs(System.Buffers.Binary.BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(22, 4))));
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
