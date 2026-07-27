// DebugLog Trace-gate tests.
//
// The Trace level exists to move the per-tile/per-frame [THUMB] firehose off
// the synchronous locked-file-I/O hot path that runs on the UI thread during
// scroll. It is gated OFF by default and must skip I/O entirely when disabled.
// Everything at Debug and above stays always-on so the [APPLY:N]/[ENGINE-SUB]
// forensic tail (CLAUDE.md: load-bearing) is never suppressed. These tests lock
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
    public void DebugAndAbove_AlwaysWrite_RegardlessOfTraceFlag(bool traceEnabled)
    {
        var original = DebugLog.TraceEnabled;
        try
        {
            DebugLog.TraceEnabled = traceEnabled;
            // The forensic tail ([APPLY:N]/[ENGINE-SUB] are Debug; SafeRun logs
            // at Error) must fire irrespective of the Trace flag.
            var dbg = $"[APPLY:0] gate-test-debug-{Guid.NewGuid():N}";
            var err = $"[ENGINE-SUB] gate-test-error-{Guid.NewGuid():N}";
            DebugLog.Debug(dbg);
            DebugLog.Error(err);
            var log = ReadLog();
            Assert.Contains(dbg, log);
            Assert.Contains(err, log);
        }
        finally
        {
            DebugLog.TraceEnabled = original;
        }
    }
}
