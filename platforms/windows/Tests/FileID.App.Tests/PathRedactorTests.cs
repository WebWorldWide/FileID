using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class PathRedactorTests
{
    [Fact]
    public void Redact_NullOrEmpty_ReturnsAngleNull()
    {
        Assert.Equal("<null>", PathRedactor.Redact(null));
        Assert.Equal("<null>", PathRedactor.Redact(""));
    }

    [Fact]
    public void Redact_PathOutsideUserProfile_ReturnsUnchanged()
    {
        const string path = @"C:\Program Files\FileID\FileID.exe";
        Assert.Equal(path, PathRedactor.Redact(path));
    }

    [Fact]
    public void Redact_MacUsersPrefix_ReplacedWithTilde()
    {
        // Cross-platform DB rows: macOS-shaped paths in the engine's
        // tables get redacted in C# logs too.
        Assert.Equal("~/photos/trip.jpg", PathRedactor.Redact("/Users/adam/photos/trip.jpg"));
    }

    [Fact]
    public void Redact_MacUsersWithoutTrailingSegment_ReturnsHomePlaceholder()
    {
        Assert.Equal("~/<home>", PathRedactor.Redact("/Users/adam"));
    }

    [Fact]
    public void Redact_WindowsHome_StripsUsername()
    {
        var home = System.Environment.GetFolderPath(System.Environment.SpecialFolder.UserProfile);
        var input = System.IO.Path.Combine(home, "photos", "trip.jpg");
        var output = PathRedactor.Redact(input);
        Assert.StartsWith("~", output);
        Assert.DoesNotContain(home, output, System.StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Redact_CaseInsensitiveWindowsMatch()
    {
        // NTFS is case-insensitive; the redactor must match the user
        // profile regardless of the casing of the input path.
        var home = System.Environment.GetFolderPath(System.Environment.SpecialFolder.UserProfile);
        var upper = home.ToUpperInvariant();
        var input = System.IO.Path.Combine(upper, "Pictures", "x.png");
        var output = PathRedactor.Redact(input);
        Assert.StartsWith("~", output);
    }
}
