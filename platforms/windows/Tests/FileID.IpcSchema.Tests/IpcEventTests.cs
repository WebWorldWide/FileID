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
            new DeepAnalyzeFileDone(99, "a cat on a couch", "cat_couch.jpg", "qwen2_5_vl_7b")));
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
        Assert.Null(e.ModelKind); // Not a model error — ModelKind absent.
    }

    [Fact]
    public void EngineError_RoundTripsModelKindOnInstallFailure()
    {
        // D-track regression: install failures must carry model_kind so the
        // app routes the error to the right slot rather than guessing from
        // the path string.
        var ev = IpcEvent.Now(new ErrorEvent(new EngineError(
            Kind: "model_download_failed",
            Message: "Couldn't download mobileclip_image.onnx: timeout",
            Path: @"C:\Users\adam\AppData\Local\FileID\Models\MobileCLIP\mobileclip_image.onnx",
            ModelKind: "mobileclip_s2")));
        var json = IpcCoder.Encode(ev);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var e = Assert.IsType<ErrorEvent>(rt.Payload).Error;
        Assert.Equal("model_download_failed", e.Kind);
        Assert.Equal("mobileclip_s2", e.ModelKind);
    }

    [Fact]
    public void EngineError_PackNotAvailableRoundTrips()
    {
        // D-track soft-failure: a Performance Pack 404 emits pack_not_available
        // instead of model_download_failed, and the app surfaces it without
        // suggesting "check your internet" (the network is fine).
        var ev = IpcEvent.Now(new ErrorEvent(new EngineError(
            Kind: "pack_not_available",
            Message: "CUDA Pack isn't published yet.",
            Path: @"C:\Users\adam\AppData\Local\FileID\Models\packs\cuda\cuda.zip",
            ModelKind: "cuda_pack_x64")));
        var json = IpcCoder.Encode(ev);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var e = Assert.IsType<ErrorEvent>(rt.Payload).Error;
        Assert.Equal("pack_not_available", e.Kind);
        Assert.Equal("cuda_pack_x64", e.ModelKind);
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

    // V14.9-K1: Phase G IPC round-trip tests. Newly-added VerifyCudaPack
    // command + HardwareReprobed event need coverage so a future schema
    // edit doesn't silently break the Settings → Performance Verify flow.

    [Fact]
    public void HardwareReprobed_RoundTripsAllFields()
    {
        var hw = new HardwareInfo(
            GpuVendor: "nvidia",
            AdapterName: "NVIDIA GeForce RTX 4070",
            ExecutionProvider: "cuda",
            PhysicalCpuCores: 16,
            CudaPackPresent: true,
            OpenvinoPackPresent: false,
            QnnPackPresent: false,
            Recommendation: "");
        // Use a plain-ASCII diagnostics string so the test assertion isn't
        // sensitive to JavaScriptEncoder's choice between literal-UTF8 and
        // \uXXXX escape forms for non-ASCII characters.
        var reprobe = new HardwareReprobed(hw, Diagnostics: "Verified at NVIDIA toolkit bin dir");
        var ev = IpcEvent.Now(new HardwareReprobedEvent(reprobe));
        var json = IpcCoder.Encode(ev);

        // Wire shape: {"t":"…","payload":{"hardwareReprobed":{"_0":{"hardware":{…},"diagnostics":"…"}}}}
        Assert.Contains("\"hardwareReprobed\":{\"_0\":", json);
        Assert.Contains("\"gpuVendor\":\"nvidia\"", json);
        Assert.Contains("\"executionProvider\":\"cuda\"", json);
        Assert.Contains("\"cudaPackPresent\":true", json);
        Assert.Contains("\"diagnostics\":\"Verified at NVIDIA toolkit bin dir\"", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var got = Assert.IsType<HardwareReprobedEvent>(rt.Payload).Result;
        Assert.Equal("nvidia", got.Hardware.GpuVendor);
        Assert.Equal("NVIDIA GeForce RTX 4070", got.Hardware.AdapterName);
        Assert.Equal("cuda", got.Hardware.ExecutionProvider);
        Assert.Equal(16u, got.Hardware.PhysicalCpuCores);
        Assert.True(got.Hardware.CudaPackPresent);
        Assert.False(got.Hardware.OpenvinoPackPresent);
        Assert.False(got.Hardware.QnnPackPresent);
        Assert.Equal("Verified at NVIDIA toolkit bin dir", got.Diagnostics);
    }

    [Fact]
    public void HardwareReprobed_OmitsNullDiagnostics()
    {
        // Engine side serializes `Option<String>` with
        // #[serde(skip_serializing_if = "Option::is_none")] — wire JSON
        // should omit the key when the field is None. C# side accepts
        // the absence + leaves the optional field null.
        var hw = new HardwareInfo("nvidia", "RTX 4070", "cuda", 16, true, false, false, "");
        var reprobe = new HardwareReprobed(hw, Diagnostics: null);
        var ev = IpcEvent.Now(new HardwareReprobedEvent(reprobe));
        var json = IpcCoder.Encode(ev);

        Assert.Contains("\"hardwareReprobed\":{\"_0\":", json);
        // Round-trip: even if the C# encoder writes "diagnostics":null,
        // the decoder must still produce a null Diagnostics field.
        var rt = IpcCoder.Decode<IpcEvent>(json);
        var got = Assert.IsType<HardwareReprobedEvent>(rt.Payload).Result;
        Assert.Null(got.Diagnostics);
    }

    [Fact]
    public void HardwareReprobed_DecodesEngineEmittedShapeWithoutDiagnostics()
    {
        // Simulate the exact wire bytes the Rust engine emits when cuDNN
        // is present (`Option<String>` = None → key omitted entirely).
        // The C# decoder must accept the absence of `diagnostics` without
        // throwing.
        const string engineWire = """
            {"t":"2026-05-13T12:00:00+00:00","payload":{"hardwareReprobed":{"_0":{"hardware":{"gpuVendor":"nvidia","adapterName":"RTX 4070","executionProvider":"cuda","physicalCpuCores":16,"cudaPackPresent":true,"openvinoPackPresent":false,"qnnPackPresent":false,"recommendation":""}}}}}
            """;
        var rt = IpcCoder.Decode<IpcEvent>(engineWire.Trim());
        var got = Assert.IsType<HardwareReprobedEvent>(rt.Payload).Result;
        Assert.True(got.Hardware.CudaPackPresent);
        Assert.Null(got.Diagnostics);
    }

    // V14.9-K1: Phase I added `currentCaption: Option<String>` to
    // DeepAnalyzeProgress so the UI renders the live token stream.
    // Lock in the round-trip + absent-field decode so a wire-format
    // regression shows up in CI.

    [Fact]
    public void DeepAnalyzeProgress_RoundTripsCurrentCaption()
    {
        var prog = new DeepAnalyzeProgress(
            Processed: 3,
            Total: 10,
            EtaSeconds: 42.5,
            CurrentPath: @"C:\photos\dog.jpg",
            ModelKind: "qwen2_5_vl_7b",
            CurrentCaption: "A dog sits on");
        var ev = IpcEvent.Now(new DeepAnalyzeProgressEvent(prog));
        var json = IpcCoder.Encode(ev);

        Assert.Contains("\"deepAnalyzeProgress\":{\"_0\":", json);
        Assert.Contains("\"currentCaption\":\"A dog sits on\"", json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var got = Assert.IsType<DeepAnalyzeProgressEvent>(rt.Payload).Progress;
        Assert.Equal(3ul, got.Processed);
        Assert.Equal("A dog sits on", got.CurrentCaption);
    }

    [Fact]
    public void DeepAnalyzeProgress_DecodesEngineEmittedShapeWithoutCurrentCaption()
    {
        // Pre-inference progress events (`processed=idx, current_path=path`)
        // don't have caption text yet — Rust emits None → key omitted.
        const string engineWire = """
            {"t":"2026-05-13T12:00:00+00:00","payload":{"deepAnalyzeProgress":{"_0":{"processed":1,"total":10,"modelKind":"qwen2_5_vl_7b"}}}}
            """;
        var rt = IpcCoder.Decode<IpcEvent>(engineWire.Trim());
        var got = Assert.IsType<DeepAnalyzeProgressEvent>(rt.Payload).Progress;
        Assert.Equal(1ul, got.Processed);
        Assert.Equal(10ul, got.Total);
        Assert.Null(got.CurrentCaption);
        Assert.Null(got.CurrentPath);
        Assert.Null(got.EtaSeconds);
    }
}
