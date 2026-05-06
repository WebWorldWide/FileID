// TreeDiffControl — side-by-side current vs proposed folder tree.
//
// Mirror of macOS TreeDiffView. Renders two TreeView columns: the
// current folder structure as discovered, and the proposed structure
// after applying the plan. Folders that change are highlighted gold;
// new folders show a "+", deleted/empty folders fade.
//
// Pure WinUI 3 (no Win2D). Two TreeView controls inside a 2-column Grid,
// driven by ObservableCollection<DiffNode> built from the engine's
// RestructurePlan.Moves list.

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using FileID.IpcSchema;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Restructure;

public sealed class TreeDiffControl : Control
{
    public sealed class DiffNode
    {
        public required string Name { get; init; }
        public required string Path { get; init; }
        /// <summary>"unchanged", "added", "removed", "moved-source", "moved-dest".</summary>
        public required string Status { get; init; }
        public int FileCount { get; set; }
        public ObservableCollection<DiffNode> Children { get; } = new();
        public string DisplayName => FileCount > 0
            ? $"{Name}  ({FileCount})"
            : Name;
        public bool IsHighlighted => Status != "unchanged";
        public Brush HighlightBrush => Status switch
        {
            "added" or "moved-dest" => (Brush)Application.Current.Resources["GoldBrush"],
            "removed" or "moved-source" => (Brush)Application.Current.Resources["TextFillColorTertiaryBrush"],
            _ => (Brush)Application.Current.Resources["TextFillColorPrimaryBrush"],
        };
    }

    private TreeView? _currentTree;
    private TreeView? _proposedTree;
    private RestructurePlan? _plan;

    public TreeDiffControl()
    {
        DefaultStyleKey = typeof(TreeDiffControl);
    }

    public void SetPlan(RestructurePlan? plan)
    {
        _plan = plan;
        Render();
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        _currentTree = GetTemplateChild("PART_CurrentTree") as TreeView;
        _proposedTree = GetTemplateChild("PART_ProposedTree") as TreeView;
        Render();
    }

    private void Render()
    {
        if (_currentTree is null || _proposedTree is null) return;
        _currentTree.RootNodes.Clear();
        _proposedTree.RootNodes.Clear();
        if (_plan is null || _plan.Moves.Count == 0) return;

        var (currentRoot, proposedRoot) = BuildTrees(_plan);
        AddTreeNodes(_currentTree, currentRoot);
        AddTreeNodes(_proposedTree, proposedRoot);
    }

    private static (DiffNode current, DiffNode proposed) BuildTrees(RestructurePlan plan)
    {
        var libraryRoot = plan.LibraryRoot ?? "";
        var current = new DiffNode { Name = LeafName(libraryRoot, "Library"), Path = libraryRoot, Status = "unchanged" };
        var proposed = new DiffNode { Name = LeafName(libraryRoot, "Library"), Path = libraryRoot, Status = "unchanged" };

        var currentBuckets = new Dictionary<string, DiffNode>(StringComparer.OrdinalIgnoreCase) { [""] = current };
        var proposedBuckets = new Dictionary<string, DiffNode>(StringComparer.OrdinalIgnoreCase) { [""] = proposed };

        foreach (var m in plan.Moves)
        {
            // Current: take parent of the source as the folder.
            var srcRel = TrimRoot(m.Source, libraryRoot);
            var srcDir = ParentDir(srcRel);
            EnsurePath(currentBuckets, current, srcDir, "moved-source").FileCount++;

            // Proposed: parent of the destination.
            var dstRel = TrimRoot(m.Destination, libraryRoot);
            var dstDir = ParentDir(dstRel);
            EnsurePath(proposedBuckets, proposed, dstDir, "moved-dest").FileCount++;
        }
        return (current, proposed);
    }

    private static DiffNode EnsurePath(Dictionary<string, DiffNode> buckets, DiffNode root, string relDir, string statusForNew)
    {
        if (string.IsNullOrEmpty(relDir)) return root;
        if (buckets.TryGetValue(relDir, out var existing)) return existing;
        var parentDir = ParentDir(relDir);
        var parent = string.IsNullOrEmpty(parentDir) ? root : EnsurePath(buckets, root, parentDir, statusForNew);
        var node = new DiffNode
        {
            Name = LeafName(relDir, "?"),
            Path = relDir,
            Status = statusForNew,
        };
        parent.Children.Add(node);
        buckets[relDir] = node;
        return node;
    }

    private static string TrimRoot(string p, string root)
    {
        if (!string.IsNullOrEmpty(root) && p.StartsWith(root, StringComparison.OrdinalIgnoreCase))
        {
            p = p.Substring(root.Length);
        }
        return p.TrimStart('\\', '/');
    }

    private static string ParentDir(string p)
    {
        if (string.IsNullOrEmpty(p)) return "";
        var idx = Math.Max(p.LastIndexOf('\\'), p.LastIndexOf('/'));
        return idx <= 0 ? "" : p.Substring(0, idx);
    }

    private static string LeafName(string p, string fallback)
    {
        if (string.IsNullOrEmpty(p)) return fallback;
        var idx = Math.Max(p.LastIndexOf('\\'), p.LastIndexOf('/'));
        return idx < 0 ? p : p.Substring(idx + 1);
    }

    private static void AddTreeNodes(TreeView tree, DiffNode root)
    {
        var rootNode = ToTreeNode(root);
        rootNode.IsExpanded = true;
        tree.RootNodes.Add(rootNode);
    }

    private static TreeViewNode ToTreeNode(DiffNode d)
    {
        var n = new TreeViewNode { Content = d };
        foreach (var child in d.Children.OrderBy(c => c.Name, StringComparer.OrdinalIgnoreCase))
        {
            n.Children.Add(ToTreeNode(child));
        }
        return n;
    }
}
