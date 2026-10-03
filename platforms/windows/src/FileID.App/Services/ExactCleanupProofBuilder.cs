using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;
using FileID.IpcSchema;

namespace FileID.Services;

internal sealed record ExactCleanupFile(long FileId, string Path, long SizeBytes);

internal sealed record ExactCleanupGroupRequest(
    ExactCleanupFile Keeper,
    IReadOnlyList<ExactCleanupFile> Victims);

internal sealed record ExactCleanupRejection(long FileId, string Reason);

internal sealed record ExactCleanupProgress(int CompletedFiles, int TotalFiles);

internal sealed record ExactCleanupProof(
    IReadOnlyList<ExactTrashIdentity> Identities,
    IReadOnlyList<ExactCleanupRejection> Rejections,
    long AuthorizationBytes);

internal static class ExactCleanupProofBuilder
{
    internal const int MaxVictims = 5_000;
    internal const long MaxAuthorizationBytes = 64L * 1024 * 1024 * 1024;
    private const int BufferBytes = 1024 * 1024;

    internal static Task<ExactCleanupProof> BuildAsync(
        IReadOnlyList<ExactCleanupGroupRequest> groups,
        IProgress<ExactCleanupProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(groups);
        var snapshot = groups
            .Select(group => new ExactCleanupGroupRequest(group.Keeper, group.Victims.ToArray()))
            .ToArray();
        return Task.Run(() =>
        {
            var buffer = new byte[BufferBytes];
            return BuildCore(
                snapshot,
                (file, token) => HashFile(file, buffer, token),
                progress,
                cancellationToken);
        }, cancellationToken);
    }

    internal static ExactCleanupProof BuildCore(
        IReadOnlyList<ExactCleanupGroupRequest> groups,
        Func<ExactCleanupFile, CancellationToken, string> hashFile,
        IProgress<ExactCleanupProgress>? progress = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(groups);
        ArgumentNullException.ThrowIfNull(hashFile);

        var victimIds = new HashSet<long>();
        var victimPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        long authorizationBytes = 0;
        var victimCount = 0;
        foreach (var group in groups)
        {
            ValidateFile(group.Keeper, "keeper");
            foreach (var victim in group.Victims)
            {
                ValidateFile(victim, "victim");
                if (!victimIds.Add(victim.FileId))
                {
                    throw new InvalidOperationException($"Exact Cleanup contains duplicate file ID {victim.FileId}.");
                }
                if (!victimPaths.Add(victim.Path))
                {
                    throw new InvalidOperationException("Exact Cleanup contains the same victim path more than once.");
                }
                victimCount = checked(victimCount + 1);
                authorizationBytes = checked(authorizationBytes + victim.SizeBytes);
                authorizationBytes = checked(authorizationBytes + group.Keeper.SizeBytes);
            }
        }
        if (victimCount == 0)
        {
            return new ExactCleanupProof(Array.Empty<ExactTrashIdentity>(), Array.Empty<ExactCleanupRejection>(), 0);
        }
        if (victimCount > MaxVictims)
        {
            throw new InvalidOperationException(
                $"Exact Cleanup is limited to {MaxVictims:N0} selected files per operation.");
        }
        if (authorizationBytes > MaxAuthorizationBytes)
        {
            throw new InvalidOperationException(
                "Exact Cleanup proof exceeds the 64 GiB verification limit. Select fewer or smaller groups.");
        }
        foreach (var group in groups)
        {
            if (victimPaths.Contains(group.Keeper.Path))
            {
                throw new InvalidOperationException(
                    "An Exact Cleanup keeper is also selected for deletion. Choose an unselected keeper in every group.");
            }
        }

        var identities = new List<ExactTrashIdentity>(victimCount);
        var rejections = new List<ExactCleanupRejection>();
        var totalFiles = groups.Count(group => group.Victims.Count > 0) + victimCount;
        var completedFiles = 0;
        foreach (var group in groups)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (group.Victims.Count == 0) continue;

            string keeperHash;
            try
            {
                keeperHash = hashFile(group.Keeper, cancellationToken);
                progress?.Report(new ExactCleanupProgress(++completedFiles, totalFiles));
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch (Exception ex)
            {
                foreach (var victim in group.Victims)
                {
                    rejections.Add(new ExactCleanupRejection(
                        victim.FileId,
                        $"The keeper could not be byte-verified: {ex.Message}"));
                }
                continue;
            }

            foreach (var victim in group.Victims)
            {
                cancellationToken.ThrowIfCancellationRequested();
                try
                {
                    var victimHash = hashFile(victim, cancellationToken);
                    progress?.Report(new ExactCleanupProgress(++completedFiles, totalFiles));
                    if (!string.Equals(victimHash, keeperHash, StringComparison.OrdinalIgnoreCase))
                    {
                        rejections.Add(new ExactCleanupRejection(
                            victim.FileId,
                            "The file no longer matches its selected keeper."));
                        continue;
                    }
                    identities.Add(new ExactTrashIdentity(
                        victim.FileId,
                        victim.Path,
                        victim.SizeBytes,
                        victimHash,
                        group.Keeper.Path,
                        group.Keeper.SizeBytes,
                        keeperHash));
                }
                catch (OperationCanceledException)
                {
                    throw;
                }
                catch (Exception ex)
                {
                    rejections.Add(new ExactCleanupRejection(
                        victim.FileId,
                        $"The file could not be byte-verified: {ex.Message}"));
                }
            }
        }
        return new ExactCleanupProof(identities, rejections, authorizationBytes);
    }

    internal static TimeSpan EngineTimeout(long authorizationBytes)
    {
        var readSeconds = authorizationBytes / (10.0 * 1024 * 1024);
        return TimeSpan.FromSeconds(Math.Clamp(120.0 + readSeconds, 120.0, 7_200.0));
    }

    private static void ValidateFile(ExactCleanupFile file, string role)
    {
        if (file.FileId <= 0 || string.IsNullOrWhiteSpace(file.Path) || file.SizeBytes < 0)
        {
            throw new InvalidOperationException($"Exact Cleanup contains an invalid {role} snapshot.");
        }
    }

    private static string HashFile(
        ExactCleanupFile file,
        byte[] buffer,
        CancellationToken cancellationToken)
    {
        using var stream = new FileStream(
            file.Path,
            FileMode.Open,
            FileAccess.Read,
            FileShare.Read | FileShare.Delete,
            BufferBytes,
            FileOptions.SequentialScan);
        if (stream.Length != file.SizeBytes)
        {
            throw new InvalidDataException("The file size changed before verification.");
        }
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        long totalRead = 0;
        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var read = stream.Read(buffer, 0, buffer.Length);
            if (read == 0) break;
            totalRead = checked(totalRead + read);
            if (totalRead > file.SizeBytes)
            {
                throw new InvalidDataException("The file grew during verification.");
            }
            hash.AppendData(buffer, 0, read);
        }
        if (totalRead != file.SizeBytes || stream.Length != file.SizeBytes)
        {
            throw new InvalidDataException("The file size changed during verification.");
        }
        return Convert.ToHexString(hash.GetHashAndReset());
    }
}
