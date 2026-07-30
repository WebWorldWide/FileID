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

    private static readonly string[] s_excludedPair = { @"C:\pics\raw", @"C:\pics\tmp" };
    private static readonly string[] s_excludedSingle = { @"C:\pics\raw" };

    [Fact]
    public void StartScan_ExcludedPaths_RoundTripsAndLegacyJsonDecodes()
    {
        // Populated list round-trips verbatim.
        var cmd = new IpcCommand("t-ex", new StartScanCommand(
            @"C:\pics", null, Rescan: true, ExcludedPaths: s_excludedPair));
        var json = IpcCoder.Encode(cmd);
        Assert.Contains("\"excludedPaths\":[", json);
        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<StartScanCommand>(rt.Payload);
        Assert.NotNull(p.ExcludedPaths);
        Assert.Equal(s_excludedPair, p.ExcludedPaths);

        // Legacy JSON without the key (pre-exclusions engine/app) decodes to null.
        const string legacy = "{\"id\":\"t-old\",\"payload\":{\"startScan\":{\"rootPath\":\"C:\\\\pics\",\"rootDisplay\":null,\"rescan\":false}}}";
        var old = IpcCoder.Decode<IpcCommand>(legacy);
        var op = Assert.IsType<StartScanCommand>(old.Payload);
        Assert.Null(op.ExcludedPaths);
    }

    [Fact]
    public void PurgeExcluded_RoundTrips()
    {
        var cmd = new IpcCommand("t-purge", new PurgeExcludedCommand(s_excludedSingle));
        var json = IpcCoder.Encode(cmd);
        Assert.Contains("\"purgeExcluded\"", json);
        Assert.Contains("\"excludedPaths\":[\"C:\\\\pics\\\\raw\"]", json);
        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<PurgeExcludedCommand>(rt.Payload);
        Assert.Equal(s_excludedSingle, p.ExcludedPaths);
    }

    [Theory]
    [InlineData(typeof(PauseScanCommand), "pauseScan")]
    [InlineData(typeof(ResumeScanCommand), "resumeScan")]
    [InlineData(typeof(CancelScanCommand), "cancelScan")]
    [InlineData(typeof(CancelRestructureCommand), "cancelRestructure")]
    [InlineData(typeof(RequestStatusCommand), "requestStatus")]
    [InlineData(typeof(ShutdownCommand), "shutdown")]
    [InlineData(typeof(RunFaceClusteringCommand), "runFaceClustering")]
    [InlineData(typeof(DeepAnalyzeCancelCommand), "deepAnalyzeCancel")]
    [InlineData(typeof(VerifyCudaPackCommand), "verifyCudaPack")]
    public void EmptyPayloadVariants_EncodeAsObjectNotString(Type t, string expectedKey)
    {
        var payload = (CommandPayload)Activator.CreateInstance(t)!;
        var cmd = new IpcCommand("e", payload);
        var json = IpcCoder.Encode(cmd);
        Assert.Contains($"\"{expectedKey}\":{{}}", json);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        Assert.IsType(t, rt.Payload);
    }

    // cancelPrewarm gained an optional modelKind (per-model cancel; null = all),
    // so it is no longer an empty payload — it round-trips its field both ways.
    [Fact]
    public void CancelPrewarm_RoundTripsWithAndWithoutModelKind()
    {
        var all = new IpcCommand("c", new CancelPrewarmCommand());
        var allJson = IpcCoder.Encode(all);
        Assert.Contains("\"cancelPrewarm\":{", allJson);
        var allCmd = Assert.IsType<CancelPrewarmCommand>(IpcCoder.Decode<IpcCommand>(allJson).Payload);
        Assert.Null(allCmd.ModelKind);

        var one = new IpcCommand("c", new CancelPrewarmCommand("clip_text"));
        var oneJson = IpcCoder.Encode(one);
        Assert.Contains("\"modelKind\":\"clip_text\"", oneJson);
        var oneCmd = Assert.IsType<CancelPrewarmCommand>(IpcCoder.Decode<IpcCommand>(oneJson).Payload);
        Assert.Equal("clip_text", oneCmd.ModelKind);
    }

    [Fact]
    public void HealthCheck_RoundTripsWithExactRequestIDCasing()
    {
        var cmd = new IpcCommand("health-envelope", new HealthCheckCommand("health-request"));
        var json = IpcCoder.Encode(cmd);

        Assert.Contains("\"healthCheck\":{\"requestID\":\"health-request\"}", json);
        Assert.DoesNotContain("\"requestId\"", json);

        var payload = Assert.IsType<HealthCheckCommand>(IpcCoder.Decode<IpcCommand>(json).Payload);
        Assert.Equal("health-request", payload.RequestId);
    }

    [Fact]
    public void UndoRestructure_ShortcutTokenRoundTripsWithoutChangingLegacyShape()
    {
        const string token = "6f5ed615-fbb2-41e2-86e5-d4bb9d84d851";
        var tokenized = new IpcCommand(
            "undo-shortcuts",
            new UndoRestructureCommand(@"F:\Adlon Drive", token));
        var tokenizedJson = IpcCoder.Encode(tokenized);
        Assert.Contains("\"shortcutUndoToken\":\"" + token + "\"", tokenizedJson);
        var payload = Assert.IsType<UndoRestructureCommand>(
            IpcCoder.Decode<IpcCommand>(tokenizedJson).Payload);
        Assert.Equal(token, payload.ShortcutUndoToken);

        var legacyJson = IpcCoder.Encode(new IpcCommand(
            "undo-moves",
            new UndoRestructureCommand(@"F:\Adlon Drive")));
        Assert.DoesNotContain("shortcutUndoToken", legacyJson);
        Assert.Null(Assert.IsType<UndoRestructureCommand>(
            IpcCoder.Decode<IpcCommand>(legacyJson).Payload).ShortcutUndoToken);
    }

    [Fact]
    public void DeepAnalyzeFile_PreservesFileIDExactCasing()
    {
        // Field name on the wire is "fileID" (matches Swift Codable's
        // synthesis for `fileID: Int64`). Lower-case "fileId" is wrong.
        var cmd = new IpcCommand("d", new DeepAnalyzeFileCommand(12345, "qwen2_5_vl_7b"));
        var json = IpcCoder.Encode(cmd);
        Assert.Contains("\"fileID\":12345", json);
        Assert.DoesNotContain("\"fileId\"", json);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<DeepAnalyzeFileCommand>(rt.Payload);
        Assert.Equal(12345, p.FileId);
        Assert.Equal("qwen2_5_vl_7b", p.ModelKind);
    }

    [Fact]
    public void DeepAnalyzeAll_RoundTrips()
    {
        var cmd = new IpcCommand("a", new DeepAnalyzeAllCommand("qwen2_5_vl_7b", SkipExisting: true, TagsOnly: true));
        var json = IpcCoder.Encode(cmd);

        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<DeepAnalyzeAllCommand>(rt.Payload);
        Assert.Equal("qwen2_5_vl_7b", p.ModelKind);
        Assert.True(p.SkipExisting);
        Assert.True(p.TagsOnly);
    }

    [Fact]
    public void DeepAnalyzeAll_OmittedTagsOnly_DefaultsFalse()
    {
        // Older clients omit tagsOnly; it must decode as false (serde default
        // on the Rust side, defaulted record param on the C# side).
        const string json = """{"id":"a","payload":{"deepAnalyzeAll":{"modelKind":"qwen2_5_vl_7b","skipExisting":false}}}""";
        var rt = IpcCoder.Decode<IpcCommand>(json);
        var p = Assert.IsType<DeepAnalyzeAllCommand>(rt.Payload);
        Assert.False(p.TagsOnly);
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
