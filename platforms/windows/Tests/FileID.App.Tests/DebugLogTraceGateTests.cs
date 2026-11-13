// DebugLog Trace-gate tests.
//
// The Trace level exists to move the per-tile/per-frame [THUMB] firehose off
// the synchronous locked-file-I/O hot path that runs on the UI thread during
// scroll. It is gated OFF by default and must skip I/O entirely when disabled.
// Debug is gated by FILEID_DEBUG_LOG; everything at Info and above stays
// always-on so the [APPLY:N]/[ENGINE-SUB] forensic tail (CLAUDE.md:
// load-bearing) is never suppressed. These tests lock
// that contract so a future refactor can't silently un-gate the firehose or,
// worse, gate the forensics.

using System;
using System.IO;
using System.Text;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class DebugLogTraceGateTests
{
    private static string ReadLog()
    {
        var path = AppPaths.AppLogPath;
        if (!File.Exists(path))
        {
            return string.Empty;
        }
        // Share read/write — DebugLog appends under its own lock concurrently.
        using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
        using var sr = new StreamReader(fs, Encoding.UTF8);
        return sr.ReadToEnd();
    }

    [Fact]
    public void Trace_WhenDisabled_WritesNothing()
    {
        var original = DebugLog.TraceEnabled;
        try
        {
            DebugLog.TraceEnabled = false;
            var marker = $"[THUMB] gate-test-disabled-{Guid.NewGuid():N}";
            DebugLog.Trace(marker);
            Assert.DoesNotContain(marker, ReadLog());
        }
        finally
        {
            DebugLog.TraceEnabled = original;
        }
    }

    [Fact]
    public void Trace_WhenEnabled_Writes()
    {
        var original = DebugLog.TraceEnabled;
        try
        {
            DebugLog.TraceEnabled = true;
            var marker = $"[THUMB] gate-test-enabled-{Guid.NewGuid():N}";
            DebugLog.Trace(marker);
            Assert.Contains(marker, ReadLog());
        }
        finally
        {
            DebugLog.TraceEnabled = original;
        }
    }

    [Fact]
    public void CrashDumpReadsTailWhilePersistentWriterIsOpen()
    {
        var marker = $"[ENGINE-SUB] crash-tail-{Guid.NewGuid():N}";
        DebugLog.Error(marker);
        var dump = DebugLog.WriteCrashDump("DebugLogTraceGateTests", null, terminating: false);
        try
        {
            Assert.False(string.IsNullOrWhiteSpace(dump));
            Assert.Contains(marker, File.ReadAllText(dump, Encoding.UTF8));
        }
        finally
        {
            if (!string.IsNullOrWhiteSpace(dump)) File.Delete(dump);
        }
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void InfoAndAbove_AlwaysWrite_RegardlessOfTraceAndDebugFlags(bool enabled)
    {
        var originalTrace = DebugLog.TraceEnabled;
        var originalDebug = DebugLog.DebugEnabled;
        try
        {
            DebugLog.TraceEnabled = enabled;
            DebugLog.DebugEnabled = enabled;
            // The forensic tail ([APPLY:N]/[ENGINE-SUB] are Info; SafeRun logs
            // at Error) must fire irrespective of either gate.
            var info = $"[APPLY:0] gate-test-info-{Guid.NewGuid():N}";
            var err = $"[ENGINE-SUB] gate-test-error-{Guid.NewGuid():N}";
            DebugLog.Info(info);
            DebugLog.Error(err);
            var log = ReadLog();
            Assert.Contains(info, log);
            Assert.Contains(err, log);
        }
        finally
        {
            DebugLog.TraceEnabled = originalTrace;
            DebugLog.DebugEnabled = originalDebug;
        }
    }

    [Fact]
    public void Debug_IsSkippedUnlessDebugLoggingIsOn()
    {
        var original = DebugLog.DebugEnabled;
        try
        {
            DebugLog.DebugEnabled = false;
            var skipped = $"[THUMB] gate-test-debug-off-{Guid.NewGuid():N}";
            DebugLog.Debug(skipped);
            DebugLog.DebugEnabled = true;
            var written = $"[THUMB] gate-test-debug-on-{Guid.NewGuid():N}";
            DebugLog.Debug(written);
            var log = ReadLog();
            Assert.DoesNotContain(skipped, log);
            Assert.Contains(written, log);
        }
        finally
        {
            DebugLog.DebugEnabled = original;
        }
    }

    [Fact]
    public void ForensicSubscriberLinesStayAtInfo()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "platforms", "windows", "FileID.sln")))
        {
            dir = dir.Parent;
        }
        Assert.NotNull(dir);
        var app = Path.Combine(dir!.FullName, "platforms", "windows", "src", "FileID.App");
        foreach (var file in Directory.EnumerateFiles(app, "*.cs", SearchOption.AllDirectories))
        {
            if (file.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal)
                || file.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.Ordinal))
            {
                continue;
            }
            var source = File.ReadAllText(file);
            Assert.DoesNotContain("DebugLog.Debug($\"[ENGINE-SUB", source, StringComparison.Ordinal);
            Assert.DoesNotContain("DebugLog.Debug($\"[APPLY", source, StringComparison.Ordinal);
            Assert.DoesNotContain("DebugLog.Trace($\"[ENGINE-SUB", source, StringComparison.Ordinal);
        }
    }
}
