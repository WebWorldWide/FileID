// Round-trip tests for IpcEvent. Asserts the `_0` wrapper is correctly
// produced for single-positional cases AND that `discoveryComplete` (the
// only named-parameter case) is NOT `_0`-wrapped.

using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class IpcEventTests
{
    [Fact]
    public void Ready_WrapsPayloadIn_0()
    {
        var info = new EngineInfo("0.1.0", 1234, 14, 16.0);
        var ev = IpcEvent.Now(new ReadyEvent(info));
        var json = IpcCoder.Encode(ev);

        // {"t":"...","payload":{"ready":{"_0":{...}}}}
        Assert.Contains("\"ready\":{\"_0\":", json);
        Assert.Contains("\"version\":\"0.1.0\"", json);
        Assert.Contains("\"physicalMemoryGB\":16", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var ready = Assert.IsType<ReadyEvent>(rt.Payload);
        Assert.Equal("0.1.0", ready.Info.Version);
        Assert.Equal(1234, ready.Info.Pid);
        Assert.Equal(14u, ready.Info.WorkerCap);
        Assert.Equal(16.0, ready.Info.PhysicalMemoryGB);
    }

    [Fact]
    public void DiscoveryComplete_DoesNotUse_0Wrapper()
    {
        var ev = IpcEvent.Now(new DiscoveryCompleteEvent(50_000));
        var json = IpcCoder.Encode(ev);

        // {"t":"...","payload":{"discoveryComplete":{"totalFiles":50000}}}
        Assert.Contains("\"discoveryComplete\":{\"totalFiles\":50000}", json);
        // Critically: no "_0" wrapper for this variant. Schema special-case.
        Assert.DoesNotContain("\"discoveryComplete\":{\"_0\"", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var dc = Assert.IsType<DiscoveryCompleteEvent>(rt.Payload);
        Assert.Equal(50_000ul, dc.TotalFiles);
    }

    [Fact]
    public void ScanProgress_RoundTripsAllFields()
    {
        var prog = new ScanProgress(
            SessionId: "sess-1",
            Phase: ScanPhase.Tagging,
            Total: 50_000,
            Discovered: 50_000,
            Processed: 12_345,
            Failed: 7,
            FilesPerSecond: 87.4,
            EtaSeconds: 432.1,
            ResidentMb: 612,
            AvailableMb: 4200);
        var ev = IpcEvent.Now(new ProgressEvent(prog));
        var json = IpcCoder.Encode(ev);

        // Spot-check special property names that diverge from camelCase default.
        Assert.Contains("\"sessionID\":\"sess-1\"", json);
        Assert.Contains("\"residentMB\":612", json);
        Assert.Contains("\"availableMB\":4200", json);
        Assert.Contains("\"phase\":\"tagging\"", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var p = Assert.IsType<ProgressEvent>(rt.Payload).Progress;
        Assert.Equal("sess-1", p.SessionId);
        Assert.Equal(ScanPhase.Tagging, p.Phase);
        Assert.Equal(50_000ul, p.Total);
        Assert.Equal(12_345ul, p.Processed);
        Assert.Equal(7ul, p.Failed);
        Assert.Equal(87.4, p.FilesPerSecond);
        Assert.Equal(432.1, p.EtaSeconds);
        Assert.Equal(612ul, p.ResidentMb);
        Assert.Equal(4200ul, p.AvailableMb);
    }

    [Fact]
    public void DeepAnalyzeFileDone_PreservesFileIDCasing()
    {
        var ev = IpcEvent.Now(new DeepAnalyzeFileDoneEvent(
            new DeepAnalyzeFileDone(99, "a cat on a couch", "cat_couch.jpg", "qwen2_5_vl_3b")));
        var json = IpcCoder.Encode(ev);

        Assert.Contains("\"fileID\":99", json);
        Assert.DoesNotContain("\"fileId\"", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var fd = Assert.IsType<DeepAnalyzeFileDoneEvent>(rt.Payload).FileDone;
        Assert.Equal(99, fd.FileId);
        Assert.Equal("a cat on a couch", fd.Description);
        Assert.Equal("cat_couch.jpg", fd.ProposedName);
    }

    [Fact]
    public void EngineError_RoundTripsKindAndPath()
    {
        var ev = IpcEvent.Now(new ErrorEvent(new EngineError(
            Kind: "vision_failed",
            Message: "OCR timed out",
            Path: @"C:\Users\adam\photos\bad.jpg")));
        var json = IpcCoder.Encode(ev);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var e = Assert.IsType<ErrorEvent>(rt.Payload).Error;
        Assert.Equal("vision_failed", e.Kind);
        Assert.Equal("OCR timed out", e.Message);
        Assert.Equal(@"C:\Users\adam\photos\bad.jpg", e.Path);
    }

    [Fact]
    public void QueueState_WithRunningJob_RoundTrips()
    {
        var qs = new QueueState(
            Running: new QueuedJob("job-1", JobCategory.Scan, "Scan Library", 120.0),
            Pending: new[]
            {
                new QueuedJob("job-2", JobCategory.FaceCluster, "Group People", 30.0),
                new QueuedJob("job-3", JobCategory.DeepAnalyze, "Captions", 600.0),
            },
            TotalEtaSeconds: 750.0);
        var ev = IpcEvent.Now(new QueueStateEvent(qs));
        var json = IpcCoder.Encode(ev);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var state = Assert.IsType<QueueStateEvent>(rt.Payload).State;
        Assert.NotNull(state.Running);
        Assert.Equal("job-1", state.Running!.Id);
        Assert.Equal(JobCategory.Scan, state.Running.Category);
        Assert.Equal(2, state.Pending.Count);
        Assert.Equal(JobCategory.FaceCluster, state.Pending[0].Category);
        Assert.Equal(JobCategory.DeepAnalyze, state.Pending[1].Category);
    }

    [Fact]
    public void DateTimeOffset_T_SerializesAsIso8601()
    {
        var ev = new IpcEvent(
            T: new DateTimeOffset(2026, 5, 2, 12, 0, 0, TimeSpan.Zero),
            new ReadyEvent(new EngineInfo("0", 1, 1, 1.0)));
        var json = IpcCoder.Encode(ev);
        Assert.Contains("\"t\":\"2026-05-02T12:00:00", json);
    }

    [Fact]
    public void UnknownEventVariant_ThrowsJsonException()
    {
        const string bad = """{"t":"2026-01-01T00:00:00+00:00","payload":{"alienVariant":{"_0":{}}}}""";
        Assert.Throws<JsonException>(() => IpcCoder.Decode<IpcEvent>(bad));
    }
}
