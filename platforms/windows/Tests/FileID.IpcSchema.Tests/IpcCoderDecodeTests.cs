// Decode tests for IpcCoder — guards the span-based Decode<T> overload that
// dropped the per-frame .ToString() copy. Behavior must stay identical: a frame
// with or without a trailing newline decodes to the same value, and the
// trailing-newline tolerance is preserved.

using Xunit;

namespace FileID.IpcSchema.Tests;

public class IpcCoderDecodeTests
{
    [Fact]
    public void Decode_FrameWithoutTrailingNewline_RoundTrips()
    {
        var ev = IpcEvent.Now(new DiscoveryCompleteEvent(12345));
        var json = IpcCoder.Encode(ev); // Encode strips the newline.
        Assert.DoesNotContain('\n', json);

        var rt = IpcCoder.Decode<IpcEvent>(json);
        var dc = Assert.IsType<DiscoveryCompleteEvent>(rt.Payload);
        Assert.Equal(12345ul, dc.TotalFiles);
    }

    [Fact]
    public void Decode_FrameWithTrailingNewline_RoundTrips()
    {
        var ev = IpcEvent.Now(new DiscoveryCompleteEvent(777));
        var line = System.Text.Encoding.UTF8.GetString(IpcCoder.EncodeLine(ev)); // has '\n'
        Assert.EndsWith("\n", line);

        var rt = IpcCoder.Decode<IpcEvent>(line);
        var dc = Assert.IsType<DiscoveryCompleteEvent>(rt.Payload);
        Assert.Equal(777ul, dc.TotalFiles);
    }

    [Fact]
    public void Decode_WithOrWithoutNewline_ProducesEqualResult()
    {
        var info = new EngineInfo("9.9.9", 4242, 8, 32.0);
        var ev = IpcEvent.Now(new ReadyEvent(info));
        var noNewline = IpcCoder.Encode(ev);
        var withNewline = noNewline + "\n";

        var a = IpcCoder.Decode<IpcEvent>(noNewline);
        var b = IpcCoder.Decode<IpcEvent>(withNewline);

        var ra = Assert.IsType<ReadyEvent>(a.Payload);
        var rb = Assert.IsType<ReadyEvent>(b.Payload);
        Assert.Equal(ra.Info.Version, rb.Info.Version);
        Assert.Equal(ra.Info.Pid, rb.Info.Pid);
        Assert.Equal(ra.Info.WorkerCap, rb.Info.WorkerCap);
        Assert.Equal(ra.Info.PhysicalMemoryGB, rb.Info.PhysicalMemoryGB);
    }

    [Fact]
    public void Decode_MultipleTrailingNewlines_AreTolerated()
    {
        var ev = IpcEvent.Now(new DiscoveryCompleteEvent(1));
        var json = IpcCoder.Encode(ev) + "\n\n";
        var rt = IpcCoder.Decode<IpcEvent>(json);
        var dc = Assert.IsType<DiscoveryCompleteEvent>(rt.Payload);
        Assert.Equal(1ul, dc.TotalFiles);
    }
}
