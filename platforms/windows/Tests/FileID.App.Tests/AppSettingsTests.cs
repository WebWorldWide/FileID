using System.Text.Json;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class AppSettingsTests
{
    // The production serializer options. We mirror the camelCase + null-skip
    // settings so the test asserts the documented wire shape.
    private static readonly JsonSerializerOptions s_options = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = false,
        DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull,
    };

    private static readonly string[] s_excludedFoldersWithMalformed =
    [
        @"C:\Pics\Raw\",
        @"c:\pics\raw",
        "relative\\path",
        "   ",
        @"C:\Pics\Other",
    ];

    private static readonly string[] s_sanitizedExcludedFolders = [@"C:\Pics\Raw", @"C:\Pics\Other"];

    private static readonly string[] s_cloneExpectedExcludedFolders = [@"C:\Pics\Raw", @"C:\Pics\Other"];

    [Fact]
    public void NewInstance_HasDocumentedDefaults()
    {
        var s = new AppSettings();
        Assert.Null(s.LastFolderPath);
        Assert.Null(s.LastFolderDisplay);
        Assert.True(s.SidebarVisible);
        Assert.Equal("library", s.ActiveTab);
        // V15.5b D7: default flipped false → true to match macOS canonical default.
        Assert.True(s.CleanupAutoTagKept);
        Assert.False(s.RestructureTreeMode);
        Assert.Equal("all", s.LibraryKindFilter);
        Assert.True(s.PeopleHideUnknown);
        Assert.Null(s.GpuExecutionProviderOverride);
        Assert.False(s.WelcomeSheetSeen);
        Assert.False(s.DisableAutoInstallCuda);
        Assert.False(s.DisableAutoInstallVulkanRuntime);
        Assert.False(s.DisableAutoInstallCudnn);
        Assert.Equal("qwen2_5_vl_7b", s.SelectedVlmModelKind);
        Assert.False(s.SelectedVlmModelWasUserChosen);
        Assert.Empty(s.ExcludedFolders);
        Assert.True(s.ConfirmCloseOnPendingChanges);
        Assert.Empty(s.DeepAnalyzeExcludedFolders);
        Assert.Equal(7, s.SchemaVersion);
    }

    [Fact]
    public void JsonRoundTrip_PreservesEveryField()
    {
        var original = new AppSettings
        {
            LastFolderPath = @"C:\Users\you\Pictures",
            LastFolderDisplay = "Pictures",
            SidebarVisible = false,
            ActiveTab = "people",
            CleanupAutoTagKept = true,
            RestructureTreeMode = true,
            LibraryKindFilter = "image",
            PeopleHideUnknown = true,
            GpuExecutionProviderOverride = "directml",
            WelcomeSheetSeen = true,
            DisableAutoInstallCuda = true,
            DisableAutoInstallVulkanRuntime = true,
            DisableAutoInstallCudnn = true,
            SelectedVlmModelKind = "mistral_small_3_2",
            SelectedVlmModelWasUserChosen = true,
            SchemaVersion = 1,
        };

        var json = JsonSerializer.Serialize(original, s_options);
        var decoded = JsonSerializer.Deserialize<AppSettings>(json, s_options);
        Assert.NotNull(decoded);

        Assert.Equal(original.LastFolderPath, decoded!.LastFolderPath);
        Assert.Equal(original.LastFolderDisplay, decoded.LastFolderDisplay);
        Assert.Equal(original.SidebarVisible, decoded.SidebarVisible);
        Assert.Equal(original.ActiveTab, decoded.ActiveTab);
        Assert.Equal(original.CleanupAutoTagKept, decoded.CleanupAutoTagKept);
        Assert.Equal(original.RestructureTreeMode, decoded.RestructureTreeMode);
        Assert.Equal(original.LibraryKindFilter, decoded.LibraryKindFilter);
        Assert.Equal(original.PeopleHideUnknown, decoded.PeopleHideUnknown);
        Assert.Equal(original.GpuExecutionProviderOverride, decoded.GpuExecutionProviderOverride);
        Assert.Equal(original.WelcomeSheetSeen, decoded.WelcomeSheetSeen);
        Assert.Equal(original.DisableAutoInstallCuda, decoded.DisableAutoInstallCuda);
        Assert.Equal(original.DisableAutoInstallVulkanRuntime, decoded.DisableAutoInstallVulkanRuntime);
        Assert.Equal(original.DisableAutoInstallCudnn, decoded.DisableAutoInstallCudnn);
        Assert.Equal(original.SelectedVlmModelKind, decoded.SelectedVlmModelKind);
        Assert.Equal(original.SelectedVlmModelWasUserChosen, decoded.SelectedVlmModelWasUserChosen);
        Assert.Equal(original.SchemaVersion, decoded.SchemaVersion);
    }

    [Fact]
    public void Serializer_OmitsNullStringProperties()
    {
        // DefaultIgnoreCondition.WhenWritingNull means nullable strings that
        // are null don't appear in the JSON output. Keeps settings.json
        // compact and forward-compatible.
        var s = new AppSettings { LastFolderPath = null, GpuExecutionProviderOverride = null };
        var json = JsonSerializer.Serialize(s, s_options);
        Assert.DoesNotContain("lastFolderPath", json);
        Assert.DoesNotContain("gpuExecutionProviderOverride", json);
    }

    [Fact]
    public void Serializer_UsesCamelCaseFieldNames()
    {
        var s = new AppSettings { ActiveTab = "people", SidebarVisible = false };
        var json = JsonSerializer.Serialize(s, s_options);
        Assert.Contains("\"activeTab\"", json);
        Assert.Contains("\"sidebarVisible\"", json);
        // PascalCase property names must NOT appear in serialized output.
        Assert.DoesNotContain("\"ActiveTab\"", json);
        Assert.DoesNotContain("\"SidebarVisible\"", json);
    }

    [Fact]
    public void Deserializer_IsCaseSensitive()
    {
        // PropertyNameCaseInsensitive = false. A tampered settings.json
        // that PascalCases field names won't read back as the canonical
        // value — instead the field gets the default. This is the
        // production posture per the AppSettings.cs comment.
        var pascalJson = "{\"ActiveTab\":\"people\"}";
        var decoded = JsonSerializer.Deserialize<AppSettings>(pascalJson, s_options);
        Assert.NotNull(decoded);
        // PascalCase ignored → default "library" stays.
        Assert.Equal("library", decoded!.ActiveTab);
    }

    [Fact]
    public void Deserializer_IgnoresUnknownFields()
    {
        // Forward-compatibility: a future schema version may add fields
        // we don't yet declare. Deserializing must not throw.
        var futureJson = "{\"activeTab\":\"people\",\"someFutureField\":42,\"anotherFuture\":\"x\"}";
        var decoded = JsonSerializer.Deserialize<AppSettings>(futureJson, s_options);
        Assert.NotNull(decoded);
        Assert.Equal("people", decoded!.ActiveTab);
    }

    [Fact]
    public void Deserializer_NullJson_ReturnsNull()
    {
        var decoded = JsonSerializer.Deserialize<AppSettings>("null", s_options);
        Assert.Null(decoded);
    }

    [Fact]
    public void Deserializer_EmptyObject_AppliesDefaults()
    {
        var decoded = JsonSerializer.Deserialize<AppSettings>("{}", s_options);
        Assert.NotNull(decoded);
        Assert.Equal("library", decoded!.ActiveTab);
        Assert.True(decoded.SidebarVisible);
        // "{}" carries no schemaVersion → property default (current schema, v7).
        Assert.Equal(7, decoded.SchemaVersion);
        // Fields absent from an old settings.json take their safe defaults.
        Assert.Empty(decoded.ExcludedFolders);
        Assert.True(decoded.ConfirmCloseOnPendingChanges);
        Assert.Empty(decoded.DeepAnalyzeExcludedFolders);
    }

    [Fact]
    public void SanitizeExcludedFolders_DropsMalformedAndDedupes()
    {
        var result = AppSettings.SanitizeExcludedFolders(s_excludedFoldersWithMalformed);
        Assert.Equal(s_sanitizedExcludedFolders, result);
    }

    [Fact]
    public void SanitizeExcludedFolders_CapsAtBound()
    {
        var many = new List<string>();
        for (int i = 0; i < 400; i++) many.Add($@"C:\x\{i}");
        var result = AppSettings.SanitizeExcludedFolders(many);
        Assert.Equal(256, result.Count);
    }

    [Fact]
    public void SanitizeExcludedFolders_NullInput_ReturnsEmpty()
    {
        Assert.Empty(AppSettings.SanitizeExcludedFolders(null));
    }

    [Fact]
    public void CloneForWrite_SnapshotsExcludedFoldersList()
    {
        // The debounced Save() serializes a clone on a worker; a mid-debounce
        // Add on the UI thread must not mutate the snapshot being written.
        var s = new AppSettings();
        s.ExcludedFolders.Add(@"C:\Pics\Raw");
        var clone = (AppSettings)typeof(AppSettings)
            .GetMethod("CloneForWrite", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!
            .Invoke(s, null)!;
        s.ExcludedFolders.Add(@"C:\Pics\Other");
        Assert.Single(clone.ExcludedFolders);
        Assert.Equal(s_cloneExpectedExcludedFolders, s.ExcludedFolders);
    }

    [Fact]
    public void DeepAnalyzeExcludedFolders_IsSeparateFromScanExcludedFolders()
    {
        // The two lists are deliberately independent — excluding a folder
        // from scanning does not exclude it from Deep Analyze, and vice
        // versa (see the field doc comments).
        var s = new AppSettings();
        s.ExcludedFolders.Add(@"C:\Pics\Raw");
        s.DeepAnalyzeExcludedFolders.Add(@"C:\Pics\Private");
        Assert.Equal(new List<string> { @"C:\Pics\Raw" }, s.ExcludedFolders);
        Assert.Equal(new List<string> { @"C:\Pics\Private" }, s.DeepAnalyzeExcludedFolders);
    }

    [Fact]
    public void DeepAnalyzeExcludedFolders_RoundTripsThroughJsonAsCamelCase()
    {
        var original = new AppSettings();
        original.DeepAnalyzeExcludedFolders.Add(@"C:\Pics\Private");
        var json = JsonSerializer.Serialize(original, s_options);
        Assert.Contains("\"deepAnalyzeExcludedFolders\"", json);
        var decoded = JsonSerializer.Deserialize<AppSettings>(json, s_options);
        Assert.NotNull(decoded);
        Assert.Equal(original.DeepAnalyzeExcludedFolders, decoded!.DeepAnalyzeExcludedFolders);
    }

    [Fact]
    public void CloneForWrite_SnapshotsDeepAnalyzeExcludedFoldersList()
    {
        var s = new AppSettings();
        s.DeepAnalyzeExcludedFolders.Add(@"C:\Pics\Private");
        var clone = (AppSettings)typeof(AppSettings)
            .GetMethod("CloneForWrite", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!
            .Invoke(s, null)!;
        s.DeepAnalyzeExcludedFolders.Add(@"C:\Pics\Other");
        Assert.Single(clone.DeepAnalyzeExcludedFolders);
    }
}
