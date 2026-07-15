using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class ThumbnailResourceBoundsTests
{
    [Fact]
    public async Task FallbackReaderRejectsOversizedSparseFileBeforeAllocation()
    {
        var path = Path.Combine(Path.GetTempPath(), $"fileid-thumb-bound-{Guid.NewGuid():N}.tiff");
        try
        {
            await using (var stream = File.Create(path))
            {
                stream.SetLength(ThumbnailService.MaxFallbackEncodedBytes + 1);
            }

            await Assert.ThrowsAsync<InvalidDataException>(() =>
                ThumbnailService.ReadFallbackFileBytesAsync(path, CancellationToken.None));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public async Task FallbackReaderReturnsSmallFileExactly()
    {
        var path = Path.Combine(Path.GetTempPath(), $"fileid-thumb-small-{Guid.NewGuid():N}.jpg");
        var expected = new byte[] { 0xFF, 0xD8, 0xFF, 0xD9 };
        try
        {
            await File.WriteAllBytesAsync(path, expected);
            var actual = await ThumbnailService.ReadFallbackFileBytesAsync(
                path,
                CancellationToken.None);
            Assert.Equal(expected, actual);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void QueueCapacityIsFiniteAndPositive()
    {
        Assert.InRange(ThumbnailService.QueueCapacity, 1, 1024);
    }
    /// A burst larger than the queue capacity must complete EVERY request:
    /// the newest requests stay queued for real work and the evicted oldest
    /// are placeholder-completed by the drop callback — none may hang in the
    /// shimmer state, which is the failure the original unbounded design's
    /// comment warned about. (audit 2026-07-14)
    [Fact]
    public async Task BurstBeyondCapacityCompletesEveryRequestAndKeepsNewest()
    {
        var channel = ThumbnailService.CreateRequestChannel();
        const int burst = 300;
        var requests = new List<ThumbnailService.ThumbnailRequest>(burst);
        for (var i = 0; i < burst; i++)
        {
            var tcs = new TaskCompletionSource<Microsoft.UI.Xaml.Media.Imaging.BitmapImage?>(
                TaskCreationOptions.RunContinuationsAsynchronously);
            var req = new ThumbnailService.ThumbnailRequest($"p{i}", null, tcs, CancellationToken.None);
            requests.Add(req);
            Assert.True(channel.Writer.TryWrite(req), "DropOldest admission must always accept");
        }

        var dropped = burst - ThumbnailService.QueueCapacity;
        for (var i = 0; i < dropped; i++)
        {
            var completed = await Task.WhenAny(
                requests[i].Completion.Task,
                Task.Delay(TimeSpan.FromSeconds(5)));
            Assert.Same(requests[i].Completion.Task, completed);
            Assert.Null(await requests[i].Completion.Task);
        }

        var kept = 0;
        while (channel.Reader.TryRead(out var queued))
        {
            Assert.False(queued.Completion.Task.IsCompleted, "queued requests must still be live work");
            kept++;
        }
        Assert.Equal(ThumbnailService.QueueCapacity, kept);
    }
}

