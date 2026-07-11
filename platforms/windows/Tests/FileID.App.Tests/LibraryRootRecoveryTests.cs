// LibraryRootRecovery.PickRecoveredRoot — the startup rule that heals a
// LastFolderPath pointing at a deleted directory by falling back to the
// engine DB's most recent scan root that still exists on disk. Filesystem
// access is injected so these run headlessly.

using System;
using System.Collections.Generic;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class LibraryRootRecoveryTests
{
    private static readonly string[] s_adlonOnly = [@"F:\Adlon Drive\Family Shared"];
    private static readonly string[] s_adlonThenOld = [@"F:\Adlon Drive\Family Shared", @"C:\OldLibrary"];
    private static readonly string[] s_unpluggedThenLocal = [@"F:\Unplugged\Root", @"C:\StillHere"];
    private static readonly string[] s_deadLibraryVariants = [@"c:\library\", @"C:\LIBRARY"];
    private static readonly string?[] s_withNullAndBlank = [null, "  ", @"D:\Photos\"];

    private static Func<string, bool> Exists(params string[] present)
    {
        var set = new HashSet<string>(present, StringComparer.OrdinalIgnoreCase);
        return p => set.Contains(p.TrimEnd('\\', '/'));
    }

    [Fact] // never-picked folder → onboarding is correct; no recovery
    public void NullOrEmptyLastFolder_NoRecovery()
    {
        Assert.Null(LibraryRootRecovery.PickRecoveredRoot(
            null, s_adlonOnly, Exists(@"F:\Adlon Drive\Family Shared")));
        Assert.Null(LibraryRootRecovery.PickRecoveredRoot(
            "", s_adlonOnly, Exists(@"F:\Adlon Drive\Family Shared")));
    }

    [Fact] // saved folder still exists → nothing to do
    public void ExistingLastFolder_NoRecovery()
    {
        Assert.Null(LibraryRootRecovery.PickRecoveredRoot(
            @"C:\Users\me\Pictures",
            s_adlonOnly,
            Exists(@"C:\Users\me\Pictures", @"F:\Adlon Drive\Family Shared")));
    }

    [Fact] // the real-machine repro: settings point at a deleted test corpus,
           // the DB holds a scan of an attached external drive → fall back
    public void MissingLastFolder_FallsBackToNewestExistingDbRoot()
    {
        var recovered = LibraryRootRecovery.PickRecoveredRoot(
            @"C:\Users\me\Documents\Codex\win-isolated-corpus",
            s_adlonThenOld,
            Exists(@"F:\Adlon Drive\Family Shared", @"C:\OldLibrary"));
        Assert.Equal(@"F:\Adlon Drive\Family Shared", recovered);
    }

    [Fact] // newest root unreachable (drive unplugged) → older existing root wins
    public void MissingNewestRoot_SkipsToOlderExistingRoot()
    {
        var recovered = LibraryRootRecovery.PickRecoveredRoot(
            @"C:\Gone",
            s_unpluggedThenLocal,
            Exists(@"C:\StillHere"));
        Assert.Equal(@"C:\StillHere", recovered);
    }

    [Fact] // no reachable root anywhere (external drive unplugged) → change
           // NOTHING: the saved path may come back when the drive returns
    public void NothingReachable_LeavesStateAlone()
    {
        Assert.Null(LibraryRootRecovery.PickRecoveredRoot(
            @"F:\Adlon Drive\Family Shared",
            s_adlonOnly,
            Exists()));
    }

    [Fact] // the DB root IS the dead saved folder (case/trailing-slash variants
           // included) → not a recovery candidate
    public void DbRootEqualToDeadLastFolder_IsSkipped()
    {
        Assert.Null(LibraryRootRecovery.PickRecoveredRoot(
            @"C:\Library",
            s_deadLibraryVariants,
            // equal roots must be skipped before probing at all
            Exists()));
    }

    [Fact] // null / blank DB rows are tolerated
    public void NullAndBlankDbRoots_AreIgnored()
    {
        var recovered = LibraryRootRecovery.PickRecoveredRoot(
            @"C:\Gone",
            s_withNullAndBlank,
            Exists(@"D:\Photos"));
        Assert.Equal(@"D:\Photos", recovered);
    }
}
