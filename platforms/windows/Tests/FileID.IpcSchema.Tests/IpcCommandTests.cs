// Round-trip tests for IpcCommand. Asserts:
//   1. Each variant survives encode → decode → encode without semantic loss
//      (the resulting payload's structure matches).
//   2. The wire bytes for empty-payload variants are `{"variantName": {}}`,
//      NOT a bare string.
//   3. The wire bytes for the breaking-change `startScan(rootPath, rootDisplay)`
//      payload match the schema (no `rootBookmark` field; rootDisplay is null
//      when omitted, not absent).

using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class IpcCommandTests
{
    [Fact]
    public void StartScan_WithRootDisplay_RoundTrips()
    {
        var cmd = new IpcCommand("test-1", new StartScanCommand(@"C:\Users\adam\Pictures", "Pictures"));
        var json = IpcCoder.Encode(cmd);

        Assert.Contains("\"startScan\"", json);
        Assert.Contains("\"rootPath\":\"C:\\\\Users\\\\adam\\\\Pictures\"", json);
        Assert.Contains("\"rootDisplay\":\"Pictures\"", json);
        Assert.DoesNotContain("rootBookmark", json);

        var roundTripped = IpcCoder.Decode<IpcCommand>(json);
        Assert.Equal("test-1", roundTripped.Id);
        var p = Assert.IsType<StartScanCommand>(roundTripped.Payload);
        Assert.Equal(@"C:\Users\adam\Pictures", p.RootPath);
        Assert.Equal("Pictures", p.RootDisplay);
    }

    [Fact]
    public void StartScan_WithoutRootDisplay_EncodesNull()
    {
        // When the C# field is null, the wire shape encodes "rootDisplay":null
        // (Swift Codable does the same for optionals it bothered to encode).
        // We could omit instead via DefaultIgnoreCondition.WhenWritingNull,
        // but matching Swift's behavior keeps round-trips byte-equal.
        var cmd = new IpcCommand("t", new StartScanCommand("/abs/path", null));
        var json = IpcCoder.Encode(cmd);
        Assert.Contains("\"rootDisplay\":null", json);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<StartScanCommand>(rt.Payload);
        Assert.Null(p.RootDisplay);
    }

    [Theory]
    [InlineData(typeof(PauseScanCommand),         "pauseScan")]
    [InlineData(typeof(ResumeScanCommand),        "resumeScan")]
    [InlineData(typeof(CancelScanCommand),        "cancelScan")]
    [InlineData(typeof(RequestStatusCommand),     "requestStatus")]
    [InlineData(typeof(ShutdownCommand),          "shutdown")]
    [InlineData(typeof(RunFaceClusteringCommand), "runFaceClustering")]
    [InlineData(typeof(DeepAnalyzeCancelCommand), "deepAnalyzeCancel")]
    [InlineData(typeof(CancelPrewarmCommand),     "cancelPrewarm")]
    public void EmptyPayloadVariants_EncodeAsObjectNotString(Type t, string expectedKey)
    {
        var payload = (CommandPayload)Activator.CreateInstance(t)!;
        var cmd = new IpcCommand("e", payload);
        var json = IpcCoder.Encode(cmd);
        Assert.Contains($"\"{expectedKey}\":{{}}", json);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        Assert.IsType(t, rt.Payload);
    }

    [Fact]
    public void DeepAnalyzeFile_PreservesFileIDExactCasing()
    {
        // Field name on the wire is "fileID" (matches Swift Codable's
        // synthesis for `fileID: Int64`). Lower-case "fileId" is wrong.
        var cmd = new IpcCommand("d", new DeepAnalyzeFileCommand(12345, "qwen2_5_vl_3b"));
        var json = IpcCoder.Encode(cmd);
        Assert.Contains("\"fileID\":12345", json);
        Assert.DoesNotContain("\"fileId\"", json);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<DeepAnalyzeFileCommand>(rt.Payload);
        Assert.Equal(12345, p.FileId);
        Assert.Equal("qwen2_5_vl_3b", p.ModelKind);
    }

    [Fact]
    public void DeepAnalyzeAll_RoundTrips()
    {
        var cmd = new IpcCommand("a", new DeepAnalyzeAllCommand("qwen2_5_vl_7b", SkipExisting: true));
        var json = IpcCoder.Encode(cmd);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<DeepAnalyzeAllCommand>(rt.Payload);
        Assert.Equal("qwen2_5_vl_7b", p.ModelKind);
        Assert.True(p.SkipExisting);
    }

    [Fact]
    public void UnknownVariant_ThrowsJsonException()
    {
        const string bad = """{"id":"x","payload":{"definitelyNotAVariant":{}}}""";
        Assert.Throws<JsonException>(() => IpcCoder.Decode<IpcCommand>(bad));
    }

    [Fact]
    public void Frame_TerminatesWithSingleNewline()
    {
        var cmd = new IpcCommand("f", new ShutdownCommand());
        var bytes = IpcCoder.EncodeLine(cmd);
        Assert.Equal((byte)'\n', bytes[^1]);
        // No embedded newlines (would corrupt the wire).
        for (int i = 0; i < bytes.Length - 1; i++)
        {
            Assert.NotEqual((byte)'\n', bytes[i]);
        }
    }
}
