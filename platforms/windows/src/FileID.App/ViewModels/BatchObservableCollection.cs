// ObservableCollection with ReplaceAll (one Reset instead of Clear plus N Adds, which re-runs XAML layout each time)
// and AppendRange (one multi-item Add, so an ItemsRepeater keeps its realized elements; a Reset would reload every visible thumbnail).

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

    public void AppendRange(IReadOnlyList<T> more)
    {
        if (more.Count == 0) return;
        int start = Items.Count;
        _suspend = true;
        try
        {
            for (int i = 0; i < more.Count; i++)
            {
                Items.Add(more[i]);
            }
        }
        finally
        {
            _suspend = false;
        }
        OnPropertyChanged(new PropertyChangedEventArgs(nameof(Count)));
        OnPropertyChanged(new PropertyChangedEventArgs("Item[]"));
        OnCollectionChanged(new NotifyCollectionChangedEventArgs(
            NotifyCollectionChangedAction.Add, (System.Collections.IList)new List<T>(more), start));
    }

    public void PrependRange(IReadOnlyList<T> more)
    {
        if (more.Count == 0) return;
        _suspend = true;
        try
        {
            for (int i = 0; i < more.Count; i++)
            {
                Items.Insert(i, more[i]);
            }
        }
        finally
        {
            _suspend = false;
        }
        OnPropertyChanged(new PropertyChangedEventArgs(nameof(Count)));
        OnPropertyChanged(new PropertyChangedEventArgs("Item[]"));
        OnCollectionChanged(new NotifyCollectionChangedEventArgs(
            NotifyCollectionChangedAction.Add, (System.Collections.IList)new List<T>(more), 0));
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
