using System.ComponentModel;
using FileID.IpcSchema;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.ViewModels;

internal sealed class FaceCropVm
{
    public required long FaceId { get; init; }
    public required string Label { get; init; }

    private BitmapImage? _image;
    private bool _resolved;
    public BitmapImage? Image => FaceReviewItemVm.LoadCrop(FaceId, ref _image, ref _resolved);

    public static IReadOnlyList<FaceCropVm> Strip(IReadOnlyList<long>? faceIds, string? personName) =>
        FaceQueuePolicy.Strip(faceIds).Select((id, index) => new FaceCropVm { FaceId = id, Label = FaceQueuePolicy.StripFaceLabel(personName, index) }).ToArray();
}

internal abstract class FaceQueueRowVm : INotifyPropertyChanged
{
    private bool _isBusy;
    public bool IsBusy
    {
        get => _isBusy;
        set
        {
            if (_isBusy == value) return;
            _isBusy = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsBusy)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ActionsEnabled)));
        }
    }

    public bool ActionsEnabled => !_isBusy;

    public event PropertyChangedEventHandler? PropertyChanged;
}

internal sealed class FaceMergeRowVm : FaceQueueRowVm
{
    public required FaceMergeSuggestion Model { get; init; }

    public string PersonName => FaceQueuePolicy.Name(Model.PersonName);
    public string OtherName => FaceQueuePolicy.Name(Model.OtherName);
    public string PersonFaces => FaceQueuePolicy.FaceCount(Model.FaceCount);
    public string OtherFaces => FaceQueuePolicy.FaceCount(Model.OtherFaceCount);
    public string RowLabel => FaceQueuePolicy.MergeRowLabel(Model);
    public string MergeLabel => FaceQueuePolicy.MergeLabel(Model);
    public string NotSameLabel => FaceQueuePolicy.NotSameLabel(Model);
    public string DismissLabel => FaceQueuePolicy.DismissMergeLabel(Model);
    public string PersonFaceLabel => $"Face of {PersonName}";
    public string OtherFaceLabel => $"Face of {OtherName}";

    private BitmapImage? _personImage;
    private bool _personResolved;
    public BitmapImage? PersonImage => FaceReviewItemVm.LoadCrop(Model.RepresentativeFaceId ?? 0, ref _personImage, ref _personResolved);

    private BitmapImage? _otherImage;
    private bool _otherResolved;
    public BitmapImage? OtherImage => FaceReviewItemVm.LoadCrop(Model.OtherRepresentativeFaceId ?? 0, ref _otherImage, ref _otherResolved);
}

internal sealed class FaceSplitRowVm : FaceQueueRowVm
{
    private IReadOnlyList<FaceCropVm>? _keep;
    private IReadOnlyList<FaceCropVm>? _move;

    public required FaceSplitSuggestion Model { get; init; }

    public string PersonName => FaceQueuePolicy.Name(Model.PersonName);
    public string KeepCaption => FaceQueuePolicy.KeepCaption(Model);
    public string MoveCaption => FaceQueuePolicy.MoveCaption(Model);
    public string RowLabel => FaceQueuePolicy.SplitRowLabel(Model);
    public string SplitLabel => FaceQueuePolicy.SplitLabel(Model);
    public string DismissLabel => FaceQueuePolicy.DismissSplitLabel(Model);
    public IReadOnlyList<FaceCropVm> KeepFaces => _keep ??= FaceCropVm.Strip(Model.KeepFaceIds, Model.PersonName);
    public IReadOnlyList<FaceCropVm> MoveFaces => _move ??= FaceCropVm.Strip(Model.SplitFaceIds, Model.PersonName);
}
