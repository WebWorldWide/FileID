using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class RestructureLargePlanCategoryVm
{
    public required string Category { get; init; }
    public required ulong Count { get; init; }

    public string CountText => Count.ToString("N0");
    public string AutomationName => $"{Category}: {Count:N0} files";
}

internal static class RestructurePlanPresentation
{
    internal const int LargePlanCategoryCap = 8;

    internal static ulong TotalMoves(RestructurePlan plan)
        => plan.TotalMoves ?? (ulong)plan.Moves.Count;

    internal static bool TryGetCompleteConfidenceCounts(
        RestructurePlan plan,
        out RestructureConfidenceCounts counts)
    {
        if (plan.ConfidenceCounts is not { } complete)
        {
            counts = null!;
            return false;
        }
        counts = complete;

        try
        {
            return checked(counts.Auto + counts.Review + counts.Ask + counts.Unknown)
                == TotalMoves(plan);
        }
        catch (OverflowException)
        {
            return false;
        }
    }

    internal static bool CanApplyStoredPlan(
        RestructurePlan plan,
        bool visibleSampleIsSafe)
        => plan.Truncated
           && !string.IsNullOrWhiteSpace(plan.PlanId)
           && visibleSampleIsSafe
           && TryGetCompleteConfidenceCounts(plan, out var counts)
           && counts.Auto > 0;

    internal static bool HasMissingContentSignals(
        int contentEligibleFiles,
        int clipEmbeddings,
        int textEmbeddings)
        => contentEligibleFiles > 0
           && ((long)clipEmbeddings + textEmbeddings) * 5
           < (long)contentEligibleFiles * 4;

    internal static bool IsDriveRoot(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        try
        {
            var fullPath = Path.GetFullPath(path).TrimEnd('\\', '/');
            var root = Path.GetPathRoot(fullPath)?.TrimEnd('\\', '/');
            return !string.IsNullOrEmpty(root) &&
                   string.Equals(fullPath, root, StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return false;
        }
    }

    internal static int CategoryCount(
        IReadOnlyList<RestructureCategoryCount> categories)
        => categories
            .Where(category => category.Count > 0)
            .Select(category => NormalizeCategory(category.Category))
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Count();

    internal static IReadOnlyList<RestructureLargePlanCategoryVm> TopCategories(
        IReadOnlyList<RestructureCategoryCount> categories,
        int cap = LargePlanCategoryCap)
    {
        if (cap <= 0) return Array.Empty<RestructureLargePlanCategoryVm>();

        return categories
            .Where(category => category.Count > 0)
            .GroupBy(
                category => NormalizeCategory(category.Category),
                StringComparer.OrdinalIgnoreCase)
            .Select(group => new RestructureLargePlanCategoryVm
            {
                Category = group.Key,
                Count = group.Aggregate(
                    0UL,
                    (total, category) => total + category.Count),
            })
            .OrderByDescending(category => category.Count)
            .ThenBy(category => category.Category, StringComparer.OrdinalIgnoreCase)
            .Take(cap)
            .ToArray();
    }

    internal static RestructurePlanIntegrity InspectPreview(RestructurePlan plan)
    {
        var duplicateDestinations = plan.Moves
            .Where(move => !string.IsNullOrWhiteSpace(move.Destination))
            .GroupBy(
                move => NormalizePath(move.Destination),
                StringComparer.OrdinalIgnoreCase)
            .Count(group => group.Count() > 1);

        var outsideRoot = 0;
        var invalidPaths = 0;
        foreach (var move in plan.Moves)
        {
            if (string.IsNullOrWhiteSpace(move.Source) ||
                string.IsNullOrWhiteSpace(move.Destination))
            {
                invalidPaths++;
                continue;
            }

            if (!IsWithinRoot(move.Source, plan.LibraryRoot) ||
                !IsWithinRoot(move.Destination, plan.LibraryRoot))
            {
                outsideRoot++;
            }

            if (string.Equals(
                    NormalizePath(move.Source),
                    NormalizePath(move.Destination),
                    StringComparison.OrdinalIgnoreCase))
            {
                invalidPaths++;
            }
        }

        return new RestructurePlanIntegrity(
            duplicateDestinations,
            outsideRoot,
            invalidPaths);
    }

    private static string NormalizeCategory(string? category)
        => string.IsNullOrWhiteSpace(category) ? "Unsorted" : category.Trim();

    private static bool IsWithinRoot(string path, string root)
    {
        try
        {
            var fullRoot = Path.GetFullPath(root).TrimEnd('\\', '/');
            var fullPath = Path.GetFullPath(path).TrimEnd('\\', '/');
            return string.Equals(fullPath, fullRoot, StringComparison.OrdinalIgnoreCase) ||
                   fullPath.StartsWith(
                       fullRoot + Path.DirectorySeparatorChar,
                       StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return false;
        }
    }

    private static string NormalizePath(string path)
    {
        try
        {
            return Path.GetFullPath(path).TrimEnd('\\', '/');
        }
        catch
        {
            return path.Trim().TrimEnd('\\', '/');
        }
    }
}

internal readonly record struct RestructurePlanIntegrity(
    int DuplicateDestinations,
    int OutsideRootMoves,
    int InvalidPaths)
{
    internal bool IsSafe =>
        DuplicateDestinations == 0 &&
        OutsideRootMoves == 0 &&
        InvalidPaths == 0;

    internal string Summary =>
        $"{DuplicateDestinations:N0} duplicate destinations, " +
        $"{OutsideRootMoves:N0} outside-root moves, {InvalidPaths:N0} invalid paths";
}
