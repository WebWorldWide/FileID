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
    public void Redact_PathOutsideUserProfile_KeepsOnlyTheTail()
    {
        const string path = @"C:\Program Files\FileID\FileID.exe";
        Assert.Equal("…/FileID/FileID.exe", PathRedactor.Redact(path));
    }

    [Fact]
    public void Redact_MacUsersPrefix_KeepsOnlyTheTail()
    {
        Assert.Equal("…/photos/trip.jpg", PathRedactor.Redact("/Users/adam/photos/trip.jpg"));
    }

    [Fact]
    public void Redact_MacUsersWithoutTrailingSegment_ReturnsHomePlaceholder()
    {
        Assert.Equal("…", PathRedactor.Redact("/Users/adam"));
    }

    [Fact]
    public void Redact_WindowsHome_StripsUsername()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var input = Path.Combine(home, "photos", "trip.jpg");
        var output = PathRedactor.Redact(input);
        Assert.Equal("…/photos/trip.jpg", output);
        Assert.DoesNotContain(home, output, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Redact_CaseInsensitiveWindowsMatch()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var input = Path.Combine(home.ToUpperInvariant(), "Pictures", "x.png");
        Assert.Equal("…/Pictures/x.png", PathRedactor.Redact(input));
    }

    [Fact]
    public void Redact_NestedUsersBackup_DropsThePossibleUsername()
    {
        Assert.Equal("…/file.txt", PathRedactor.Redact(@"D:\Backups\Users\alice\file.txt"));
        Assert.Equal("…/Users/file.txt", PathRedactor.Redact(@"D:\Backups\Users\file.txt"));
    }

    [Fact]
    public void Redact_FileIdStatePath_DoesNotExposeTheUserProfile()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var input = Path.Combine(home, "AppData", "Local", "FileID", "Models", "weights.onnx");
        var output = PathRedactor.Redact(input);
        Assert.Equal("…/Models/weights.onnx", output);
        Assert.DoesNotContain(home, output, StringComparison.OrdinalIgnoreCase);
    }
}
