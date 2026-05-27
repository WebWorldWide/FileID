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
        Assert.False(s.PeopleHideUnknown);
        Assert.Null(s.GpuExecutionProviderOverride);
        Assert.False(s.WelcomeSheetSeen);
        Assert.False(s.DisableAutoInstallCuda);
        Assert.False(s.DisableAutoInstallVulkanRuntime);
        Assert.False(s.DisableAutoInstallCudnn);
        Assert.Equal("qwen2_5_vl_3b", s.SelectedVlmModelKind);
        Assert.Equal(4, s.SchemaVersion);
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
        // "{}" carries no schemaVersion → property default (current schema, v4).
        Assert.Equal(4, decoded.SchemaVersion);
    }
}
