// SuggestedMergesSheet code-behind. Binds EngineClient's
// LastMergeSuggestions to an ItemsRepeater via a DataTemplate over
// MergeSuggestionVm (see PeopleViewModel). Each row shows side-by-side anchor
// face JPEGs + similarity % + action buttons. Merge fires mergeClusters IPC;
// Different-people writes a face_verifications row so we don't keep
// re-suggesting it.
//
// Rendering is data-template-driven (not imperative UIElement construction):
// the template resolves {ThemeResource} brushes natively and the ItemsRepeater
// recycles containers, so we never index theme brushes off
// Application.Resources (KeyNotFoundException) nor rebuild sibling UIElement
// subtrees per engine event (layout-pass fast-fail) — the two crash shapes the
// prior imperative BuildRow/BuildFaceImage path hit. See platforms/windows/
// CLAUDE.md (V15.x DispatcherObject / ItemsRepeater notes).

using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Threading.Tasks;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.People;

public sealed partial class SuggestedMergesSheet : UserControl
{
    private readonly ObservableCollection<MergeSuggestionVm> _rows = new();
    private bool _unloaded;

    public SuggestedMergesSheet()
    {
        InitializeComponent();
        // subscribe in ctor (not Loaded). ContentDialog hosts
        // don't reliably fire Loaded; the WelcomeSheet hit the same wall.
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += OnUnloaded;
        Loaded += async (_, _) =>
        {
            // Trigger a fresh suggestion fetch whenever the sheet opens.
            // Awaited so engine-not-ready exceptions surface to the log
            // rather than silently leaving the sheet on "Looking for…".
            HeaderText.Text = "Looking for similar clusters…";
            try
            {
                await EngineClient.Instance.FindMergeSuggestionsAsync();
            }
            catch (Exception ex)
            {
                Services.DebugLog.Error($"FindMergeSuggestions failed: {ex.Message}");
                HeaderText.Text = "Couldn't fetch suggestions — see logs.";
            }
        };
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        // Guard any dispatcher tick that fires after the ContentDialog closes
        // (Render would otherwise touch a torn-down sheet), and drop the
        // engine subscription.
        _unloaded = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("SuggestedMergesSheet.OnEngineChanged", () =>
        {
            if (e.PropertyName != nameof(EngineClient.LastMergeSuggestions)) return;
            Services.DebugLog.Debug($"[ENGINE-SUB:SuggestedMergesSheet] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(Render);
        });

    private void Render()
    {
        if (_unloaded) return;
        var sug = EngineClient.Instance.LastMergeSuggestions;
        _rows.Clear();
        if (sug is null || sug.Pairs.Count == 0)
        {
            HeaderText.Text = "No likely merges found. (Try after a fresh scan + re-cluster.)";
            return;
        }
        HeaderText.Text = $"{sug.Pairs.Count} candidate pair{(sug.Pairs.Count == 1 ? "" : "s")} — review each.";
        foreach (var p in sug.Pairs)
        {
            _rows.Add(new MergeSuggestionVm { Model = p });
        }
    }

    private async void OnMergeClicked(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not MergeSuggestionVm vm) return;
        try
        {
            await EngineClient.Instance.MergeClustersAsync(vm.SourcePersonId, vm.DestinationPersonId);
            vm.IsResolved = true;
            // The merged-away source person no longer exists; resolve any other
            // visible pair that references it so the user can't act on a
            // now-dangling person (which would be a no-op merge).
            foreach (var other in _rows)
            {
                if (other.SourcePersonId == vm.SourcePersonId
                    || other.DestinationPersonId == vm.SourcePersonId)
                {
                    other.IsResolved = true;
                }
            }
            StatusText.Text = $"Merged #{vm.SourcePersonId} into #{vm.DestinationPersonId}.";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Merge failed: {ex.Message}";
        }
    }

    private async void OnDifferentClicked(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not MergeSuggestionVm vm) return;
        await MarkDifferentAsync(vm);
    }

    private async Task MarkDifferentAsync(MergeSuggestionVm vm)
    {
        // Route through the engine's single-writer connection (the app must
        // not open its own DB writer). The engine persists the verdict keyed
        // on the stable anchor face ids so findMergeSuggestions keeps
        // suppressing the pair across re-clustering.
        try
        {
            await EngineClient.Instance.MarkPersonsDifferentAsync(
                vm.SourcePersonId,
                vm.DestinationPersonId,
                vm.SourceAnchorFaceId,
                vm.DestinationAnchorFaceId);
            vm.IsResolved = true;
            StatusText.Text = $"Marked #{vm.SourcePersonId} ↔ #{vm.DestinationPersonId} as different people.";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Couldn't save: {ex.Message}";
        }
    }
}
