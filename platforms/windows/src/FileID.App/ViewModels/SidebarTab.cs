// SidebarTab -- the pages the shell exposes, grouped into the Prototype v3
// areas (Browse / Analyze / Act) plus Settings. Each carries an id (persisted
// in app-settings.json), a label (displayed), and a Segoe Fluent glyph.

namespace FileID.ViewModels;

internal sealed record SidebarTab(string Id, string Label, string IconGlyph)
{
    public static SidebarTab Library => new("library", "Library", ""); // Photo
    public static SidebarTab People => new("people", "People", ""); // People
    public static SidebarTab Cleanup => new("cleanup", "Duplicates", ""); // Delete
    public static SidebarTab DeepAnalyze => new("deepanalyze", "Describe Files", ""); // FontAwesome equivalent of sparkles
    public static SidebarTab Restructure => new("restructure", "Organize Folders", ""); // FolderHorizontal
    public static SidebarTab Settings => new("settings", "Settings", ""); // Setting

    public static SidebarTab Moments => new("moments", "Search & Moments", "\uE721");
    public static SidebarTab Chat => new("chat", "Chat", "\uE8F2");
    public static SidebarTab Models => new("models", "Models", "\uE950");
    public static SidebarTab Tools => new("tools", "File Tools", "\uE90F");

    public static IReadOnlyList<string> Areas { get; } = new[] { "Browse", "Analyze", "Act" };

    public static IReadOnlyList<SidebarTab> Order { get; } = new[]
    {
        Library, People, Moments, DeepAnalyze, Chat, Models, Cleanup, Restructure, Tools,
    };

    public static IReadOnlyList<SidebarTab> All { get; } = new[]
    {
        Library, People, Moments, DeepAnalyze, Chat, Models, Cleanup, Restructure, Tools, Settings,
    };

    public string? Area => Id switch
    {
        "library" or "people" or "moments" => "Browse",
        "deepanalyze" or "chat" or "models" => "Analyze",
        "cleanup" or "restructure" or "tools" => "Act",
        _ => null,
    };

    public string Summary => Id switch
    {
        "library" => "Every scanned file, with search and preview",
        "people" => "Faces grouped by person; name them here",
        "moments" => "Search inside videos and documents",
        "deepanalyze" => "Write a description and a better file name for each file",
        "chat" => "Ask questions about your files",
        "models" => "Download and manage the local AI models",
        "cleanup" => "Find extra copies and move them to Trash",
        "restructure" => "Suggest a tidier folder layout, then move files",
        "tools" => "Convert, resize or reframe photos and videos",
        "settings" => "Appearance, AI models and storage",
        _ => Label,
    };

    public bool RequiresFolder => Id is "library" or "people" or "cleanup" or "deepanalyze" or "restructure";

    public int ShortcutNumber
    {
        get
        {
            for (int i = 0; i < Order.Count; i++)
            {
                if (Order[i].Id == Id) return i + 1;
            }
            return 0;
        }
    }

    public string Crumb => Area is { } area ? $"{area} / {Label}" : $"FileID / {Label}";

    public static IReadOnlyList<SidebarTab> InArea(string area) =>
        Order.Where(t => t.Area == area).ToArray();

    public static SidebarTab ById(string id) =>
        All.FirstOrDefault(t => t.Id == id) ?? Library;
}
