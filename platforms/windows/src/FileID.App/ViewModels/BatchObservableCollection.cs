// BatchObservableCollection — ObservableCollection<T> with an atomic
// ReplaceAll that emits a single CollectionChanged(Reset) instead of
// Clear+N Adds (which fires N+1 events and re-runs XAML layout each
// time). Used by LibraryViewModel + others where a full-list refresh
// is the common mutation.

using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.ComponentModel;

namespace FileID.ViewModels;

internal sealed class BatchObservableCollection<T> : ObservableCollection<T>
{
    private bool _suspend;

    public void ReplaceAll(IReadOnlyList<T> next)
    {
        _suspend = true;
        try
        {
            // Use the protected base list directly so per-item events
            // don't fire while we mutate.
            Items.Clear();
            for (int i = 0; i < next.Count; i++)
            {
                Items.Add(next[i]);
            }
        }
        finally
        {
            _suspend = false;
        }
        OnPropertyChanged(new PropertyChangedEventArgs(nameof(Count)));
        OnPropertyChanged(new PropertyChangedEventArgs("Item[]"));
        OnCollectionChanged(new NotifyCollectionChangedEventArgs(NotifyCollectionChangedAction.Reset));
    }

    protected override void OnCollectionChanged(NotifyCollectionChangedEventArgs e)
    {
        if (_suspend) return;
        base.OnCollectionChanged(e);
    }

    protected override void OnPropertyChanged(PropertyChangedEventArgs e)
    {
        if (_suspend) return;
        base.OnPropertyChanged(e);
    }
}
