using System.Threading;

namespace FileID.Services;

internal sealed class GenerationOwnedOperationSlot<TPayload> where TPayload : class
{
    internal sealed record Owner(long AttemptId, int Generation, long Revision, TPayload Payload);

    private Owner? _owner;
    private long _nextAttemptId;

    internal Owner? Current => Volatile.Read(ref _owner);

    internal bool TryReserve(int generation, long revision, TPayload payload, out Owner owner)
    {
        owner = new Owner(Interlocked.Increment(ref _nextAttemptId), generation, revision, payload);
        return Interlocked.CompareExchange(ref _owner, owner, null) is null;
    }

    internal bool Release(Owner owner) =>
        ReferenceEquals(Interlocked.CompareExchange(ref _owner, null, owner), owner);

    internal Owner? ReleaseGeneration(int generation)
    {
        while (true)
        {
            var owner = Volatile.Read(ref _owner);
            if (owner is null || owner.Generation != generation) return null;
            if (ReferenceEquals(Interlocked.CompareExchange(ref _owner, null, owner), owner))
            {
                return owner;
            }
        }
    }
}
