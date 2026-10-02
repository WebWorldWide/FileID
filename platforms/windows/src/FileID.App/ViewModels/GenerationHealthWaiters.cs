using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace FileID.ViewModels;

internal sealed class GenerationHealthWaiters
{
    internal sealed class Waiter
    {
        internal Waiter(int generation, int pid) { Generation = generation; Pid = pid; }
        internal int Generation { get; }
        internal int Pid { get; }
        internal TaskCompletionSource Completion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Task => Completion.Task;
    }

    private readonly object _gate = new();
    private readonly Dictionary<string, Waiter> _pending = new(StringComparer.Ordinal);
    private readonly HashSet<int> _retired = new();

    internal int Count { get { lock (_gate) return _pending.Count; } }

    internal Waiter Register(string nonce, int generation, int pid)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(nonce);
        ArgumentOutOfRangeException.ThrowIfNegative(generation);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(pid);
        var waiter = new Waiter(generation, pid);
        lock (_gate)
        {
            if (_retired.Contains(generation))
                waiter.Completion.TrySetException(new InvalidOperationException("The engine generation has retired."));
            else if (!_pending.TryAdd(nonce, waiter))
                throw new InvalidOperationException("A health check with this nonce already exists.");
        }
        return waiter;
    }

    internal bool TryResolve(string nonce, int pid, int generation)
    {
        if (string.IsNullOrEmpty(nonce)) return false;
        Waiter waiter;
        lock (_gate)
        {
            if (!_pending.TryGetValue(nonce, out waiter!) || waiter.Pid != pid || waiter.Generation != generation) return false;
            _pending.Remove(nonce);
        }
        return waiter.Completion.TrySetResult();
    }

    internal bool TryFail(string nonce, Exception exception)
    {
        if (string.IsNullOrEmpty(nonce)) return false;
        Waiter waiter;
        lock (_gate)
        {
            if (!_pending.Remove(nonce, out waiter!)) return false;
        }
        return waiter.Completion.TrySetException(exception);
    }

    internal int FailGeneration(int generation, Exception exception)
    {
        var victims = new List<KeyValuePair<string, Waiter>>();
        lock (_gate)
        {
            _retired.Add(generation);
            foreach (var entry in _pending)
                if (entry.Value.Generation == generation) victims.Add(entry);
            foreach (var entry in victims) _pending.Remove(entry.Key);
        }
        foreach (var entry in victims) entry.Value.Completion.TrySetException(exception);
        return victims.Count;
    }
}
