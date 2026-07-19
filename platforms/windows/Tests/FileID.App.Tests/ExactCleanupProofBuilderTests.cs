using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ExactCleanupProofBuilderTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), $"fileid-exact-proof-{Guid.NewGuid():N}");

    public ExactCleanupProofBuilderTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    [Fact]
    public async System.Threading.Tasks.Task IdenticalVictimsProduceCompleteKeeperBoundEvidence()
    {
        var keeper = Write("keeper.bin", "same bytes");
        var victimA = Write("a.bin", "same bytes");
        var victimB = Write("b.bin", "same bytes");
        var request = Group(keeper, 1, new[] { (victimA, 2L), (victimB, 3L) });

        var proof = await ExactCleanupProofBuilder.BuildAsync(
            new[] { request }, progress: null, CancellationToken.None);

        Assert.Equal(new long[] { 2, 3 }, proof.Identities.Select(identity => identity.FileId));
        Assert.Empty(proof.Rejections);
        Assert.All(proof.Identities, identity =>
        {
            Assert.Equal(identity.SizeBytes, identity.KeeperSizeBytes);
            Assert.Equal(identity.Sha256Hex, identity.KeeperSha256Hex, ignoreCase: true);
            Assert.Equal(64, identity.Sha256Hex.Length);
            Assert.Equal(keeper, identity.KeeperPath);
        });
    }

    [Fact]
    public async System.Threading.Tasks.Task UnequalAndChangedFilesAreRejectedIndividually()
    {
        var keeper = Write("keeper.bin", "keeper!");
        var equal = Write("equal.bin", "keeper!");
        var unequal = Write("unequal.bin", "victim!!");
        var request = Group(keeper, 1, new[] { (equal, 2L), (unequal, 3L) });

        var proof = await ExactCleanupProofBuilder.BuildAsync(
            new[] { request }, progress: null, CancellationToken.None);

        Assert.Equal(2, Assert.Single(proof.Identities).FileId);
        Assert.Equal(3, Assert.Single(proof.Rejections).FileId);
    }

    [Fact]
    public async System.Threading.Tasks.Task MissingKeeperRejectsEveryVictim()
    {
        var missing = Path.Combine(_root, "missing.bin");
        var a = Write("a.bin", "same");
        var b = Write("b.bin", "same");
        var request = Group(missing, 1, new[] { (a, 2L), (b, 3L) }, expectedSize: 4);

        var proof = await ExactCleanupProofBuilder.BuildAsync(
            new[] { request }, progress: null, CancellationToken.None);

        Assert.Empty(proof.Identities);
        Assert.Equal(new long[] { 2, 3 }, proof.Rejections.Select(rejection => rejection.FileId));
    }

    [Fact]
    public void KeeperIsHashedOnceForMultipleVictims()
    {
        var calls = new Dictionary<long, int>();
        var request = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "keeper", 4),
            new[]
            {
                new ExactCleanupFile(2, "a", 4),
                new ExactCleanupFile(3, "b", 4),
                new ExactCleanupFile(4, "c", 4),
            });

        var proof = ExactCleanupProofBuilder.BuildCore(new[] { request }, (file, _) =>
        {
            calls[file.FileId] = calls.GetValueOrDefault(file.FileId) + 1;
            return new string('A', 64);
        });

        Assert.Equal(1, calls[1]);
        Assert.Equal(3, proof.Identities.Count);
    }

    [Fact]
    public void AuthorizationCapAndOverflowFailBeforeHashing()
    {
        var calls = 0;
        string Hash(ExactCleanupFile _, CancellationToken __)
        {
            calls++;
            return new string('A', 64);
        }
        var half = ExactCleanupProofBuilder.MaxAuthorizationBytes / 2;
        var atCap = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "keeper", half),
            new[] { new ExactCleanupFile(2, "victim", half) });
        Assert.Single(ExactCleanupProofBuilder.BuildCore(new[] { atCap }, Hash).Identities);
        Assert.Equal(2, calls);

        calls = 0;
        var overCap = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "keeper", half),
            new[] { new ExactCleanupFile(2, "victim", half + 1) });
        Assert.Throws<InvalidOperationException>(() =>
            ExactCleanupProofBuilder.BuildCore(new[] { overCap }, Hash));
        Assert.Equal(0, calls);

        var overflow = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "keeper", 1),
            new[] { new ExactCleanupFile(2, "victim", long.MaxValue) });
        Assert.Throws<OverflowException>(() =>
            ExactCleanupProofBuilder.BuildCore(new[] { overflow }, Hash));
        Assert.Equal(0, calls);
    }

    [Fact]
    public void SelectedKeeperAndCancellationFailBeforeEvidence()
    {
        var selectedKeeper = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "same", 4),
            new[] { new ExactCleanupFile(2, "same", 4) });
        Assert.Throws<InvalidOperationException>(() =>
            ExactCleanupProofBuilder.BuildCore(
                new[] { selectedKeeper },
                (_, _) => new string('A', 64)));

        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        var normal = new ExactCleanupGroupRequest(
            new ExactCleanupFile(1, "keeper", 4),
            new[] { new ExactCleanupFile(2, "victim", 4) });
        Assert.Throws<OperationCanceledException>(() =>
            ExactCleanupProofBuilder.BuildCore(
                new[] { normal },
                (_, _) => new string('A', 64),
                cancellationToken: cancellation.Token));
    }

    [Fact]
    public void ExactCommandDerivesCompleteUniqueIdSet()
    {
        var identities = new[]
        {
            Identity(2, "a"),
            Identity(3, "b"),
        };
        var command = EngineClient.CreateExactTrashCommand(identities);
        Assert.Equal(new long[] { 2, 3 }, command.FileIds);
        Assert.Same(identities[0], command.ExactIdentities![0]);

        Assert.Throws<ArgumentException>(() =>
            EngineClient.CreateExactTrashCommand(Array.Empty<ExactTrashIdentity>()));
        Assert.Throws<ArgumentException>(() =>
            EngineClient.CreateExactTrashCommand(new[] { Identity(2, "a"), Identity(2, "b") }));
    }

    private string Write(string name, string value)
    {
        var path = Path.Combine(_root, name);
        File.WriteAllText(path, value);
        return path;
    }

    private static ExactCleanupGroupRequest Group(
        string keeperPath,
        long keeperId,
        IReadOnlyList<(string Path, long Id)> victims,
        long? expectedSize = null)
    {
        var size = expectedSize ?? new FileInfo(keeperPath).Length;
        return new ExactCleanupGroupRequest(
            new ExactCleanupFile(keeperId, keeperPath, size),
            victims.Select(victim => new ExactCleanupFile(
                victim.Id,
                victim.Path,
                new FileInfo(victim.Path).Length)).ToArray());
    }

    private static ExactTrashIdentity Identity(long id, string path) =>
        new(id, path, 4, new string('A', 64), "keeper", 4, new string('A', 64));
}
