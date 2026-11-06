using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class MediaToolChoiceTests
{
    private static readonly string[] MediaInputs = { "mp4", "mov", "m4v" };
    private static readonly string[] Containers = { "mp4", "mov" };
    private static readonly string[] M4a = { "m4a" };
    private static readonly string[] MediaIds = { "trim", "remux", "audio", "proxy", "rotate" };
    private static readonly string[] ExportOnly = { "export" };
    private static readonly string[] AllTools = { "export", "trim", "remux", "audio", "proxy", "rotate" };
    private static readonly string[] AllLabels = { "Export", "Trim", "Remux", "Audio", "Proxy", "Rotate" };
    private static readonly string[] ExportAndRotate = { "export", "rotate" };
    private static readonly string[] RotationLabels = { "90° right", "180°", "90° left" };
    private static readonly string[] MovSource = { @"C:\v\Clip.MOV" };
    private static readonly string[] Mp4Source = { @"C:\v\Clip.mp4" };
    private static readonly string[] M4vSource = { @"C:\v\Phone.M4V" };
    private static readonly string[] MixedSources = { @"C:\v\a.mp4", @"C:\v\b.mov" };
    private static readonly string[] MovOnly = { "mov" };

    private static ToolCapability Capability(string id, bool available = true, string[]? outputs = null) =>
        new(id, available, MediaInputs, outputs ?? Containers, "detail");

    [Fact]
    public void ToolPickerHidesToolsTheEngineReportsUnavailable()
    {
        Assert.Equal(ExportOnly, MediaToolChoice.Available(Array.Empty<ToolCapability>()).Select(tool => tool.Id));
        var all = MediaIds.Select(id => Capability(id)).ToArray();
        Assert.Equal(AllTools, MediaToolChoice.Available(all).Select(tool => tool.Id));
        Assert.Equal(AllLabels, MediaToolChoice.Available(all).Select(tool => tool.Label));
        ToolCapability[] partial = [Capability("trim", false), Capability("rotate"), Capability("photo")];
        Assert.Equal(ExportAndRotate, MediaToolChoice.Available(partial).Select(tool => tool.Id));
        Assert.False(MediaToolChoice.IsMediaTool("video"));
        Assert.True(MediaToolChoice.IsMediaTool("proxy"));
    }

    [Fact]
    public void EachToolSendsOnlyTheFieldsItsRecipeTakes()
    {
        var choice = new MediaToolChoice();
        Assert.Null(choice.Recipe());

        choice.Adopt("trim", MovSource);
        choice.TrimStart = 1.25;
        choice.TrimEnd = 4.5;
        Assert.Equal(new ToolRecipe("trim", "mov", 1920, TrimStartSeconds: 1.25, TrimEndSeconds: 4.5, TrimMode: "keyframe"), choice.Recipe());
        choice.TrimExact = true;
        Assert.Equal("mp4", choice.Recipe()!.Format);
        Assert.Equal("exact", choice.Recipe()!.TrimMode);
        Assert.Contains("mov", choice.DisabledContainers(MovSource));

        choice.Adopt("audio", Array.Empty<string>());
        Assert.Equal(new ToolRecipe("audio", "m4a", 1920), choice.Recipe());

        choice.Adopt("proxy", Array.Empty<string>());
        choice.ProxySide = 960;
        Assert.Equal(new ToolRecipe("proxy", "mp4", 960), choice.Recipe());

        choice.Adopt("rotate", Mp4Source);
        foreach (var (degrees, _) in MediaToolChoice.Rotations)
        {
            choice.Rotation = degrees;
            var recipe = choice.Recipe()!;
            Assert.Equal(new ToolRecipe("rotate", "mp4", 1920, RotationDegrees: degrees), recipe);
            Assert.Null(recipe.AllowUpscale);
            Assert.Null(recipe.VideoAspectRatio);
            Assert.Null(recipe.TrimMode);
        }
        Assert.Equal(RotationLabels, MediaToolChoice.Rotations.Select(rotation => rotation.Label));

        choice.Adopt("unknown", Array.Empty<string>());
        Assert.True(choice.IsExport);
    }

    [Fact]
    public void RemuxGreysOutTheContainerTheSourcesAlreadyHave()
    {
        var choice = new MediaToolChoice { Container = "mov" };
        choice.Adopt("remux", MovSource);
        Assert.Equal(MovOnly, choice.DisabledContainers(MovSource));
        Assert.Equal("mp4", choice.Recipe()!.Format);
        choice.SettleContainer(M4vSource);
        Assert.Equal("mov", choice.Recipe()!.Format);
        Assert.Equal(2, choice.DisabledContainers(MixedSources).Count);
        Assert.Null(MediaToolChoice.ContainerOf(@"C:\v\notes.txt"));
    }

    [Fact]
    public void MediaCapabilitiesPreviewOnlyWhenAvailableAndTheFormatIsOffered()
    {
        Assert.True(ToolPreviewState.Supports(Capability("trim"), "mov", 1920));
        Assert.True(ToolPreviewState.Supports(Capability("audio", true, M4a), "m4a", 1920));
        Assert.False(ToolPreviewState.Supports(Capability("audio", true, M4a), "mp4", 1920));
        Assert.False(ToolPreviewState.Supports(Capability("remux", false), "mp4", 1920));
        Assert.False(ToolPreviewState.Supports(Capability("videoEnhancement"), "mp4", 1920));
    }
}
