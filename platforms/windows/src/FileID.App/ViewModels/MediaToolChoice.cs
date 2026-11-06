using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>The Video tool picker's choices and the recipe each one sends: Export keeps the
/// video export recipe; trim, remux, audio, proxy and rotate send only the fields they take.</summary>
internal sealed class MediaToolChoice
{
    public static readonly IReadOnlyList<(string Id, string Label)> Tools =
    [
        ("export", "Export"), ("trim", "Trim"), ("remux", "Remux"), ("audio", "Audio"), ("proxy", "Proxy"), ("rotate", "Rotate"),
    ];
    public static readonly IReadOnlyList<string> Containers = ["mp4", "mov"];
    public static readonly IReadOnlyList<uint> ProxySides = [640, 960];
    public static readonly IReadOnlyList<(uint Degrees, string Label)> Rotations = [(90, "90° right"), (180, "180°"), (270, "90° left")];

    public string Tool { get; private set; } = "export";
    public double TrimStart { get; set; }
    public double TrimEnd { get; set; }
    public bool TrimExact { get; set; }
    public string Container { get; set; } = "mp4";
    public uint ProxySide { get; set; } = 640;
    public uint Rotation { get; set; } = 90;

    public bool IsExport => Tool == "export";

    /// <summary>Export, plus each common-media tool the engine reports available.</summary>
    public static IReadOnlyList<(string Id, string Label)> Available(IEnumerable<ToolCapability> capabilities)
    {
        var available = capabilities.Where(capability => capability.Available).Select(capability => capability.Id).ToHashSet(StringComparer.Ordinal);
        return Tools.Where(tool => tool.Id == "export" || available.Contains(tool.Id)).ToArray();
    }

    public static bool IsMediaTool(string id) => id is "trim" or "remux" or "audio" or "proxy" or "rotate";

    public static string? ContainerOf(string path) => Path.GetExtension(path).ToLowerInvariant() switch
    {
        ".mp4" or ".m4v" => "mp4",
        ".mov" => "mov",
        _ => null,
    };

    public string Format => Tool switch
    {
        "audio" => "m4a",
        "proxy" => "mp4",
        "trim" when TrimExact => "mp4",
        _ => Container,
    };

    /// <summary>Remuxing into the container a file already has only copies it; an exact trim is MP4 only.</summary>
    public IReadOnlySet<string> DisabledContainers(IEnumerable<string> sources) => Tool switch
    {
        "remux" => sources.Select(ContainerOf).OfType<string>().ToHashSet(StringComparer.Ordinal),
        "trim" when TrimExact => new HashSet<string>(StringComparer.Ordinal) { "mov" },
        _ => new HashSet<string>(StringComparer.Ordinal),
    };

    public void Adopt(string tool, IReadOnlyList<string> sources)
    {
        Tool = Tools.Any(known => known.Id == tool) ? tool : "export";
        if (Tool is "trim" or "rotate" && sources.Count > 0 && ContainerOf(sources[0]) is { } container) Container = container;
        SettleContainer(sources);
    }

    public void SettleContainer(IEnumerable<string> sources)
    {
        var disabled = DisabledContainers(sources);
        if (disabled.Contains(Container) && Containers.FirstOrDefault(container => !disabled.Contains(container)) is { } allowed) Container = allowed;
    }

    /// <summary>Null for Export. Fields a recipe does not take stay null; the engine refuses them otherwise.</summary>
    public ToolRecipe? Recipe() => Tool switch
    {
        "trim" => new ToolRecipe("trim", Format, 1920, TrimStartSeconds: TrimStart, TrimEndSeconds: TrimEnd, TrimMode: TrimExact ? "exact" : "keyframe"),
        "remux" or "audio" => new ToolRecipe(Tool, Format, 1920),
        "proxy" => new ToolRecipe("proxy", "mp4", ProxySide),
        "rotate" => new ToolRecipe("rotate", Format, 1920, RotationDegrees: Rotation),
        _ => null,
    };
}
