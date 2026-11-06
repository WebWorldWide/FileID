namespace FileID.Services;

/// <summary>The people Chat means by "this person" and "these people" (macOS PeopleView
/// publishSelection): the person open in People's detail view, otherwise the people checked
/// for merging. People publishes; Chat reads it into ChatContext.personIDs.</summary>
internal sealed class PersonSelectionRegistry
{
    public const int MaxPeople = 50;

    public static PersonSelectionRegistry Instance { get; } = new();

    private readonly object _gate = new();
    private long? _open;
    private long[] _checked = Array.Empty<long>();

    public void OpenPerson(long personID)
    {
        lock (_gate) _open = personID;
    }

    public void ClosePerson(long personID)
    {
        lock (_gate)
        {
            if (_open == personID) _open = null;
        }
    }

    public void SetChecked(IEnumerable<long> personIDs)
    {
        var snapshot = personIDs.ToArray();
        lock (_gate) _checked = snapshot;
    }

    public void Clear()
    {
        lock (_gate)
        {
            _open = null;
            _checked = Array.Empty<long>();
        }
    }

    public IReadOnlyList<long> Selection
    {
        get
        {
            lock (_gate) return Resolve(_open, _checked);
        }
    }

    public static IReadOnlyList<long> Resolve(long? open, IEnumerable<long> checkedIDs) =>
        open is > 0 and var person
            ? new[] { person }
            : checkedIDs.Where(id => id > 0).Distinct().Order().Take(MaxPeople).ToArray();
}
