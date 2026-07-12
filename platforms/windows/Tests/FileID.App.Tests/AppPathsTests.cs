using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class AppPathsTests
{
    [Fact]
    public void ResolveRoot_PrefersLocalAppDataEnvironmentOverride()
    {
        var result = AppPaths.ResolveRoot(
            @"D:\Isolated\Local",
            @"C:\Users\fallback",
            @"C:\KnownLocal",
            @"C:\Users\known");

        Assert.Equal(@"D:\Isolated\Local\FileID", result);
    }

    [Fact]
    public void ResolveRoot_EmptyLocalAppDataFallsBackToUserProfileEnvironment()
    {
        var result = AppPaths.ResolveRoot(
            "  ",
            @"D:\IsolatedUser",
            @"C:\KnownLocal",
            @"C:\Users\known");

        Assert.Equal(@"D:\IsolatedUser\AppData\Local\FileID", result);
    }

    [Fact]
    public void ExplicitDbAndModelOverridesWinAndHuggingFaceDerivesFromModels()
    {
        const string root = @"C:\State\FileID";
        var db = AppPaths.ResolveDbPath(root, @"D:\Scratch\test.sqlite");
        var models = AppPaths.ResolveModelsDir(root, @"E:\Models");

        Assert.Equal(@"D:\Scratch\test.sqlite", db);
        Assert.Equal(@"E:\Models", models);
        Assert.Equal(@"E:\Models\HuggingFace", AppPaths.ResolveHuggingFaceDir(models));
    }

    [Fact]
    public void EmptyDbAndModelOverridesUseRootDefaults()
    {
        const string root = @"C:\State\FileID";

        Assert.Equal(@"C:\State\FileID\fileid.sqlite", AppPaths.ResolveDbPath(root, ""));
        Assert.Equal(@"C:\State\FileID\Models", AppPaths.ResolveModelsDir(root, "  "));
    }
}
