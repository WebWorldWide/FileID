using System.Threading;

namespace FileID.ViewModels;

internal sealed class EngineLifecycleCoordinator
{
    private readonly object _gate = new();
    private long _revision;
    private bool _shouldRun = true;
    private long? _terminalStopRevision;
    private CancellationTokenSource? _activeSuperseded;

    internal bool ShouldRun
    {
        get { lock (_gate) { return _shouldRun; } }
    }

    internal long CurrentRevision
    {
        get { lock (_gate) { return _revision; } }
    }

    internal bool TerminalStopActive
    {
        get { lock (_gate) { return _terminalStopRevision.HasValue; } }
    }

    internal EngineLifecycleIntent Begin(
        bool shouldRun,
        CancellationToken caller = default)
        => BeginCore(
            shouldRun: shouldRun,
            terminalStop: false,
            caller: caller);

    internal EngineLifecycleIntent BeginTerminalStop(
        CancellationToken caller = default)
        => BeginCore(
            shouldRun: false,
            terminalStop: true,
            caller: caller);

    private EngineLifecycleIntent BeginCore(
        bool shouldRun,
        bool terminalStop,
        CancellationToken caller)
    {
        CancellationTokenSource? previous;
        EngineLifecycleIntent intent;

        lock (_gate)
        {
            if (_terminalStopRevision.HasValue)
            {
                throw new InvalidOperationException(
                    "The application close stop cannot be superseded.");
            }
            var previousShouldRun = _shouldRun;
            previous = _activeSuperseded;
            var superseded = new CancellationTokenSource();
            var linked = CancellationTokenSource.CreateLinkedTokenSource(
                caller, superseded.Token);
            var revision = ++_revision;
            _shouldRun = shouldRun;
            if (terminalStop)
            {
                _terminalStopRevision = revision;
            }
            _activeSuperseded = superseded;
            intent = new EngineLifecycleIntent(
                this,
                revision,
                shouldRun,
                previousShouldRun,
                superseded,
                linked);
        }

        try
        {
            previous?.Cancel();
        }
        catch (ObjectDisposedException)
        {
        }
        catch (AggregateException)
        {
        }

        return intent;
    }

    internal bool ReleaseTerminalStop(long revision)
    {
        lock (_gate)
        {
            if (_terminalStopRevision != revision)
            {
                return false;
            }
            _terminalStopRevision = null;
            return true;
        }
    }

    internal bool IsTerminalStopCurrent(long revision)
    {
        lock (_gate)
        {
            return _terminalStopRevision == revision
                && _revision == revision
                && !_shouldRun;
        }
    }

    internal bool IsCurrent(long revision, bool shouldRun)
    {
        lock (_gate)
        {
            return _revision == revision && _shouldRun == shouldRun;
        }
    }

    internal void Complete(EngineLifecycleIntent intent)
    {
        lock (_gate)
        {
            if (ReferenceEquals(_activeSuperseded, intent.Superseded))
            {
                _activeSuperseded = null;
            }
        }
    }
}

internal sealed class EngineLifecycleIntent : IDisposable
{
    private readonly EngineLifecycleCoordinator _owner;
    private readonly CancellationTokenSource _linked;
    private int _disposed;

    internal EngineLifecycleIntent(
        EngineLifecycleCoordinator owner,
        long revision,
        bool shouldRun,
        bool previousShouldRun,
        CancellationTokenSource superseded,
        CancellationTokenSource linked)
    {
        _owner = owner;
        Revision = revision;
        ShouldRun = shouldRun;
        PreviousShouldRun = previousShouldRun;
        Superseded = superseded;
        _linked = linked;
    }

    internal long Revision { get; }
    internal bool ShouldRun { get; }
    internal bool PreviousShouldRun { get; }
    internal CancellationTokenSource Superseded { get; }
    internal CancellationToken Token => _linked.Token;
    internal bool IsCurrent => _owner.IsCurrent(Revision, ShouldRun);

    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0) return;
        _owner.Complete(this);
        _linked.Dispose();
        Superseded.Dispose();
    }
}
