using System;
using System.IO;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

// Regression for the CUDA-pack prewarm loop: the engine writes hashed
// `{id}-{hash}.installed` sentinels, but SettingsView + the auto-installers
// probed only the flat `{id}.installed` name — so an installed pack read as
// missing and its prewarm re-dispatched on every Settings load / engine
// Ready. The shared probe must latch on BOTH forms.
public sealed class SentinelProbeTests : IDisposable
{
    private readonly string _dir;

    public SentinelProbeTests()
    {
        _dir = Path.Combine(Path.GetTempPath(), "fileid-sentinel-tests-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dir);
    }

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { }
    }

    private void Touch(string name) =>
        File.WriteAllText(Path.Combine(_dir, name), string.Empty);

    private void CreateSized(string relative, long length)
    {
        var path = Path.Combine(_dir, relative);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
        stream.SetLength(length);
    }

    [Fact]
    public void FlatSentinel_ReadsInstalled()
    {
        Touch("llama_runtime_cuda_x64.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void HashedSentinel_ReadsInstalled()
    {
        Touch("llama_runtime_cuda_x64-c380324c262d1b84.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void NoSentinel_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void MissingDirectory_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn(Path.Combine(_dir, "nope"), "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void HashedSentinel_DoesNotMatchOtherKinds()
    {
        Touch("llama_runtime_x64-bbc12079e7ab29b1.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_x64"));
        Assert.False(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void IdPrefix_DoesNotMatchLongerId()
    {
        Touch("arcface_xl-0011223344556677.installed");
        Assert.False(SentinelProbe.InstalledIn(_dir, "arcface"));
    }

    [Fact]
    public void RuntimeArtifacts_RejectZeroByteOrTruncatedFiles()
    {
        var runtime = Path.Combine(_dir, "llama.cpp");
        Directory.CreateDirectory(runtime);
        File.WriteAllBytes(Path.Combine(runtime, "llama-server.exe"), new byte[20_000]);
        File.WriteAllBytes(Path.Combine(runtime, "llama-mtmd-cli.exe"), Array.Empty<byte>());
        File.WriteAllBytes(Path.Combine(runtime, "mtmd.dll"), new byte[20_000]);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "llama_runtime_x64"));

        File.WriteAllBytes(Path.Combine(runtime, "llama-mtmd-cli.exe"), new byte[20_000]);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "llama_runtime_x64"));
    }

    [Fact]
    public void MobileClip_UsesCanonicalRegistryPath()
    {
        CreateSized(Path.Combine("mobileclip", "mobileclip_s2_image.onnx"), 1_000_000);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "mobileclip_s2"));
    }

    [Fact]
    public void RamPlus_RequiresThresholdSidecar()
    {
        CreateSized(Path.Combine("ram_plus", "ram_plus.onnx"), 1_000_000);
        CreateSized(Path.Combine("ram_plus", "ram_plus_tags.txt"), 1_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ram_plus"));
        CreateSized(Path.Combine("ram_plus", "ram_plus_thresholds.txt"), 1_000);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ram_plus"));
    }

    [Fact]
    public void CudaRuntime_RequiresMultimodalLibrary()
    {
        CreateSized(Path.Combine("llama.cpp-cuda", "llama-server.exe"), 20_000);
        CreateSized(Path.Combine("llama.cpp-cuda", "llama-mtmd-cli.exe"), 20_000);
        // Realistic sizes: cudart64_12.dll is CUDA's small runtime shim (~540 KB),
        // NOT a multi-MB lib. Using 1 MB here (as the old test did) hid the bug
        // where a 1 MB floor rejected the real, fully-installed pack.
        CreateSized(Path.Combine("llama.cpp-cuda", "cudart64_12.dll"), 540_000);
        CreateSized(Path.Combine("llama.cpp-cuda", "cublas64_12.dll"), 100_000_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "llama_runtime_cuda_x64"));
        CreateSized(Path.Combine("llama.cpp-cuda", "mtmd.dll"), 20_000);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void SmallCudaDispatchShims_AreRecognizedInstalled()
    {
        // Regression: cudart64_12.dll (~540 KB) and cudnn64_9.dll (~260 KB) are
        // small dispatch shims. A 1 MB size floor made SentinelProbe report a
        // fully-installed CUDA/cuDNN pack as missing, so the app re-dispatched
        // the prewarm forever and eventually surfaced "installer stopped
        // reporting progress".
        CreateSized(Path.Combine("llama.cpp-cuda", "llama-server.exe"), 20_000);
        CreateSized(Path.Combine("llama.cpp-cuda", "llama-mtmd-cli.exe"), 20_000);
        CreateSized(Path.Combine("llama.cpp-cuda", "mtmd.dll"), 20_000);
        CreateSized(Path.Combine("llama.cpp-cuda", "cudart64_12.dll"), 553_984);
        CreateSized(Path.Combine("llama.cpp-cuda", "cublas64_12.dll"), 100_033_536);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "llama_runtime_cuda_x64"));

        CreateSized(Path.Combine("cudnn", "bin", "cudnn64_9.dll"), 265_784);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "cudnn_runtime_x64"));
    }

    [Fact]
    public void OrtCudaPack_RequiresMathRuntimeDlls()
    {
        // The expanded CUDA pack extracts ORT + the CUDA math runtime (cudart /
        // cublas / cublasLt / cuFFT) + NVRTC under packs\cuda. The probe must
        // require every DLL registry.rs marks runtime-required, or a
        // half-extracted pack reads "installed" and the CUDA EP fails to bind.
        // Real extracts nest each archive's DLLs under its own -archive/bin dir;
        // TreeContains searches recursively, so mirror that layout here.
        var cuda = Path.Combine("packs", "cuda");
        CreateSized(Path.Combine(cuda, "ort", "lib", "onnxruntime.dll"), 5_000_000);
        CreateSized(Path.Combine(cuda, "ort", "lib", "onnxruntime_providers_cuda.dll"), 5_000_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ort_cuda_x64"));

        // cudart64_12.dll is CUDA's ~540 KB dispatch shim: above the 100 KB floor
        // but well under 1 MB. The rest are fake-small (above the floor, not real
        // multi-MB sizes) — the probe only floors at 100 KB.
        CreateSized(Path.Combine(cuda, "cudart", "bin", "cudart64_12.dll"), 553_984);
        CreateSized(Path.Combine(cuda, "cublas", "bin", "cublas64_12.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "cublas", "bin", "cublasLt64_12.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "cufft", "bin", "cufft64_11.dll"), 200_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ort_cuda_x64"));

        CreateSized(Path.Combine(cuda, "nvrtc", "bin", "nvrtc64_120_0.dll"), 200_000);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ort_cuda_x64"));
    }

    [Fact]
    public void OrtCudaPack_RejectsTruncatedMathShim()
    {
        // A truncated cudart shim (below the 100 KB floor) must not count as
        // installed even when every other required DLL is present.
        var cuda = Path.Combine("packs", "cuda");
        CreateSized(Path.Combine(cuda, "onnxruntime.dll"), 5_000_000);
        CreateSized(Path.Combine(cuda, "onnxruntime_providers_cuda.dll"), 5_000_000);
        CreateSized(Path.Combine(cuda, "cublas64_12.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "cublasLt64_12.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "cufft64_11.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "nvrtc64_120_0.dll"), 200_000);
        CreateSized(Path.Combine(cuda, "cudart64_12.dll"), 50_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "ort_cuda_x64"));
    }

    [Fact]
    public void Florence_RequiresEveryRegisteredArtifact()
    {
        CreateSized(Path.Combine("florence2", "vision_encoder.onnx"), 1_000_000);
        CreateSized(Path.Combine("florence2", "embed_tokens.onnx"), 1_000_000);
        CreateSized(Path.Combine("florence2", "encoder_model.onnx"), 1_000_000);
        CreateSized(Path.Combine("florence2", "decoder_model_merged.onnx"), 1_000_000);
        CreateSized(Path.Combine("florence2", "tokenizer.json"), 1_000);
        Assert.False(SentinelProbe.RequiredArtifactsPresentIn(_dir, "florence2_base"));
        CreateSized(Path.Combine("florence2", "config.json"), 1_000);
        Assert.True(SentinelProbe.RequiredArtifactsPresentIn(_dir, "florence2_base"));
    }

    [Fact]
    public void UnreadableDir_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn("\0invalid\0", "llama_runtime_cuda_x64"));
    }
}
