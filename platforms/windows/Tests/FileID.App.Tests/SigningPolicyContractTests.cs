using Xunit;

namespace FileID.App.Tests;

public sealed class SigningPolicyContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void DevelopmentBuildDoesNotRequireSignedEngine()
    {
        Assert.False(FileID.Services.ReleaseSigningPolicy.RequireSignedEngine);
        Assert.Null(FileID.Services.ReleaseSigningPolicy.ExpectedSignerSubject);
        Assert.Null(FileID.Services.ReleaseSigningPolicy.ExpectedSignerPublicKeySha256);
    }

    [Fact]
    public void ReleaseBuildEmbedsPublisherPolicyAndReleaseScriptVerifiesEveryLayer()
    {
        var project = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "FileID.App.csproj"));
        var publish = File.ReadAllText(PathInRepo(
            "platforms", "windows", "build", "publish-bundle.ps1"));
        var workflow = File.ReadAllText(PathInRepo(".github", "workflows", "release.yml"));
        var engineClient = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var trustChecker = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Services", "WinVerifyTrustChecker.cs"));
        var signHelper = File.ReadAllText(PathInRepo(
            "platforms", "windows", "build", "sign.ps1"));

        Assert.Contains("FileIDRequireSignedEngine", project, StringComparison.Ordinal);
        Assert.Contains("FileIDExpectedSignerSubject", project, StringComparison.Ordinal);
        Assert.Contains("FileIDExpectedSignerPublicKeySha256", project, StringComparison.Ordinal);
        Assert.Contains("-SigningAdapter", publish, StringComparison.Ordinal);
        Assert.Contains("verify /pa /all /v", publish, StringComparison.OrdinalIgnoreCase);
        Assert.Contains("burn detach", publish, StringComparison.Ordinal);
        Assert.Contains("burn reattach", publish, StringComparison.Ordinal);
        Assert.Contains("FileIDSetup.verify-engine.exe", publish, StringComparison.Ordinal);
        Assert.Contains("FileIDRequireSignedEngine", publish, StringComparison.Ordinal);
        Assert.Contains("TimeStamperCertificate", publish, StringComparison.Ordinal);
        Assert.Contains("SignerPublicKeySha256", publish, StringComparison.Ordinal);
        Assert.Contains("FileIDExpectedSignerPublicKeySha256", publish, StringComparison.Ordinal);
        Assert.Contains("ExpectedSignerPublicKeySha256", engineClient, StringComparison.Ordinal);
        Assert.Contains("expectedSignerPublicKeySha256", trustChecker, StringComparison.Ordinal);
        Assert.Contains("explicit signing requires", signHelper, StringComparison.Ordinal);
        Assert.DoesNotContain("Skipping codesigning", signHelper, StringComparison.Ordinal);
        Assert.Contains("unsigned-tools", workflow, StringComparison.Ordinal);
        Assert.Contains("thumbprint secret alone is never sufficient", workflow, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("FILEID_EV_THUMBPRINT: ${{ secrets.FILEID_EV_THUMBPRINT }}", workflow, StringComparison.Ordinal);
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
