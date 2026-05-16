// SidebarTab -- the six top-level destinations the sidebar exposes.
//
// Mirror of macOS Sidebar.swift's tab enum (Library / People / Cleanup /
// Deep Analyze / Restructure / Settings). Each carries an id (persisted
// in app-settings.json), a label (displayed), and a Segoe Fluent glyph.

namespace FileID.ViewModels;

internal sealed record SidebarTab(string Id, string Label, string IconGlyph)
{
    public static SidebarTab Library => new("library", "Library", ""); // Photo
    public static SidebarTab People => new("people", "People", ""); // People
    public static SidebarTab Cleanup => new("cleanup", "Cleanup", ""); // Delete
    public static SidebarTab DeepAnalyze => new("deepanalyze", "Deep Analyze", ""); // FontAwesome equivalent of sparkles
    public static SidebarTab Restructure => new("restructure", "Restructure", ""); // FolderHorizontal
    public static SidebarTab Settings => new("settings", "Settings", ""); // Setting

    public static IReadOnlyList<SidebarTab> All { get; } = new[]
    {
        Library, People, Cleanup, DeepAnalyze, Restructure, Settings,
    };

    public static SidebarTab ById(string id) =>
        All.FirstOrDefault(t => t.Id == id) ?? Library;
}
